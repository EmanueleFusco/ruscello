//! Runs the same binary as either the test driver or a small JSON-lines child.

use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process,
    time::Duration,
};

use ruscello_browser::{
    BrowserClient, BrowserOptions,
    protocol::{Failure, MAX_IMPORT_COOKIES, Request, Response, Session, Url, View},
};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const DEADLINE: Duration = Duration::from_secs(20);

fn main() -> TestResult {
    let mut args = std::env::args_os().skip(1).peekable();
    if args.next_if(|arg| arg == "--profile").is_some() {
        let profile = PathBuf::from(args.next().ok_or("missing fixture profile")?);
        return child(&profile);
    }
    // Expose the test to the workspace's nextest runner as a libtest-style entry.
    let args: Vec<_> = args.collect();
    if args.iter().any(|arg| arg == "--list") {
        if !args.iter().any(|arg| arg == "--ignored") {
            writeln!(io::stdout().lock(), "browser_protocol: test")?;
        }
        return Ok(());
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            timeout(DEADLINE, imports_across_protocol_batches()).await??;
            timeout(DEADLINE, observes_idle_exit_and_recovers_from_crash()).await??;
            Ok(())
        })
}

fn child(profile: &Path) -> TestResult {
    // Each launch writes one marker so the parent can distinguish process instances.
    let marker = OpenOptions::new()
        .create(true)
        .append(true)
        .open(profile.join("launches"))?;
    writeln!(&marker, "{}", process::id())?;
    let mut output = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let response = match serde_json::from_str::<Request>(&line?)? {
            Request::Open { url, .. } if url.path() == "/crash" => process::exit(73),
            Request::Open { .. } => Response::Session {
                session: Session {
                    user_agent: process::id().to_string(),
                    cookies: Vec::new(),
                },
            },
            Request::Import { cookies, .. } if cookies.len() <= MAX_IMPORT_COOKIES => {
                let mut batches = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(profile.join("batches"))?;
                writeln!(batches, "{}", cookies.len())?;
                Response::Imported {
                    count: cookies.len(),
                }
            }
            Request::Import { .. } => Response::Error {
                kind: Failure::InvalidRequest,
            },
            Request::Shutdown => {
                let mut stopped = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(profile.join("stopped"))?;
                writeln!(stopped, "{}", process::id())?;
                return Ok(());
            }
        };
        serde_json::to_writer(&mut output, &response)?;
        writeln!(output)?;
        output.flush()?;
    }
    Ok(())
}

fn client(profile: &Path, idle_timeout: Duration) -> TestResult<BrowserClient> {
    Ok(BrowserClient::new(BrowserOptions {
        executable: std::env::current_exe()?,
        profile: profile.into(),
        idle_timeout,
    }))
}

async fn imports_across_protocol_batches() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let client = client(tmp.path(), DEADLINE)?;
    let url = Url::parse("https://example.test")?;
    let cookies = (0..=MAX_IMPORT_COOKIES)
        .map(|index| format!("fixture-{index}=value; Path=/; HttpOnly"))
        .collect();
    let count = client
        .import_cookies(&url, cookies, &CancellationToken::new())
        .await?;
    assert_eq!(count, MAX_IMPORT_COOKIES + 1);
    assert_eq!(
        fs::read_to_string(tmp.path().join("batches"))?,
        format!("{MAX_IMPORT_COOKIES}\n1\n")
    );
    client.stop().await;
    Ok(())
}

async fn observes_idle_exit_and_recovers_from_crash() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let client = client(tmp.path(), Duration::from_millis(500))?;
    let cancel = CancellationToken::new();
    let open = |path: &str| {
        Ok::<_, url::ParseError>(Request::Open {
            url: Url::parse("https://example.test")?.join(path)?,
            view: View::Session,
            steps: Vec::new(),
        })
    };
    let launches = tmp.path().join("launches");
    assert!(!launches.exists(), "helper must start on demand");
    let Response::Session { session: first } = client.request(open("/")?, &cancel).await? else {
        return Err("expected fixture session".into());
    };
    let stopped = tmp.path().join("stopped");
    loop {
        match fs::read_to_string(&stopped) {
            Ok(contents) if contents.lines().any(|pid| pid == first.user_agent) => break,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert!(matches!(
        client.request(open("/crash")?, &cancel).await,
        Err(Failure::Unavailable)
    ));
    let Response::Session { session: recovered } = client.request(open("/")?, &cancel).await?
    else {
        return Err("expected a session from the replacement helper".into());
    };
    let instances = fs::read_to_string(launches)?;
    let pids: Vec<_> = instances.lines().collect();
    assert_eq!(pids.len(), 3);
    assert_eq!(pids[0], first.user_agent);
    assert_eq!(pids[2], recovered.user_agent);
    assert_ne!(pids[0], pids[1]);
    assert_ne!(pids[1], pids[2]);
    client.stop().await;
    Ok(())
}
