use std::{
    error::Error,
    fmt::Write,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use reqwest::{StatusCode, header};
use ruscello_http::{
    ChecksumAlgorithm, DownloadOptions, ExpectedChecksum, HttpDownloader, HttpError,
    ScopedConnections, TransferLink, Url, WriterError, persisted_bytes,
};
use ruscello_testkit::{FileSpec, Reply, Representation, TestFile, TestServer};
use sha2::{Digest, Sha256};

const MIB: u64 = 1024 * 1024;
const SIZE: u64 = 4 * MIB;
type TestResult = Result<(), Box<dyn Error>>;

#[tokio::test]
async fn an_out_of_scope_redirect_uses_the_ordinary_per_file_limit() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(47, SIZE));
    let mut destination = Url::parse(file.url())?;
    destination.set_host(Some("localhost"))?;
    let location = header::HeaderValue::from_str(destination.as_str())?;
    let url = server.route("/scoped.bin", move |_| {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::LOCATION, location.clone());
        Reply::Response(StatusCode::FOUND, headers, String::new())
    });
    let directory = tempfile::tempdir()?;
    let mut options = settings(&directory, &file)?;
    options.connections = NonZeroUsize::MIN;
    options.scoped_connections = Some(ScopedConnections {
        hostnames: &["127.0.0.1"],
        limit: NonZeroUsize::MIN.saturating_add(3),
    });
    let path = HttpDownloader::new()?
        .download(&Url::parse(&url)?, options)
        .await?;
    assert_eq!(std::fs::read(path)?, file.payload().bytes(0..SIZE));
    let ranges: Vec<_> = file
        .requests()
        .into_iter()
        .filter(|record| record.if_range.is_some())
        .collect();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].range.as_deref(), Some("bytes=0-4194303"));
    Ok(())
}

#[tokio::test]
async fn a_scoped_transfer_respects_the_resolver_limit() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(48, SIZE));
    let url = Url::parse(file.url())?;
    let directory = tempfile::tempdir()?;
    let mut options = settings(&directory, &file)?;
    options.connections = NonZeroUsize::MIN;
    options.scoped_connections = Some(ScopedConnections {
        hostnames: &["127.0.0.1"],
        limit: NonZeroUsize::MIN.saturating_add(7),
    });
    let downloader = HttpDownloader::new()?;
    let path = downloader.download(&url, options.clone()).await?;
    assert_eq!(std::fs::read(&path)?, file.payload().bytes(0..SIZE));
    let prior = file.requests().len();
    assert!(
        file.requests()
            .iter()
            .filter(|record| record.if_range.is_some())
            .count()
            > 1
    );
    std::fs::remove_file(path)?;
    let mut resolved = TransferLink::new(url.clone());
    resolved.max_connections = Some(NonZeroUsize::MIN);
    options.resolved = Some(resolved);
    let path = downloader.download(&url, options).await?;
    assert_eq!(std::fs::read(path)?, file.payload().bytes(0..SIZE));
    assert_eq!(
        file.requests()[prior..]
            .iter()
            .filter(|record| record.if_range.is_some())
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn a_scoped_transfer_resumes_from_its_existing_checkpoint() -> TestResult {
    let server = TestServer::start().await?;
    let size = 8 * MIB;
    let file = server.file(FileSpec::random(49, size).throttle_per_conn(MIB));
    let url = Url::parse(file.url())?;
    let directory = tempfile::tempdir()?;
    let mut options = settings(&directory, &file)?;
    options.connections = NonZeroUsize::MIN;
    options.scoped_connections = Some(ScopedConnections {
        hostnames: &["127.0.0.1"],
        limit: NonZeroUsize::MIN.saturating_add(3),
    });
    let mut first = options.clone();
    first.cancellation = options.cancellation.child_token();
    let token = first.cancellation.clone();
    let (progress, mut updates) =
        tokio::sync::watch::channel(ruscello_http::DownloadProgress::default());
    first.progress = Some(progress);
    let downloader = HttpDownloader::new()?;
    let download = downloader.download(&url, first);
    tokio::pin!(download);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut download => return Err(format!("transfer ended before cancellation: {result:?}")),
                updated = updates.changed() => updated.map_err(|error| error.to_string())?,
            }
            if updates.borrow_and_update().completed >= MIB / 2 {
                token.cancel();
                return Ok(());
            }
        }
    }).await??;
    assert!(matches!(download.await, Err(HttpError::Cancelled)));
    let durable = persisted_bytes(&directory.path().join("payload.bin.rscpart"))?;
    assert!((MIB / 2..size).contains(&durable));

    let before = file.requests().len();
    let path = downloader.download(&url, options).await?;
    assert_eq!(std::fs::read(path)?, file.payload().bytes(0..size));
    let requests = file.requests();
    let resumed = requests[before..]
        .iter()
        .filter(|record| record.if_range.is_some());
    assert!(resumed.clone().count() > 0);
    assert!(
        resumed
            .filter_map(|record| record.range.as_deref())
            .all(|range| !range.starts_with("bytes=0-"))
    );
    Ok(())
}

