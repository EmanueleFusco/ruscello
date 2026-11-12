use std::{
    error::Error,
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use ruscello_testkit::{FileSpec, Payload, TestFile, TestServer};
use sha2::{Digest, Sha256};
use tokio::process::Command;

const MIB: u64 = 1024 * 1024;
const SIZE: u64 = 256 * MIB;
const SAMPLES: usize = 5;
const LATENCY: Duration = Duration::from_millis(50);
const FILENAME: &str = "payload.bin";

/// Requires aria2c on PATH or an explicit ARIA2C executable path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual local throughput benchmark requiring aria2c"]
async fn compare_segmented_throughput_to_aria2() -> Result<(), Box<dyn Error>> {
    let server = TestServer::start().await?;
    let fixture = server.file(FileSpec::random(901, SIZE).latency(LATENCY));
    let expected = payload_hash(fixture.payload());
    let aria2 = std::env::var_os("ARIA2C").unwrap_or_else(|| "aria2c".into());
    let mut ruscello_rates = Vec::with_capacity(SAMPLES);
    let mut aria2_rates = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        for use_ruscello in [sample % 2 == 0, sample % 2 != 0] {
            let directory = tempfile::tempdir()?;
            let executable = if use_ruscello {
                env!("CARGO_BIN_EXE_ruscello-cli").into()
            } else {
                aria2.clone()
            };
            let rate = measure(
                &fixture,
                &directory,
                Path::new(&executable),
                use_ruscello,
                expected,
            )
            .await?;
            let (name, rates) = if use_ruscello {
                ("Ruscello", &mut ruscello_rates)
            } else {
                ("aria2", &mut aria2_rates)
            };
            writeln!(
                io::stdout().lock(),
                "{name} sample {}: {rate:.1} MiB/s",
                sample + 1
            )?;
            rates.push(rate);
        }
    }
    ruscello_rates.sort_by(f64::total_cmp);
    aria2_rates.sort_by(f64::total_cmp);
    let ruscello = ruscello_rates[SAMPLES / 2];
    let aria2 = aria2_rates[SAMPLES / 2];
    let percent = 100.0 * ruscello / aria2;
    writeln!(
        io::stdout().lock(),
        "Median: Ruscello {ruscello:.1} MiB/s, aria2 {aria2:.1} MiB/s ({percent:.2}%)"
    )?;
    assert!(
        percent >= 95.0,
        "Ruscello must reach 95% of aria2 throughput"
    );
    Ok(())
}

async fn measure(
    fixture: &TestFile,
    directory: &tempfile::TempDir,
    executable: &Path,
    ruscello: bool,
    expected: [u8; 32],
) -> Result<f64, Box<dyn Error>> {
    let mut command = Command::new(executable);
    command
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if ruscello {
        command
            .args(["get", fixture.url(), "-x", "16", "--quiet", "-o"])
            .arg(directory.path())
            .env("RUSCELLO_LOG_DIR", directory.path().join("logs"));
    } else {
        command
            .args([
                "-x16",
                "-s16",
                "-k1M",
                "--file-allocation=none",
                "--summary-interval=0",
                "--console-log-level=error",
                "--no-conf",
                "--max-tries=1",
                "--out",
                FILENAME,
                "--dir",
            ])
            .arg(directory.path())
            .arg(fixture.url());
    }
    let start = Instant::now();
    let output = tokio::time::timeout(Duration::from_secs(90), command.output()).await??;
    let elapsed = start.elapsed();
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{} exited with {}. {} {}",
            executable.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ))
        .into());
    }
    let path = directory.path().join(FILENAME);
    let actual = tokio::task::spawn_blocking(move || file_hash(path)).await??;
    assert_eq!(
        actual, expected,
        "benchmark transfer did not match the fixture"
    );
    Ok(SIZE as f64 / MIB as f64 / elapsed.as_secs_f64())
}

fn payload_hash(payload: Payload) -> [u8; 32] {
    let mut digest = Sha256::new();
    for start in (0..payload.len()).step_by(MIB as usize) {
        digest.update(payload.bytes(start..start + MIB));
    }
    digest.finalize().into()
}

fn file_hash(path: PathBuf) -> io::Result<[u8; 32]> {
    let mut reader = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; MIB as usize];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(digest.finalize().into());
        }
        digest.update(&buffer[..count]);
    }
}
