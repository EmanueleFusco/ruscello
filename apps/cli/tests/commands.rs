use std::error::Error;

use ruscello_testkit::{FileSpec, TestServer};
use tokio::process::Command;

type TestResult = Result<(), Box<dyn Error>>;

#[tokio::test]
async fn help_and_argument_errors_use_the_selected_language() -> TestResult {
    let help = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
        .args(["--language", "it", "--help"])
        .output()
        .await?;
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout)?.contains("Uso: ruscello-cli"));
    let error = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
        .args(["--language", "en", "get", "http://127.0.0.1/", "-x", "0"])
        .output()
        .await?;
    assert_eq!(error.status.code(), Some(2));
    assert!(String::from_utf8(error.stderr)?.contains("Usage: ruscello-cli"));
    Ok(())
}

#[tokio::test]
async fn downloads_to_a_new_directory_and_reports_non_resumable_progress() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(77, 64 * 1024).ranges(false).chunked());
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("destination");
    let output = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
        .args([
            "--language",
            "it",
            "get",
            file.url(),
            "-x",
            "2",
            "--progress",
            "-o",
        ])
        .arg(&destination)
        .env("RUSCELLO_LOG_DIR", directory.path().join("logs"))
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stderr)?;
    assert!(text.contains("Non riprendibile"));
    assert!(text.contains("100%"));
    let path = String::from_utf8(output.stdout)?;
    assert!(path.trim().ends_with("payload.bin"));
    assert_eq!(
        std::fs::read(destination.join("payload.bin"))?,
        file.payload().bytes(0..64 * 1024)
    );
    Ok(())
}
