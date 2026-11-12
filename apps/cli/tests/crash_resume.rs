use std::{error::Error, io, path::Path, process::Stdio, time::Duration};

use ruscello_http::persisted_bytes;
use ruscello_testkit::{FileSpec, Representation, TestFile, TestServer};
use sha2::{Digest, Sha256};
use tokio::{process::Command, time};

const MIB: u64 = 1024 * 1024;
const SIZE: u64 = 20 * MIB;
type TestResult = Result<(), Box<dyn Error>>;

fn cli(file: &TestFile, directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"));
    command
        .arg("get")
        .arg(file.url())
        .arg("-o")
        .arg(directory)
        .args(["-x", "1"])
        .env("RUSCELLO_LOG_DIR", directory.join("logs"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn crash_at_seeded_checkpoint(
    file: &TestFile,
    directory: &Path,
    seed: u64,
) -> Result<u64, Box<dyn Error>> {
    let mut child = cli(file, directory).spawn()?;
    let partial = directory.join("payload.bin.rscpart");
    let threshold = 2 * MIB + seed.wrapping_mul(0x9e3779b97f4a7c15) % (7 * MIB);
    time::timeout(Duration::from_secs(30), async {
        let mut poll = time::interval(Duration::from_millis(20));
        loop {
            poll.tick().await;
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "download exited before interruption: {status}"
                )));
            }
            if let Ok(persisted) = persisted_bytes(&partial)
                && persisted >= threshold
                && persisted < SIZE
            {
                child.kill().await?;
                return persisted_bytes(&partial).map_err(io::Error::other);
            }
        }
    })
    .await?
    .map_err(Into::into)
}

async fn finish_and_verify(file: &TestFile, directory: &Path) -> TestResult {
    let output = time::timeout(Duration::from_secs(30), cli(file, directory).output()).await??;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let published = std::fs::read(directory.join("payload.bin"))?;
    let expected = file.payload().bytes(0..SIZE);
    assert_eq!(Sha256::digest(&published), Sha256::digest(&expected));
    assert!(!directory.join("payload.bin.rscpart").try_exists()?);
    assert!(!directory.join("payload.bin.rscpart.ctl").try_exists()?);
    Ok(())
}

#[tokio::test]
async fn force_kill_and_resume_from_the_persisted_range() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(37, SIZE).throttle_per_conn(4 * MIB));
    let directory = tempfile::tempdir()?;
    let persisted = crash_at_seeded_checkpoint(&file, directory.path(), 0x741e).await?;
    let requests = file.requests().len();
    finish_and_verify(&file, directory.path()).await?;
    let expected = format!("bytes={persisted}-{}", SIZE - 1);
    assert!(file.requests()[requests..].iter().any(|record| {
        record.range.as_deref() == Some(expected.as_str()) && record.if_range.is_some()
    }));
    Ok(())
}

#[tokio::test]
async fn force_kill_then_restart_if_the_remote_etag_changed() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(93, SIZE).throttle_per_conn(4 * MIB));
    let directory = tempfile::tempdir()?;
    crash_at_seeded_checkpoint(&file, directory.path(), 0x2a51).await?;
    file.replace(Representation::random(97, SIZE).etag("\"new\""));
    let requests = file.requests().len();
    finish_and_verify(&file, directory.path()).await?;
    let expected = format!("bytes=0-{}", SIZE - 1);
    assert!(file.requests()[requests..].iter().any(|record| {
        record.range.as_deref() == Some(expected.as_str())
            && record.if_range.as_deref() == Some("\"new\"")
    }));
    Ok(())
}
