use std::{
    error::Error,
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
    time::Duration,
};

use reqwest::StatusCode;
use ruscello_http::{
    DownloadOptions, DownloadProgress, ExpiredLinkResolver, HttpDownloader, ResolvedLink,
    RetryPolicy, Url,
};
use ruscello_testkit::{FileSpec, TestServer};
use tokio::{
    sync::watch,
    time::{self, Instant},
};

type TestResult = Result<(), Box<dyn Error>>;
const MIB: u64 = 1024 * 1024;

fn options(directory: &tempfile::TempDir) -> DownloadOptions {
    DownloadOptions {
        directory: directory.path().to_owned(),
        connections: NonZeroUsize::MIN,
        retry: RetryPolicy {
            initial_delay: Duration::from_millis(5),
            maximum_delay: Duration::from_millis(20),
            ..RetryPolicy::default()
        },
        ..DownloadOptions::default()
    }
}

#[tokio::test]
async fn retries_interrupted_ranges_and_intermittent_server_errors() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(
        FileSpec::random(153, 2 * MIB)
            .drop_first_responses(2, MIB / 2)
            .intermittent_error(3, StatusCode::BAD_GATEWAY),
    );
    let directory = tempfile::tempdir()?;
    let path = HttpDownloader::new()?
        .download(&Url::parse(file.url())?, options(&directory))
        .await?;
    assert_eq!(std::fs::read(path)?, file.payload().bytes(0..2 * MIB));
    let requests = file.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.status == StatusCode::BAD_GATEWAY)
    );
    assert!(requests.iter().any(|request| {
        request.if_range.is_some()
            && request
                .range
                .as_deref()
                .is_some_and(|range| !range.starts_with("bytes=0-"))
    }));
    Ok(())
}

#[tokio::test]
async fn restarts_a_dropped_chunked_download_from_zero() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(
        FileSpec::random(211, 256 * 1024)
            .ranges(false)
            .chunked()
            .drop_first_responses(2, 32 * 1024),
    );
    let directory = tempfile::tempdir()?;
    let mut settings = options(&directory);
    let (progress, view) = watch::channel(DownloadProgress::default());
    settings.progress = Some(progress);
    let path = HttpDownloader::new()?
        .download(&Url::parse(file.url())?, settings)
        .await?;
    assert_eq!(std::fs::read(path)?, file.payload().bytes(0..256 * 1024));
    let requests = file.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.range.is_none())
            .count(),
        2
    );
    assert!(requests.iter().all(|request| request.if_range.is_none()));
    assert!(!view.borrow().resumable);
    assert_eq!(view.borrow().completed, 256 * 1024);
    Ok(())
}

#[tokio::test]
async fn waits_for_connectivity_before_spending_any_attempts() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(11, 32 * 1024));
    let directory = tempfile::tempdir()?;
    let mut settings = options(&directory);
    settings.retry.max_retries = 0;
    let (progress, mut view) = watch::channel(DownloadProgress::default());
    settings.progress = Some(progress);
    let downloader = HttpDownloader::new()?;
    let connectivity = downloader.connectivity();
    connectivity.set_online(false);
    let url = Url::parse(file.url())?;
    let task = tokio::spawn(async move { downloader.download(&url, settings).await });
    time::timeout(
        Duration::from_secs(3),
        view.wait_for(|progress| progress.waiting_for_network),
    )
    .await??;
    assert!(file.requests().is_empty());
    connectivity.set_online(true);
    let path = time::timeout(Duration::from_secs(3), task).await???;
    assert_eq!(std::fs::read(path)?, file.payload().bytes(0..32 * 1024));
    Ok(())
}

#[derive(Debug)]
struct LocalResolver(Url);

impl ExpiredLinkResolver for LocalResolver {
    fn resolve<'a>(&'a self, _expired: &'a Url) -> ResolvedLink<'a> {
        Box::pin(std::future::ready(Ok(self.0.clone())))
    }
}

#[tokio::test]
async fn asks_the_resolver_to_refresh_an_expired_link() -> TestResult {
    let server = TestServer::start().await?;
    let expired = server.file(FileSpec::random(21, 128).intermittent_error(1, StatusCode::GONE));
    let fresh = server.file(FileSpec::random(29, 128));
    let directory = tempfile::tempdir()?;
    let mut settings = options(&directory);
    settings.expired_link_resolver = Some(Arc::new(LocalResolver(Url::parse(fresh.url())?)));
    let path = HttpDownloader::new()?
        .download(&Url::parse(expired.url())?, settings)
        .await?;
    assert_eq!(std::fs::read(path)?, fresh.payload().bytes(0..128));
    assert_eq!(expired.requests().len(), 1);
    Ok(())
}

#[tokio::test]
async fn enforces_runtime_bandwidth_limits_on_a_download() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(99, MIB / 2));
    let directory = tempfile::tempdir()?;
    let settings = options(&directory);
    settings.bandwidth.set_rate(NonZeroU64::new(MIB / 4));
    let downloader = HttpDownloader::new()?;
    downloader.global_bandwidth().set_rate(NonZeroU64::new(MIB));
    let started = Instant::now();
    let url = Url::parse(file.url())?;
    let path = time::timeout(Duration::from_secs(6), downloader.download(&url, settings)).await??;
    assert!(started.elapsed() >= Duration::from_millis(1750));
    assert_eq!(std::fs::read(path)?, file.payload().bytes(0..MIB / 2));
    Ok(())
}
