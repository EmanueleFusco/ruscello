use std::{error::Error, fs, path::PathBuf, process::Stdio, time::Duration};

use ruscello_testkit::{Reply, TestServer};
use tokio::{process::Command, time::timeout};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[tokio::test]
async fn cli_inspects_and_downloads_local_hls_stream() -> TestResult {
    let server = TestServer::start().await?;
    let temp = tempfile::tempdir()?;
    let out_dir = temp.path().join("out");
    fs::create_dir_all(&out_dir)?;

    server.route("/cli/720/0.ts", |_| {
        Reply::bytes("video/mp2t", b"SEG0-".to_vec())
    });
    server.route("/cli/720/1.ts", |_| {
        Reply::bytes("video/mp2t", b"SEG1".to_vec())
    });
    server.route("/cli/720.m3u8", |_| {
        let m3u8 = "\
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:4
#EXTINF:4.0,
/cli/720/0.ts
#EXTINF:4.0,
/cli/720/1.ts
#EXT-X-ENDLIST
";
        Reply::bytes("application/vnd.apple.mpegurl", m3u8.as_bytes().to_vec())
    });
    let master_url = server.route("/cli/clip.m3u8", |_| {
        let master = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1280x720
/cli/720.m3u8
";
        Reply::bytes("application/vnd.apple.mpegurl", master.as_bytes().to_vec())
    });

    // 1) Inspect prints formats in English and Italian.
    let inspect_en = timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
            .args(["--language", "en", "inspect", &master_url])
            .env("RUSCELLO_LOG_DIR", temp.path().join("logs"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await??;
    assert!(inspect_en.status.success());
    let stdout_en = String::from_utf8_lossy(&inspect_en.stdout);
    assert!(stdout_en.contains("Formats:"));
    assert!(stdout_en.contains("720p"));

    let inspect_it = timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
            .args(["--language", "it", "inspect", &master_url])
            .env("RUSCELLO_LOG_DIR", temp.path().join("logs"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await??;
    assert!(inspect_it.status.success());
    let stdout_it = String::from_utf8_lossy(&inspect_it.stdout);
    assert!(stdout_it.contains("Formati:"));

    // 2) Download HLS stream cleanly with `--format 720p`.
    let get_out = timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
            .args([
                "--language",
                "en",
                "get",
                &master_url,
                "--format",
                "720p",
                "--quiet",
                "-o",
            ])
            .arg(&out_dir)
            .env("RUSCELLO_LOG_DIR", temp.path().join("logs"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await??;
    assert!(
        get_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&get_out.stderr)
    );
    let written_path = PathBuf::from(String::from_utf8_lossy(&get_out.stdout).trim());
    assert!(written_path.exists());
    assert_eq!(fs::read(&written_path)?, b"SEG0-SEG1");
    Ok(())
}
