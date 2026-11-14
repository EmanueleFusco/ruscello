use std::{path::PathBuf, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot},
    time,
};
use tokio_util::sync::CancellationToken;

use crate::protocol::{
    Failure, MAX_COOKIE_BYTES, MAX_FRAME, MAX_IMPORT_COOKIES, Request, Response, Url, View,
};

#[derive(Debug, Clone)]
pub struct BrowserOptions {
    pub executable: PathBuf,
    pub profile: PathBuf,
    pub idle_timeout: Duration,
}

impl BrowserOptions {
    pub fn installed(data_dir: PathBuf) -> std::io::Result<Self> {
        let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "ruscello-webview.exe"
        } else {
            "ruscello-webview"
        });
        Ok(Self {
            executable,
            profile: data_dir.join("webview_profile"),
            idle_timeout: Duration::from_secs(60),
        })
    }
}

#[derive(Debug, Clone)]
pub struct BrowserClient {
    jobs: mpsc::Sender<Job>,
    shutdown: CancellationToken,
    stopped: CancellationToken,
}

struct Job {
    request: Request,
    cancellation: CancellationToken,
    reply: oneshot::Sender<Result<Response, Failure>>,
}

impl BrowserClient {
    pub fn new(options: BrowserOptions) -> Self {
        let (jobs, receiver) = mpsc::channel(32);
        let shutdown = CancellationToken::new();
        let stopped = CancellationToken::new();
        tokio::spawn({
            let shutdown = shutdown.clone();
            let stopped = stopped.clone();
            async move {
                run(options, receiver, shutdown).await;
                stopped.cancel();
            }
        });
        Self {
            jobs,
            shutdown,
            stopped,
        }
    }

    pub async fn request(
        &self,
        request: Request,
        cancellation: &CancellationToken,
    ) -> Result<Response, Failure> {
        let (reply, result) = oneshot::channel();
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(Failure::Cancelled),
            sent = self.jobs.send(Job { request, cancellation: cancellation.clone(), reply }) => {
                sent.map_err(|_| Failure::Unavailable)?;
            }
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(Failure::Cancelled),
            resolved = result => resolved.map_err(|_| Failure::Unavailable)?,
        }
    }

    /// Imports every eligible cookie in a series of bounded protocol requests.
    pub async fn import_cookies(
        &self,
        url: &Url,
        cookies: Vec<String>,
        cancellation: &CancellationToken,
    ) -> Result<usize, Failure> {
        let mut count = 0;
        let mut cookies = cookies
            .into_iter()
            .filter(|cookie| cookie.len() <= MAX_COOKIE_BYTES);
        loop {
            let batch: Vec<_> = cookies.by_ref().take(MAX_IMPORT_COOKIES).collect();
            if batch.is_empty() {
                return Ok(count);
            }
            // Even with worst-case JSON escaping, the bounded batch fits in a frame.
            let response = self
                .request(
                    Request::Import {
                        url: url.clone(),
                        cookies: batch,
                    },
                    cancellation,
                )
                .await?;
            let Response::Imported { count: imported } = response else {
                return Err(Failure::Unavailable);
            };
            count += imported;
        }
    }

    /// Flushes the persistent profile without starting an idle helper.
    pub async fn stop(&self) {
        self.shutdown.cancel();
        self.stopped.cancelled().await;
    }
}

struct Process {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Process {
    fn spawn(options: &BrowserOptions) -> Result<Self, Failure> {
        let mut command = Command::new(&options.executable);
        command
            .arg("--profile")
            .arg(&options.profile)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW for the helper's console.
        let mut child = command.spawn().map_err(|_| Failure::Unavailable)?;
        let input = child.stdin.take().ok_or(Failure::Unavailable)?;
        let output = BufReader::new(child.stdout.take().ok_or(Failure::Unavailable)?);
        Ok(Self {
            child,
            input,
            output,
        })
    }

    async fn send(&mut self, request: &Request) -> Result<(), Failure> {
        let mut frame = serde_json::to_vec(request).map_err(|_| Failure::InvalidRequest)?;
        if frame.len() >= MAX_FRAME {
            return Err(Failure::InvalidRequest);
        }
        frame.push(b'\n');
        self.input
            .write_all(&frame)
            .await
            .map_err(|_| Failure::Unavailable)
    }

    async fn exchange(&mut self, request: &Request) -> Result<Response, Failure> {
        self.send(request).await?;
        let mut frame = Vec::new();
        (&mut self.output)
            .take(MAX_FRAME as u64)
            .read_until(b'\n', &mut frame)
            .await
            .map_err(|_| Failure::Unavailable)?;
        if frame.last() != Some(&b'\n') {
            return Err(Failure::Unavailable);
        }
        serde_json::from_slice(&frame).map_err(|_| Failure::Unavailable)
    }

    async fn stop(mut self) {
        let stopped = time::timeout(Duration::from_secs(3), async {
            let _ = self.send(&Request::Shutdown).await;
            let _ = self.input.shutdown().await;
            self.child.wait().await
        })
        .await;
        if !matches!(stopped, Ok(Ok(_))) {
            let _ = self.child.kill().await;
        }
    }
}

async fn run(options: BrowserOptions, mut jobs: mpsc::Receiver<Job>, shutdown: CancellationToken) {
    let mut process: Option<Process> = None;
    loop {
        let job = tokio::select! {
            () = shutdown.cancelled() => break,
            job = jobs.recv() => job,
            () = time::sleep(options.idle_timeout), if process.is_some() => {
                if let Some(child) = process.take() {
                    child.stop().await;
                }
                continue;
            }
        };
        let Some(mut job) = job else { break };
        if job.reply.is_closed() || job.cancellation.is_cancelled() {
            continue;
        }
        if process
            .as_mut()
            .is_some_and(|child| !matches!(child.child.try_wait(), Ok(None)))
        {
            process = None;
        }
        if process.is_none() {
            match Process::spawn(&options) {
                Ok(child) => process = Some(child),
                Err(error) => {
                    let _ = job.reply.send(Err(error));
                    continue;
                }
            }
        }
        let Some(child) = &mut process else { continue };
        let timeout = match &job.request {
            Request::Open {
                view: View::Verify(_),
                ..
            } => Duration::from_secs(600),
            Request::Open {
                view: View::Resolve,
                ..
            } => Duration::from_secs(3720),
            _ => Duration::from_secs(45),
        };
        let result = tokio::select! {
            () = shutdown.cancelled() => break,
            () = job.cancellation.cancelled() => Err(Failure::Cancelled),
            () = job.reply.closed() => Err(Failure::Cancelled),
            result = time::timeout(timeout, child.exchange(&job.request)) => {
                result.unwrap_or(Err(Failure::Timeout))
            }
        };
        if result.is_err()
            && let Some(mut child) = process.take()
        {
            let _ = child.child.kill().await;
        }
        let _ = job.reply.send(result.and_then(|response| match response {
            Response::Error { kind } => Err(kind),
            response => Ok(response),
        }));
    }
    drop(jobs);
    if let Some(child) = process {
        child.stop().await;
    }
}