#[tokio::test]
async fn retries_a_throttled_html_range_response_without_treating_the_link_as_expired() -> TestResult
{
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(46, SIZE));
    let fixture = file.clone();
    let blocked = Arc::new(AtomicBool::new(false));
    let was_blocked = blocked.clone();
    let url = Url::parse(&server.route("/throttled.bin", move |request| {
        if request.headers.contains_key(header::IF_RANGE) && !blocked.swap(true, Ordering::AcqRel) {
            let mut headers = header::HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/html"),
            );
            headers.insert(header::RETRY_AFTER, header::HeaderValue::from_static("0"));
            return Reply::Response(
                StatusCode::TOO_MANY_REQUESTS,
                headers,
                "<html>Retry later</html>".into(),
            );
        }
        Reply::File(fixture.clone())
    }))?;
    let directory = tempfile::tempdir()?;
    let mut options = settings(&directory, &file)?;
    options.connections = NonZeroUsize::MIN;
    options.scoped_connections = Some(ScopedConnections {
        hostnames: &["127.0.0.1"],
        limit: NonZeroUsize::MIN.saturating_add(3),
    });
    options.resolved = Some(TransferLink::new(url.clone()));
    let path = HttpDownloader::new()?.download(&url, options).await?;
    assert!(was_blocked.load(Ordering::Acquire));
    assert_eq!(std::fs::read(path)?, file.payload().bytes(0..SIZE));
    Ok(())
}

#[tokio::test]
async fn uses_a_full_get_when_the_server_ignores_a_matching_if_range() -> TestResult {
    let server = TestServer::start().await?;
    let payload = vec![17; 54_059];
    let expected = payload.clone();
    let url = server.route("/conditional.bin", move |request| {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::ETAG, header::HeaderValue::from_static("\"stable\""));
        let status = if request.headers.contains_key(header::RANGE)
            && !request.headers.contains_key(header::IF_RANGE)
        {
            headers.insert(
                header::CONTENT_RANGE,
                header::HeaderValue::from_static("bytes 0-54058/54059"),
            );
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };
        Reply::Bytes(status, headers, payload.clone())
    });
    let directory = tempfile::tempdir()?;
    let options = DownloadOptions {
        directory: directory.path().to_owned(),
        ..DownloadOptions::default()
    };
    let path = HttpDownloader::new()?
        .download(&Url::parse(&url)?, options)
        .await?;
    assert_eq!(std::fs::read(path)?, expected);
    Ok(())
}

#[tokio::test]
async fn full_stream_uses_its_own_framing_after_an_inconsistent_range_probe() -> TestResult {
    let server = TestServer::start().await?;
    let payload = vec![29; 9_519];
    let expected = payload.clone();
    let url = server.route("/domains.txt", move |request| {
        let mut headers = header::HeaderMap::new();
        let status = if request.headers.contains_key(header::RANGE) {
            headers.insert(
                header::CONTENT_RANGE,
                header::HeaderValue::from_static("bytes 0-4882/4883"),
            );
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };
        Reply::Chunked(status, headers, payload.clone())
    });
    let directory = tempfile::tempdir()?;
    let mut digest = String::new();
    for byte in Sha256::digest(&expected) {
        write!(digest, "{byte:02x}")?;
    }
    let options = DownloadOptions {
        directory: directory.path().to_owned(),
        expected_checksum: Some(ExpectedChecksum::from_hex(
            ChecksumAlgorithm::Sha256,
            &digest,
        )?),
        ..DownloadOptions::default()
    };
    let path = HttpDownloader::new()?
        .download(&Url::parse(&url)?, options)
        .await?;
    assert_eq!(std::fs::read(path)?, expected);
    Ok(())
}

