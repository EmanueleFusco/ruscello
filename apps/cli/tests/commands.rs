use std::{error::Error, net::Ipv4Addr, time::Duration};

use ruscello_testkit::{FileSpec, Reply, StatusCode, TestServer};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::Command,
    sync::mpsc,
    time::timeout,
};

type TestResult = Result<(), Box<dyn Error>>;

#[tokio::test]
async fn fresh_logs_are_quiet_and_cli_explains_missing_files_and_pages() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(17, 10_000));
    let missing = server.route("/missing.bin", |_| Reply::status(StatusCode::NOT_FOUND));
    let page = server.route("/ordinary", |_| {
        Reply::html("<html><title>Directory</title><body><p>Ordinary web page</p></body></html>")
    });
    let directory = tempfile::tempdir()?;
    let logs = directory.path().join("first/logs");
    let mut command = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"));
    let success = command
        .args(["--language", "en", "get", file.url(), "--quiet", "-o"])
        .arg(directory.path().join("output"))
        .env("RUSCELLO_LOG_DIR", &logs)
        .output()
        .await?;
    assert!(success.status.success());
    assert!(
        success.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    for (url, expected) in [(missing, "Controlla il link"), (page, "LinkGrabber")] {
        let output = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
            .args(["--language", "it", "get", &url, "--quiet", "-o"])
            .arg(directory.path().join("output"))
            .env("RUSCELLO_LOG_DIR", &logs)
            .output()
            .await?;
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            stderr.contains(expected),
            "expected {expected:?}, got {stderr:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn cli_localizes_dlc_import_failures_and_only_tries_fallback_on_request() -> TestResult {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let proxy_url = format!("http://{}", listener.local_addr()?);
    let (requests, mut seen) = mpsc::unbounded_channel();
    let proxy = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let mut socket = BufReader::new(socket);
            let mut request = String::new();
            if socket.read_line(&mut request).await.is_err() || requests.send(request).is_err() {
                return;
            }
            let _ = socket
                .get_mut()
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }
    });
    let directory = tempfile::tempdir()?;
    let malformed = directory.path().join("broken.dlc");
    tokio::fs::write(&malformed, b"not a DLC container").await?;
    let container = directory.path().join("service-unavailable.dlc");
    // The envelope has a 16-byte encrypted body and a correctly encoded 64-byte key tail.
    tokio::fs::write(
        &container,
        format!("{}=={}==", "A".repeat(22), "A".repeat(86)),
    )
    .await?;

    for (language, invalid_message, key_message) in [
        (
            "en",
            "The download container is invalid.",
            "Ruscello could not obtain the container key.",
        ),
        (
            "it",
            "Il contenitore di download non è valido.",
            "Ruscello non ha potuto ottenere la chiave del contenitore.",
        ),
    ] {
        let primary = "CONNECT service.jdownloader.org:443 HTTP/1.1";
        let fallback = "CONNECT dcrypt.it:443 HTTP/1.1";
        for (file, allow_fallback, diagnostic, expected_requests) in [
            (&malformed, true, invalid_message, &[][..]),
            (&container, false, key_message, &[primary][..]),
            (&container, true, key_message, &[primary, fallback][..]),
        ] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"));
            command.args(["--language", language, "import"]).arg(file);
            if allow_fallback {
                command.arg("--dlc-fallback");
            }
            for variable in [
                "http_proxy",
                "https_proxy",
                "all_proxy",
                "no_proxy",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "NO_PROXY",
            ] {
                command.env_remove(variable);
            }
            let output = timeout(
                Duration::from_secs(12),
                command
                    .env("HTTPS_PROXY", &proxy_url)
                    .env("RUSCELLO_LOG_DIR", directory.path().join("cli-logs"))
                    .kill_on_drop(true)
                    .output(),
            )
            .await??;
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert_eq!(String::from_utf8(output.stderr)?.trim(), diagnostic);
            let mut transmitted = Vec::new();
            while let Ok(request) = seen.try_recv() {
                transmitted.push(request.trim().to_owned());
            }
            assert_eq!(transmitted, expected_requests);
        }
    }
    proxy.abort();
    Ok(())
}

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
