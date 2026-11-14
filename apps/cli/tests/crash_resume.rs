use std::{error::Error, io, path::Path, process::Stdio, time::Duration};

use ruscello_http::persisted_bytes;
use ruscello_testkit::{FileSpec, Representation, TestFile, TestServer};
use sha2::{Digest, Sha256};
use tokio::{process::Command, time};

const MIB: u64 = 1024 * 1024;
const SIZE: u64 = 20 * MIB;
type TestResult = Result<(), Box<dyn Error>>;

fn cli(file: &TestFile, directory: &Path) -> Command {
    cli_url(file.url(), directory)
}

fn cli_url(url: &str, directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"));
    command
        .arg("get")
        .arg(url)
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
    crash_at_checkpoint(
        cli(file, directory),
        &directory.join("payload.bin.rscpart"),
        SIZE,
        2 * MIB + seed.wrapping_mul(0x9e3779b97f4a7c15) % (7 * MIB),
    )
    .await
}

async fn crash_at_checkpoint(
    mut command: Command,
    partial: &Path,
    size: u64,
    threshold: u64,
) -> Result<u64, Box<dyn Error>> {
    let mut child = command.spawn()?;
    time::timeout(Duration::from_secs(30), async {
        let mut poll = time::interval(Duration::from_millis(20));
        loop {
            poll.tick().await;
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "download exited before interruption: {status}"
                )));
            }
            if let Ok(persisted) = persisted_bytes(partial)
                && persisted >= threshold
                && persisted < size
            {
                child.kill().await?;
                return persisted_bytes(partial).map_err(io::Error::other);
            }
        }
    })
    .await?
    .map_err(Into::into)
}

#[tokio::test]
async fn batch_of_twenty_hoster_pages_survives_a_process_kill_and_a_connection_drop() -> TestResult
{
    use ruscello_testkit::{Reply, StatusCode};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let server = TestServer::start().await?;
    let temp = tempfile::tempdir()?;
    let recipes = temp.path().join("recipes");
    std::fs::create_dir_all(&recipes)?;
    std::fs::write(
        recipes.join("fixture.toml"),
        r#"
        [recipe]
        name = "Share fixture"
        match = ['^http://127\.0\.0\.1:[0-9]+/share/']
        max_connections = 1
        [[step]]
        get = ""
        [[step]]
        extract = { regex = 'window\.open\("(/dl/[^"]+)"\)' }
    "#,
    )?;
    let mut pages = Vec::new();
    let mut files = Vec::new();
    let mut replacement = None;
    for index in 0..20 {
        let filename = format!("file-{index:02}.bin");
        let size = if index == 6 { 4 * MIB } else { 64 * 1024 };
        let spec = FileSpec::random(index, size)
            .filename(filename.clone())
            .content_disposition(format!("attachment; filename=\"{filename}\""));
        let file = server.file(if index == 6 {
            spec.clone().throttle_per_conn(MIB)
        } else if index == 15 {
            spec.clone().drop_first_responses(2, 8 * 1024)
        } else {
            spec.clone()
        });
        let route = format!("/dl/{index}");
        let stale = Arc::new(AtomicBool::new(false));
        server.route(&route, {
            let file = file.clone();
            let stale = stale.clone();
            move |_| {
                if stale.load(Ordering::Relaxed) {
                    Reply::status(StatusCode::GONE)
                } else {
                    Reply::File(file.clone())
                }
            }
        });
        let fresh_route = format!("/dl/{index}-renewed");
        if index == 6 {
            let fresh = server.file(spec);
            server.route(&fresh_route, {
                let fresh = fresh.clone();
                move |_| Reply::File(fresh.clone())
            });
            replacement = Some((stale.clone(), fresh));
        }
        pages.push(server.route(&format!("/share/{index}"), move |_| {
            let direct = if stale.load(Ordering::Relaxed) { &fresh_route } else { &route };
            Reply::html(format!(r#"<script>window.open("https://ads.invalid/popup");window.open("{direct}")</script>"#))
        }));
        files.push((filename, file, size));
    }
    let list = temp.path().join("links.txt");
    std::fs::write(&list, pages.join("\n"))?;
    let imported = Command::new(env!("CARGO_BIN_EXE_ruscello-cli"))
        .arg("import")
        .arg(&list)
        .env("RUSCELLO_LOG_DIR", temp.path().join("logs"))
        .output()
        .await?;
    assert!(imported.status.success());
    let output = String::from_utf8(imported.stdout)?;
    let imported: Vec<_> = output.lines().collect();
    assert_eq!(imported.len(), 20);
    let mut persisted = None;
    for (index, (url, (name, file, size))) in imported.into_iter().zip(files.iter()).enumerate() {
        let command = || {
            let mut command = cli_url(url, temp.path());
            command.env("RUSCELLO_RECIPES_DIR", &recipes);
            command
        };
        if index == 6 {
            persisted = Some(
                crash_at_checkpoint(
                    command(),
                    &temp.path().join(format!("{name}.rscpart")),
                    *size,
                    MIB,
                )
                .await?,
            );
            if let Some((stale, _)) = &replacement {
                stale.store(true, Ordering::Relaxed);
            }
        }
        let finished = time::timeout(Duration::from_secs(30), command().output()).await??;
        assert!(
            finished.status.success(),
            "{}",
            String::from_utf8_lossy(&finished.stderr)
        );
        assert_eq!(
            Sha256::digest(std::fs::read(temp.path().join(name))?),
            Sha256::digest(file.payload().bytes(0..*size))
        );
    }
    let (_, fresh) = replacement.ok_or("missing renewed link")?;
    let expected = format!(
        "bytes={}-{}",
        persisted.ok_or("missing checkpoint")?,
        4 * MIB - 1
    );
    assert!(
        fresh.requests().iter().any(
            |request| request.range.as_deref() == Some(&expected) && request.if_range.is_some()
        )
    );
    Ok(())
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
