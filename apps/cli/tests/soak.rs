use std::{
    env, fs,
    io::{self, Read, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};

use ruscello_core::{Source, Status};
use ruscello_engine::{AddDownload, EngineHandle, EngineOptions};
use ruscello_testkit::{FileSpec, TestServer};
use sha2::{Digest, Sha256};
use tokio::{process::Command, time};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Batch {
    urls: Vec<String>,
    generation: u64,
}

fn options(root: &Path) -> EngineOptions {
    let mut options = EngineOptions::new(root.join("queue.db"), root.join("files"));
    options.limits.concurrent_downloads = std::num::NonZeroUsize::MIN.saturating_add(9);
    options
}

#[tokio::test]
#[ignore = "child process of mixed_download_soak"]
async fn soak_worker() -> anyhow::Result<()> {
    let Some(root) = env::var_os("RUSCELLO_SOAK_WORKDIR") else {
        return Ok(());
    };
    let root = Path::new(&root);
    let batch: Batch = serde_json::from_slice(&fs::read(root.join("batch.json"))?)?;
    let engine = EngineHandle::start(options(root)).await?;
    let present = engine.snapshot().await?.downloads;
    let requests = batch
        .urls
        .into_iter()
        .enumerate()
        .filter(|(index, url)| {
            let filename = format!("{}-{index}.bin", batch.generation);
            !present.iter().any(|download| {
                download.source
                    == Source::Http {
                        url: (*url).clone(),
                    }
                    && download.filename.as_deref() == Some(filename.as_str())
            })
        })
        .map(|(index, url)| {
            let mut request = AddDownload::new(Source::Http { url });
            let filename = format!("{}-{index}.bin", batch.generation);
            request.output_filename = Some(filename.clone());
            request.filename_hint = Some(filename);
            request
        })
        .collect::<Vec<_>>();
    engine.add_many(requests).await?;
    fs::write(root.join("ready"), b"queued")?;
    let mut tick = time::interval(Duration::from_millis(200));
    loop {
        tick.tick().await;
        let snapshot = engine.snapshot().await?;
        anyhow::ensure!(
            snapshot
                .downloads
                .iter()
                .all(|download| !matches!(download.status(), Status::Failed { .. })),
            "A soak download failed"
        );
        if snapshot
            .downloads
            .iter()
            .all(|download| download.status() == Status::Completed)
        {
            break;
        }
    }
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long-running offline resilience check"]
async fn mixed_download_soak() -> anyhow::Result<()> {
    let duration = Duration::from_secs(
        env::var("RUSCELLO_SOAK_SECONDS").map_or(Ok(24 * 60 * 60), |value| value.parse::<u64>())?,
    );
    let count =
        env::var("RUSCELLO_SOAK_DOWNLOADS").map_or(Ok(200), |value| value.parse::<u64>())?;
    anyhow::ensure!(count >= 10, "At least 10 downloads are required");
    let started = time::Instant::now();
    let root = tempfile::tempdir()?;
    let mut rss = RssBudget::default();
    let mut generation = 0;
    let mut seed = 0x9e3779b97f4a7c15_u64;
    let mut interrupted = 0;
    let mut verified = 0;
    while generation == 0 || started.elapsed() < duration {
        let server = TestServer::start().await?;
        let mut files = Vec::new();
        for index in 0..count {
            let random = next(&mut seed);
            let size = match index % 4 {
                0 => 2 * MIB + random % (64 * KIB),
                _ => 80 * KIB + random % (300 * KIB),
            };
            let mut spec = FileSpec::random(random, size)
                .filename(format!("{generation}-{index}.bin"))
                .ranges(index % 4 != 1)
                .redirects((index % 3) as u8)
                .latency(Duration::from_millis(random % 15));
            if index % 4 == 0 {
                spec = spec.throttle_per_conn(384 * KIB);
            }
            if index % 3 == 0 {
                spec = spec.drop_first_responses(1 + random % 2, 32 * KIB + random % (32 * KIB));
            }
            files.push(server.file(spec));
        }
        let batch = Batch {
            urls: files.iter().map(|file| file.url().to_owned()).collect(),
            generation,
        };
        let work = tempfile::tempdir_in(root.path())?;
        fs::write(work.path().join("batch.json"), serde_json::to_vec(&batch)?)?;
        let kills = 2 + next(&mut seed) % 4;
        for run in 0..=kills {
            let ready = work.path().join("ready");
            if ready.exists() {
                fs::remove_file(&ready)?;
            }
            let mut child = Command::new(env::current_exe()?)
                .args(["--exact", "soak_worker", "--ignored", "--nocapture"])
                .env("RUSCELLO_SOAK_WORKDIR", work.path())
                .stdout(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()?;
            let mut tick = time::interval(Duration::from_millis(100));
            let stop_after = Duration::from_millis(500 + next(&mut seed) % 2000);
            let ready_at = time::timeout(Duration::from_secs(60), async {
                loop {
                    tick.tick().await;
                    if ready.is_file() {
                        break Ok::<_, anyhow::Error>(time::Instant::now());
                    }
                    anyhow::ensure!(child.try_wait()?.is_none(), "Worker exited before queuing");
                }
            })
            .await??;
            let mut peak = 0;
            time::timeout(Duration::from_secs(300), async {
                loop {
                    tick.tick().await;
                    if let Some(id) = child.id() {
                        peak = peak.max(process_rss(id).unwrap_or(0));
                    }
                    if let Some(status) = child.try_wait()? {
                        anyhow::ensure!(status.success(), "Worker exited: {status}");
                        break;
                    }
                    if run < kills && ready_at.elapsed() >= stop_after {
                        child.kill().await?;
                        interrupted += 1;
                        break;
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
            .await??;
            rss.record(peak)?;
        }
        let engine = EngineHandle::start(options(work.path())).await?;
        let downloads = engine.snapshot().await?.downloads;
        engine.shutdown().await?;
        for (index, file) in files.iter().enumerate() {
            let name = format!("{generation}-{index}.bin");
            let mut matches = downloads
                .iter()
                .filter(|d| d.filename.as_deref() == Some(&name));
            let entry = matches
                .next()
                .ok_or_else(|| anyhow::anyhow!("Missing {name}"))?;
            anyhow::ensure!(matches.next().is_none(), "Duplicate {name}");
            let path = entry
                .completed_path
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Unpublished {name} (ID {:?})", entry.id))?;
            let mut actual = fs::File::open(path)?;
            let payload = file.payload();
            let mut got = Sha256::new();
            let mut expected = Sha256::new();
            let mut buffer = vec![0; 64 * 1024];
            for start in (0..payload.len()).step_by(buffer.len()) {
                let bytes = payload.bytes(start..start + buffer.len() as u64);
                actual.read_exact(&mut buffer[..bytes.len()])?;
                got.update(&buffer[..bytes.len()]);
                expected.update(bytes);
            }
            assert_eq!(actual.read(&mut buffer[..1])?, 0);
            assert_eq!(got.finalize(), expected.finalize(), "{name}");
            verified += 1;
        }
        anyhow::ensure!(downloads.len() as u64 == count, "Unexpected queue size");
        generation += 1;
        writeln!(
            io::stdout().lock(),
            "Soak: {verified} checksums, {interrupted} process kills, {} s",
            started.elapsed().as_secs()
        )?;
    }
    anyhow::ensure!(interrupted > 0, "The soak did not interrupt any worker");
    Ok(())
}

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[derive(Default)]
struct RssBudget {
    runs: usize,
    baseline: u64,
}

impl RssBudget {
    fn record(&mut self, peak: u64) -> anyhow::Result<()> {
        if peak == 0 {
            return Ok(());
        }
        self.runs += 1;
        if self.runs <= 5 {
            self.baseline = self.baseline.max(peak);
        }
        anyhow::ensure!(
            peak < 200 * MIB && peak <= self.baseline + 64 * MIB,
            "Soak worker RSS grew beyond its bound (baseline {}, peak {peak})",
            self.baseline
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn process_rss(id: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{id}/status")).ok()?;
    let rss = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?;
    rss.split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
        .map(|kib| kib * KIB)
}

#[cfg(not(target_os = "linux"))]
fn process_rss(_: u32) -> Option<u64> {
    None
}
