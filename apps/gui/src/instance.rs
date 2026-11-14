use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

const MAX_MESSAGE: usize = 256 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn normalize(input: &std::ffi::OsStr) -> String {
    let value = input.to_string_lossy();
    if let Ok(url) = ruscello_engine::Url::parse(&value) {
        if url.scheme() == "file"
            && let Ok(path) = url.to_file_path()
        {
            return path.to_string_lossy().into_owned();
        }
        if matches!(url.scheme(), "http" | "https" | "ftp" | "ftps" | "magnet") {
            return value.into_owned();
        }
    }
    std::path::absolute(Path::new(input)).map_or_else(
        |_| value.into_owned(),
        |path| path.to_string_lossy().into_owned(),
    )
}

#[derive(Debug, Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: [u8; 32],
}

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    token: [u8; 32],
    inputs: Vec<String>,
}

pub(crate) struct Primary {
    _lock: File,
    stop: Arc<std::sync::atomic::AtomicBool>,
    address: std::net::SocketAddr,
    worker: Option<std::thread::JoinHandle<()>>,
    receiver: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<String>>>>,
}

impl Drop for Primary {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = TcpStream::connect_timeout(&self.address, IO_TIMEOUT);
            let _ = worker.join();
        }
    }
}

impl Primary {
    pub(crate) fn receiver(
        &self,
    ) -> Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<String>>>> {
        self.receiver.clone()
    }
}

pub(crate) fn start(directory: &Path, inputs: Vec<String>) -> anyhow::Result<Option<Primary>> {
    std::fs::create_dir_all(directory)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("desktop-instance.lock"))?;
    let endpoint = directory.join("desktop-instance.json");
    for _ in 0..30 {
        match lock.try_lock() {
            Ok(()) => return primary(lock, endpoint).map(Some),
            Err(std::fs::TryLockError::WouldBlock) => {
                if forward(&endpoint, &inputs).is_ok() {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    anyhow::bail!("Could not connect to the running desktop instance")
}

fn primary(lock: File, path: PathBuf) -> anyhow::Result<Primary> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let address = listener.local_addr()?;
    let mut token = [0; 32];
    getrandom::fill(&mut token)
        .map_err(|_| anyhow::anyhow!("Could not generate an IPC credential"))?;
    let endpoint = Endpoint {
        port: address.port(),
        token,
    };
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut file, &endpoint)?;
    file.persist(&path)?;
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = std::thread::Builder::new()
        .name("ruscello-ipc".into())
        .spawn(move || {
            for mut connection in listener.incoming().flatten() {
                if worker_stop.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                let handled = (|| -> anyhow::Result<()> {
                    connection.set_write_timeout(Some(IO_TIMEOUT))?;
                    let request: Request = serde_json::from_slice(&read_frame(&mut TimedRead {
                        stream: &mut connection,
                        deadline: Instant::now() + IO_TIMEOUT,
                    })?)?;
                    anyhow::ensure!(
                        request.token == token && request.inputs.len() <= 1000,
                        "Invalid IPC request"
                    );
                    sender.try_send(request.inputs)?;
                    connection.write_all(&[1])?;
                    Ok(())
                })();
                if handled.is_err() {
                    tracing::debug!("Desktop IPC request rejected");
                }
            }
        })?;
    Ok(Primary {
        _lock: lock,
        stop,
        address,
        worker: Some(worker),
        receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
    })
}

fn forward(path: &Path, inputs: &[String]) -> anyhow::Result<()> {
    let bytes = std::fs::read(path)?;
    let endpoint: Endpoint = serde_json::from_slice(&bytes)?;
    let mut connection =
        TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, endpoint.port).into(), IO_TIMEOUT)?;
    connection.set_read_timeout(Some(IO_TIMEOUT))?;
    connection.set_write_timeout(Some(IO_TIMEOUT))?;
    let request = serde_json::to_vec(&Request {
        token: endpoint.token,
        inputs: inputs.to_vec(),
    })?;
    anyhow::ensure!(request.len() <= MAX_MESSAGE, "IPC request is too large");
    connection.write_all(&(request.len() as u32).to_be_bytes())?;
    connection.write_all(&request)?;
    let mut ack = [0];
    connection.read_exact(&mut ack)?;
    anyhow::ensure!(ack == [1], "IPC request was not accepted");
    Ok(())
}

fn read_frame(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if size > MAX_MESSAGE {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

struct TimedRead<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl Read for TimedRead<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_messages_before_allocating() {
        let invalid = ((MAX_MESSAGE + 1) as u32).to_be_bytes();
        assert!(matches!(
            read_frame(&mut invalid.as_slice()),
            Err(error) if error.kind() == io::ErrorKind::InvalidData
        ));
    }

    #[tokio::test]
    async fn forwards_supported_inputs_to_the_existing_instance() -> anyhow::Result<()> {
        let data = tempfile::tempdir()?;
        let first = start(data.path(), Vec::new())?
            .ok_or_else(|| anyhow::anyhow!("First instance was not primary"))?;
        let inputs = vec![
            "https://example.test/download".to_owned(),
            "ftp://example.test/archive.zip".to_owned(),
            "ftps://example.test/secure.tar.gz".to_owned(),
            "magnet:?xt=urn:btih:abc".to_owned(),
            "example.torrent".to_owned(),
            "example.dlc".to_owned(),
        ];
        assert!(start(data.path(), inputs.clone())?.is_none());
        let receiver = first.receiver();
        let received = tokio::time::timeout(Duration::from_secs(2), async {
            receiver.lock().await.recv().await
        })
        .await?;
        assert_eq!(received, Some(inputs));
        Ok(())
    }
}