fn settings(
    directory: &tempfile::TempDir,
    file: &TestFile,
) -> Result<DownloadOptions, Box<dyn Error>> {
    let payload = file.payload();
    let mut expected = String::new();
    for byte in Sha256::digest(payload.bytes(0..payload.len())) {
        write!(expected, "{byte:02x}")?;
    }
    Ok(DownloadOptions {
        directory: directory.path().to_owned(),
        connections: NonZeroUsize::MIN.saturating_add(3),
        expected_checksum: Some(ExpectedChecksum::from_hex(
            ChecksumAlgorithm::Sha256,
            &expected,
        )?),
        ..DownloadOptions::default()
    })
}

#[tokio::test]
async fn distributes_ranges_across_matching_mirrors_and_skips_mismatched_sizes() -> TestResult {
    let server = TestServer::start().await?;
    let primary = server.file(FileSpec::random(19, SIZE));
    let mirror = server.file(
        FileSpec::random(0, 0).representation(Representation::random(19, SIZE).etag("\"mirror\"")),
    );
    let different_size = server.file(FileSpec::random(23, SIZE / 2));
    let directory = tempfile::tempdir()?;
    let mut options = settings(&directory, &primary)?;
    options.mirrors = vec![Url::parse(mirror.url())?, Url::parse(different_size.url())?];
    let result = HttpDownloader::new()?
        .download(&Url::parse(primary.url())?, options)
        .await?;
    assert_eq!(std::fs::read(result)?, primary.payload().bytes(0..SIZE));
    assert!(
        primary
            .requests()
            .iter()
            .any(|request| request.if_range.is_some())
    );
    assert!(
        mirror
            .requests()
            .iter()
            .any(|request| request.if_range.as_deref() == Some("\"mirror\""))
    );
    assert!(
        different_size
            .requests()
            .iter()
            .all(|request| request.if_range.is_none())
    );
    Ok(())
}

#[tokio::test]
async fn falls_back_to_the_primary_when_a_mirror_fails() -> TestResult {
    let server = TestServer::start().await?;
    let primary = server.file(FileSpec::random(29, SIZE));
    let mirror = server
        .file(FileSpec::random(29, SIZE).intermittent_error(2, StatusCode::SERVICE_UNAVAILABLE));
    let directory = tempfile::tempdir()?;
    let mut options = settings(&directory, &primary)?;
    options.mirrors = vec![Url::parse(mirror.url())?];
    let result = HttpDownloader::new()?
        .download(&Url::parse(primary.url())?, options)
        .await?;
    assert_eq!(std::fs::read(result)?, primary.payload().bytes(0..SIZE));
    assert!(
        mirror
            .requests()
            .iter()
            .any(|request| request.status == StatusCode::SERVICE_UNAVAILABLE)
    );
    Ok(())
}

#[tokio::test]
async fn rejects_same_length_corruption_before_publication_and_discards_resume_ranges() -> TestResult
{
    let server = TestServer::start().await?;
    let primary = server.file(FileSpec::random(19, SIZE));
    let bad_mirror = server.file(FileSpec::random(20, SIZE));
    let directory = tempfile::tempdir()?;
    let clean = settings(&directory, &primary)?;
    let mut with_mirror = clean.clone();
    with_mirror.mirrors = vec![Url::parse(bad_mirror.url())?];
    let downloader = HttpDownloader::new()?;
    let url = Url::parse(primary.url())?;
    assert!(matches!(
        downloader.download(&url, with_mirror).await,
        Err(HttpError::Storage(WriterError::ChecksumMismatch))
    ));
    assert!(!directory.path().join("payload.bin").try_exists()?);
    assert_eq!(
        persisted_bytes(&directory.path().join("payload.bin.rscpart"))?,
        0
    );
    let path = downloader.download(&url, clean).await?;
    assert_eq!(std::fs::read(path)?, primary.payload().bytes(0..SIZE));
    Ok(())
}

#[tokio::test]
async fn requires_a_checksum_before_contacting_any_mirror() -> TestResult {
    let server = TestServer::start().await?;
    let primary = server.file(FileSpec::random(19, SIZE));
    let directory = tempfile::tempdir()?;
    let url = Url::parse(primary.url())?;
    let options = DownloadOptions {
        directory: directory.path().to_owned(),
        mirrors: vec![url.clone()],
        ..DownloadOptions::default()
    };
    assert!(matches!(
        HttpDownloader::new()?.download(&url, options).await,
        Err(HttpError::MirrorChecksumRequired)
    ));
    assert!(primary.requests().is_empty());
    Ok(())
}
