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

#[tokio::test]
async fn queues_and_queries_status_through_loopback_api() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(88, 16 * 1024).filename("cli-api.bin"));
    let directory = tempfile::tempdir()?;
    let mut engine_options = ruscello_engine::EngineOptions::new(
        directory.path().join("queue.db"),
        directory.path().join("downloads"),
    );
    engine_options.data_dir = Some(directory.path().to_path_buf());
    let engine = ruscello_engine::EngineHandle::start(engine_options).await?;
    let api_config = ruscello_api::ApiConfig {
        data_dir: Some(directory.path().to_path_buf()),
        local_port: 0,
        fallback_to_ephemeral_local_port: false,
        ..ruscello_api::ApiConfig::default()
    };
    let api = ruscello_api::ApiServer::start(engine.clone(), api_config).await?;

    let queued = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
        .args(["--language", "en", "queue", file.url(), "--paused"])
        .env("RUSCELLO_DATA_DIR", directory.path())
        .env("RUSCELLO_LOG_DIR", directory.path().join("logs"))
        .output()
        .await?;
    assert!(
        queued.status.success(),
        "queue stderr: {}",
        String::from_utf8_lossy(&queued.stderr)
    );
    assert!(String::from_utf8(queued.stdout)?.contains(": 1"));

    let status = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
        .args(["--language", "en", "status"])
        .env("RUSCELLO_DATA_DIR", directory.path())
        .env("RUSCELLO_LOG_DIR", directory.path().join("logs"))
        .output()
        .await?;
    assert!(
        status.status.success(),
        "status stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status_out = String::from_utf8(status.stdout)?;
    assert!(status_out.contains("downloads=1"));
    assert!(status_out.contains("cli-api.bin"));

    api.shutdown().await;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn downloads_ftp_with_credentials_and_queues_via_api() -> TestResult {
    let ftp = ruscello_testkit::MockFtpServer::start(ruscello_testkit::FtpServerOptions {
        credentials: Some(("alice".to_owned(), "secret".to_owned())),
        allow_tls: false,
        require_tls: false,
        drop_first_after_bytes: Some(2048),
        ..ruscello_testkit::FtpServerOptions::default()
    })
    .await;
    let payload: Vec<u8> = (0..(24 * 1024_usize))
        .map(|index| ((index * 31) % 251) as u8)
        .collect();
    ftp.register_file("/pub/remote.bin", payload.clone());

    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("ftp-dest");
    let get_output = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
        .args([
            "--language",
            "en",
            "get",
            &ftp.url("ftp", "/pub/remote.bin"),
            "--ftp-user",
            "alice",
            "--ftp-password",
            "secret",
            "--quiet",
            "-o",
        ])
        .arg(&destination)
        .env("RUSCELLO_LOG_DIR", directory.path().join("logs"))
        .output()
        .await?;
    assert!(
        get_output.status.success(),
        "ftp get stderr: {}",
        String::from_utf8_lossy(&get_output.stderr)
    );
    assert_eq!(std::fs::read(destination.join("remote.bin"))?, payload);
    assert!(
        ftp.observed_commands()
            .iter()
            .any(|cmd| cmd.starts_with("REST 2048")),
        "expected REST resume in {:?}",
        ftp.observed_commands()
    );

    let engine_dir = directory.path().join("engine");
    let mut engine_options = ruscello_engine::EngineOptions::new(
        engine_dir.join("queue.db"),
        engine_dir.join("downloads"),
    );
    engine_options.data_dir = Some(engine_dir.clone());
    let engine = ruscello_engine::EngineHandle::start(engine_options).await?;
    let api_config = ruscello_api::ApiConfig {
        data_dir: Some(engine_dir.clone()),
        local_port: 0,
        fallback_to_ephemeral_local_port: false,
        ..ruscello_api::ApiConfig::default()
    };
    let api = ruscello_api::ApiServer::start(engine.clone(), api_config).await?;

    let queued = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
        .args([
            "--language",
            "it",
            "queue",
            &format!("ftp://alice:secret@127.0.0.1:{}/pub/remote.bin", ftp.port()),
            "--paused",
        ])
        .env("RUSCELLO_DATA_DIR", &engine_dir)
        .env("RUSCELLO_LOG_DIR", directory.path().join("logs"))
        .output()
        .await?;
    assert!(
        queued.status.success(),
        "ftp queue stderr: {}",
        String::from_utf8_lossy(&queued.stderr)
    );
    assert!(String::from_utf8(queued.stdout)?.contains(": 1"));

    let status = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
        .args(["--language", "en", "status"])
        .env("RUSCELLO_DATA_DIR", &engine_dir)
        .env("RUSCELLO_LOG_DIR", directory.path().join("logs"))
        .output()
        .await?;
    assert!(status.status.success());
    let status_out = String::from_utf8(status.stdout)?;
    assert!(status_out.contains("remote.bin"));
    // Verify embedded password is never exposed in status or snapshot
    assert!(!status_out.contains("secret"));

    api.shutdown().await;
    engine.shutdown().await?;
    Ok(())
}
