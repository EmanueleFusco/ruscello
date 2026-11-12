use std::{error::Error, fmt::Write, num::NonZeroUsize};

use reqwest::StatusCode;
use ruscello_http::{
    ChecksumAlgorithm, DownloadOptions, ExpectedChecksum, HttpDownloader, HttpError, Url,
    WriterError, persisted_bytes,
};
use ruscello_testkit::{FileSpec, Representation, TestFile, TestServer};
use sha2::{Digest, Sha256};

const MIB: u64 = 1024 * 1024;
const SIZE: u64 = 4 * MIB;
type TestResult = Result<(), Box<dyn Error>>;

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
