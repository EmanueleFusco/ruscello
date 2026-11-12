use std::{error::Error, num::NonZeroUsize};

use ruscello_http::{DownloadOptions, HttpDownloader, Url, WriterError, persisted_bytes};
use ruscello_testkit::{FileSpec, Representation, TestServer};
use sha2::{Digest, Sha256};

type TestResult = Result<(), Box<dyn Error>>;
const MIB: u64 = 1024 * 1024;

#[tokio::test]
async fn restarts_when_if_range_fails_during_segmented_download() -> TestResult {
    let server = TestServer::start().await?;
    let changed = Representation::random(27, 4 * MIB);
    let file = server.file(
        FileSpec::random(19, 4 * MIB)
            .throttle_per_conn(3 * MIB)
            .change_after_bytes(128 * 1024, changed.clone()),
    );
    let directory = tempfile::tempdir()?;
    let options = DownloadOptions {
        directory: directory.path().to_owned(),
        connections: NonZeroUsize::MIN.saturating_add(1),
        ..DownloadOptions::default()
    };
    let path = HttpDownloader::new()?
        .download(&Url::parse(file.url())?, options)
        .await?;
    assert_eq!(
        Sha256::digest(std::fs::read(path)?),
        Sha256::digest(changed.payload().bytes(0..4 * MIB))
    );
    assert!(
        file.requests().iter().any(|record| {
            record.if_range.is_some() && record.status == reqwest::StatusCode::OK
        })
    );
    Ok(())
}

#[test]
fn rejects_a_corrupt_checkpoint() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("invalid.rscpart");
    std::fs::write(
        directory.path().join("invalid.rscpart.ctl"),
        b"RSCT\x01\x00invalid",
    )?;
    assert!(matches!(
        persisted_bytes(&path),
        Err(WriterError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData
    ));
    Ok(())
}
