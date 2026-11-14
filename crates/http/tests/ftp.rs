use ruscello_http::{
    CheckpointJob, ChecksumAlgorithm, DownloadOptions, ErrorClass, ExpectedChecksum,
    FtpCredentials, HttpDownloader, HttpError, RetryPolicy, Url, sanitize_ftp_url,
    scan_checkpoints,
};
use ruscello_testkit::{FtpServerOptions, MockFtpServer};
use sha2::{Digest, Sha256};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn sha256_checksum(data: &[u8]) -> Result<ExpectedChecksum, ruscello_http::ChecksumError> {
    let hex = Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    ExpectedChecksum::from_hex(ChecksumAlgorithm::Sha256, &hex)
}

#[tokio::test]
async fn downloads_over_plain_ftp_and_verifies_checksum() -> TestResult {
    let server = MockFtpServer::start(FtpServerOptions::default()).await;
    let payload: Vec<u8> = (0..131_072u32).map(|i| (i % 251) as u8).collect();
    server.register_file("/pub/releases/archive.bin", payload.clone());

    let dir = tempfile::tempdir()?;
    let downloader = HttpDownloader::new()?;
    let url = Url::parse(&server.url("ftp", "/pub/releases/archive.bin"))?;
    let path = downloader
        .download(
            &url,
            DownloadOptions {
                directory: dir.path().to_path_buf(),
                expected_checksum: Some(sha256_checksum(&payload)?),
                ..DownloadOptions::default()
            },
        )
        .await?;

    assert_eq!(path, dir.path().join("archive.bin"));
    assert_eq!(std::fs::read(&path)?, payload);
    assert!(scan_checkpoints(dir.path())?.is_empty());
    Ok(())
}

#[tokio::test]
async fn resumes_ftp_transfer_with_rest_after_mid_stream_drop() -> TestResult {
    let server = MockFtpServer::start(FtpServerOptions {
        drop_first_after_bytes: Some(24_576),
        ..FtpServerOptions::default()
    })
    .await;
    let payload: Vec<u8> = (0..98_304u32).map(|i| ((i * 7) % 255) as u8).collect();
    server.register_file("/bundle.tar.gz", payload.clone());

    let dir = tempfile::tempdir()?;
    let downloader = HttpDownloader::new()?;
    let url = Url::parse(&server.url("ftp", "/bundle.tar.gz"))?;
    let path = downloader
        .download(
            &url,
            DownloadOptions {
                directory: dir.path().to_path_buf(),
                retry: RetryPolicy {
                    max_retries: 3,
                    initial_delay: std::time::Duration::from_millis(10),
                    maximum_delay: std::time::Duration::from_millis(50),
                },
                expected_checksum: Some(sha256_checksum(&payload)?),
                ..DownloadOptions::default()
            },
        )
        .await?;

    assert_eq!(std::fs::read(&path)?, payload);
    let commands = server.observed_commands();
    assert!(
        commands.iter().any(|cmd| cmd == "REST 24576"),
        "expected REST 24576 in server commands: {commands:?}"
    );
    Ok(())
}

#[tokio::test]
async fn downloads_over_explicit_ftps_with_rest_resume_and_credentials() -> TestResult {
    let server = MockFtpServer::start(FtpServerOptions {
        credentials: Some(("alice".into(), "top-secret-pw".into())),
        allow_tls: true,
        require_tls: true,
        drop_first_after_bytes: Some(16_384),
        allow_restart: true,
        support_size: true,
    })
    .await;
    let payload: Vec<u8> = (0..65_536u32).map(|i| ((i * 13) % 253) as u8).collect();
    server.register_file("/secure/data.zip", payload.clone());

    let dir = tempfile::tempdir()?;
    let downloader = HttpDownloader::new()?;
    let url = Url::parse(&server.url("ftps", "/secure/data.zip"))?;
    let path = downloader
        .download(
            &url,
            DownloadOptions {
                directory: dir.path().to_path_buf(),
                retry: RetryPolicy {
                    max_retries: 3,
                    initial_delay: std::time::Duration::from_millis(10),
                    maximum_delay: std::time::Duration::from_millis(50),
                },
                ftp_credentials: Some(FtpCredentials {
                    username: "alice".into(),
                    password: "top-secret-pw".into(),
                }),
                ftp_root_certificates: vec![server.ca_cert_der().to_vec()],
                expected_checksum: Some(sha256_checksum(&payload)?),
                ..DownloadOptions::default()
            },
        )
        .await?;

    assert_eq!(std::fs::read(&path)?, payload);
    let commands = server.observed_commands();
    assert!(commands.iter().any(|cmd| cmd == "AUTH TLS"));
    assert!(commands.iter().any(|cmd| cmd == "PROT P"));
    assert!(commands.iter().any(|cmd| cmd == "REST 16384"));
    Ok(())
}

#[tokio::test]
async fn ftp_authentication_failure_is_not_retried() -> TestResult {
    let server = MockFtpServer::start(FtpServerOptions {
        credentials: Some(("alice".into(), "right".into())),
        ..FtpServerOptions::default()
    })
    .await;
    server.register_file("/secret.bin", vec![1, 2, 3, 4]);

    let dir = tempfile::tempdir()?;
    let downloader = HttpDownloader::new()?;
    let url = Url::parse(&format!(
        "ftp://alice:wrong@127.0.0.1:{}/secret.bin",
        server.port()
    ))?;
    let err = match downloader
        .download(
            &url,
            DownloadOptions {
                directory: dir.path().to_path_buf(),
                ..DownloadOptions::default()
            },
        )
        .await
    {
        Ok(path) => return Err(format!("expected auth error, got {}", path.display()).into()),
        Err(err) => err,
    };

    assert!(matches!(err, HttpError::FtpAuthentication));
    assert_eq!(err.classification(), ErrorClass::Authentication);
    Ok(())
}

#[tokio::test]
async fn restarts_from_zero_when_ftp_server_refuses_rest() -> TestResult {
    let server = MockFtpServer::start(FtpServerOptions {
        drop_first_after_bytes: Some(8_192),
        allow_restart: false,
        ..FtpServerOptions::default()
    })
    .await;
    let payload: Vec<u8> = (0..32_768u32).map(|i| (i % 199) as u8).collect();
    server.register_file("/fallback.bin", payload.clone());

    let dir = tempfile::tempdir()?;
    let downloader = HttpDownloader::new()?;
    let url = Url::parse(&server.url("ftp", "/fallback.bin"))?;
    let path = downloader
        .download(
            &url,
            DownloadOptions {
                directory: dir.path().to_path_buf(),
                retry: RetryPolicy {
                    max_retries: 3,
                    initial_delay: std::time::Duration::from_millis(10),
                    maximum_delay: std::time::Duration::from_millis(50),
                },
                expected_checksum: Some(sha256_checksum(&payload)?),
                ..DownloadOptions::default()
            },
        )
        .await?;

    assert_eq!(std::fs::read(&path)?, payload);
    Ok(())
}

#[tokio::test]
async fn ftp_checkpoints_strip_passwords_before_writing_control_file() -> TestResult {
    let server = MockFtpServer::start(FtpServerOptions {
        credentials: Some(("alice".into(), "super-secret-password".into())),
        drop_first_after_bytes: Some(8_192),
        ..FtpServerOptions::default()
    })
    .await;
    let payload: Vec<u8> = vec![42u8; 32_768];
    server.register_file("/private/blob.bin", payload.clone());

    let dir = tempfile::tempdir()?;
    let downloader = HttpDownloader::new()?;
    let raw_url = Url::parse(&format!(
        "ftp://alice:super-secret-password@127.0.0.1:{}/private/blob.bin",
        server.port()
    ))?;
    assert_eq!(
        sanitize_ftp_url(&raw_url).as_str(),
        format!("ftp://alice@127.0.0.1:{}/private/blob.bin", server.port())
    );

    let err = downloader
        .download(
            &raw_url,
            DownloadOptions {
                directory: dir.path().to_path_buf(),
                retry: RetryPolicy {
                    max_retries: 0,
                    ..RetryPolicy::default()
                },
                checkpoint_job: Some(CheckpointJob {
                    id: 99,
                    destination: dir.path().to_path_buf(),
                }),
                ..DownloadOptions::default()
            },
        )
        .await;
    assert!(err.is_err());

    let ctl_path = dir.path().join("blob.bin.rscpart.ctl");
    let ctl_bytes = std::fs::read(&ctl_path)?;
    assert!(
        !ctl_bytes
            .windows(b"super-secret-password".len())
            .any(|w| w == b"super-secret-password"),
        "password must never appear in control file"
    );
    let checkpoints = scan_checkpoints(dir.path())?;
    assert_eq!(checkpoints.len(), 1);
    assert!(checkpoints[0].matches_url(&sanitize_ftp_url(&raw_url)));
    assert!(checkpoints[0].recovery_url().is_none());

    // Anonymous FTP checkpoints keep their non-secret recovery URL.
    let server_anon = MockFtpServer::start(FtpServerOptions {
        drop_first_after_bytes: Some(4_096),
        ..FtpServerOptions::default()
    })
    .await;
    server_anon.register_file("/anon.bin", payload);
    let dir_anon = tempfile::tempdir()?;
    let anon_url = Url::parse(&server_anon.url("ftp", "/anon.bin"))?;
    let anon_err = downloader
        .download(
            &anon_url,
            DownloadOptions {
                directory: dir_anon.path().to_path_buf(),
                retry: RetryPolicy {
                    max_retries: 0,
                    ..RetryPolicy::default()
                },
                checkpoint_job: Some(CheckpointJob {
                    id: 100,
                    destination: dir_anon.path().to_path_buf(),
                }),
                ..DownloadOptions::default()
            },
        )
        .await;
    assert!(anon_err.is_err());
    let anon_checkpoints = scan_checkpoints(dir_anon.path())?;
    assert_eq!(anon_checkpoints.len(), 1);
    assert_eq!(
        anon_checkpoints[0].recovery_url().map(Url::as_str),
        Some(anon_url.as_str())
    );
    Ok(())
}
