//! Deterministic in-process mock FTP/FTPS server for offline tests.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use rcgen::generate_simple_self_signed;
use rustls::ServerConfig;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::Notify,
};
use tokio_rustls::TlsAcceptor;

/// Options for configuring the mock FTP/FTPS server.
#[derive(Debug, Clone)]
pub struct FtpServerOptions {
    /// Expected `(username, password)` pair; when `None`, any credentials succeed.
    pub credentials: Option<(String, String)>,
    /// Whether explicit `AUTH TLS` is supported.
    pub allow_tls: bool,
    /// Rejects plain logins until `AUTH TLS` succeeds.
    pub require_tls: bool,
    /// When set, the first `RETR` disconnects after writing this many bytes so the client
    /// must recover using `REST`.
    pub drop_first_after_bytes: Option<usize>,
    /// Rejects `REST` with `502` when false.
    pub allow_restart: bool,
    /// Reports declared length from `SIZE` when true.
    pub support_size: bool,
}

impl Default for FtpServerOptions {
    fn default() -> Self {
        Self {
            credentials: None,
            allow_tls: true,
            require_tls: false,
            drop_first_after_bytes: None,
            allow_restart: true,
            support_size: true,
        }
    }
}

#[derive(Debug)]
struct ServerState {
    files: Mutex<HashMap<String, Vec<u8>>>,
    commands: Mutex<Vec<String>>,
    dropped_once: AtomicBool,
    options: FtpServerOptions,
}

/// Loopback FTP / explicit FTPS test server.
#[derive(Debug)]
pub struct MockFtpServer {
    addr: SocketAddr,
    ca_cert_der: Vec<u8>,
    state: Arc<ServerState>,
    shutdown: Arc<Notify>,
}

impl MockFtpServer {
    /// Starts a mock FTP/FTPS server on an ephemeral loopback port.
    pub async fn start(options: FtpServerOptions) -> Self {
        let cert =
            match generate_simple_self_signed(["127.0.0.1".to_owned(), "localhost".to_owned()]) {
                Ok(cert) => cert,
                Err(error) => panic!("failed to generate mock FTPS cert: {error}"),
            };
        let cert_der = cert.cert.der().to_vec();
        let key_der = rustls_pki_types::PrivateKeyDer::Pkcs8(
            rustls_pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()),
        );
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let tls_config = match ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
        {
            Ok(builder) => match builder
                .with_no_client_auth()
                .with_single_cert(vec![cert.cert.der().clone()], key_der)
            {
                Ok(cfg) => TlsAcceptor::from(Arc::new(cfg)),
                Err(error) => panic!("failed to build mock FTPS TLS cert config: {error}"),
            },
            Err(error) => panic!("failed to configure TLS versions: {error}"),
        };

        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await {
            Ok(listener) => listener,
            Err(error) => panic!("failed to bind mock FTP listener: {error}"),
        };
        let addr = match listener.local_addr() {
            Ok(addr) => addr,
            Err(error) => panic!("failed to read mock FTP listener addr: {error}"),
        };
        let state = Arc::new(ServerState {
            files: Mutex::new(HashMap::new()),
            commands: Mutex::new(Vec::new()),
            dropped_once: AtomicBool::new(false),
            options,
        });
        let shutdown = Arc::new(Notify::new());
        let accept_state = Arc::clone(&state);
        let accept_shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = accept_shutdown.notified() => break,
                    res = listener.accept() => match res {
                        Ok((stream, _)) => {
                            let conn_state = Arc::clone(&accept_state);
                            let acceptor = tls_config.clone();
                            tokio::spawn(async move {
                                let _ = handle_connection(stream, conn_state, acceptor).await;
                            });
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        Self {
            addr,
            ca_cert_der: cert_der,
            state,
            shutdown,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn ca_cert_der(&self) -> &[u8] {
        &self.ca_cert_der
    }

    pub fn url(&self, scheme: &str, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("{scheme}://127.0.0.1:{}/{path}", self.addr.port())
    }

    pub fn register_file(&self, path: &str, data: Vec<u8>) {
        let key = normalize_path(path);
        let mut files = self
            .state
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        files.insert(key, data);
    }

    pub fn observed_commands(&self) -> Vec<String> {
        self.state
            .commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for MockFtpServer {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
    }
}

fn normalize_path(path: &str) -> String {
    path.trim_matches('/').to_owned()
}

fn resolve_file_key(cwd: &str, arg: &str) -> String {
    if arg.starts_with('/') || cwd.is_empty() {
        normalize_path(arg)
    } else {
        let arg = normalize_path(arg);
        if arg.is_empty() {
            cwd.to_owned()
        } else {
            format!("{cwd}/{arg}")
        }
    }
}

enum SessionStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl AsyncRead for SessionStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for SessionStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

async fn handle_connection(
    tcp: TcpStream,
    state: Arc<ServerState>,
    acceptor: TlsAcceptor,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(SessionStream::Plain(tcp));
    reader
        .get_mut()
        .write_all(b"220 Ruscello Mock FTP ready\r\n")
        .await?;

    let mut username = String::new();
    let mut logged_in = false;
    let mut is_tls = false;
    let mut prot_private = false;
    let mut cwd = String::new();
    let mut restart_offset = 0usize;
    let mut passive_listener: Option<TcpListener> = None;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        state
            .commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(trimmed.to_owned());

        let (cmd, arg) = match trimmed.split_once(' ') {
            Some((c, a)) => (c.to_ascii_uppercase(), a.trim()),
            None => (trimmed.to_ascii_uppercase(), ""),
        };

        match cmd.as_str() {
            "AUTH" if arg.eq_ignore_ascii_case("TLS") && state.options.allow_tls && !is_tls => {
                reader
                    .get_mut()
                    .write_all(b"234 Proceed with TLS negotiation\r\n")
                    .await?;
                reader.get_mut().flush().await?;
                let SessionStream::Plain(tcp) = reader.into_inner() else {
                    return Ok(());
                };
                let tls = acceptor.accept(tcp).await?;
                reader = BufReader::new(SessionStream::Tls(Box::new(tls)));
                is_tls = true;
            }
            "USER" => {
                if state.options.require_tls && !is_tls {
                    reader
                        .get_mut()
                        .write_all(b"530 TLS required before authentication\r\n")
                        .await?;
                    continue;
                }
                username = arg.to_owned();
                reader
                    .get_mut()
                    .write_all(b"331 Password required\r\n")
                    .await?;
            }
            "PASS" => {
                if state.options.require_tls && !is_tls {
                    reader
                        .get_mut()
                        .write_all(b"530 TLS required before authentication\r\n")
                        .await?;
                    continue;
                }
                let ok = state
                    .options
                    .credentials
                    .as_ref()
                    .is_none_or(|(user, pass)| user == &username && pass == arg);
                if ok {
                    logged_in = true;
                    reader.get_mut().write_all(b"230 Login OK\r\n").await?;
                } else {
                    logged_in = false;
                    reader
                        .get_mut()
                        .write_all(b"530 Login incorrect\r\n")
                        .await?;
                }
            }
            "PBSZ" => {
                reader.get_mut().write_all(b"200 PBSZ=0\r\n").await?;
            }
            "PROT" => {
                prot_private = arg.eq_ignore_ascii_case("P");
                reader
                    .get_mut()
                    .write_all(b"200 Protection level set\r\n")
                    .await?;
            }
            "FEAT" => {
                reader
                    .get_mut()
                    .write_all(b"211-Features:\r\n SIZE\r\n MDTM\r\n REST STREAM\r\n EPSV\r\n AUTH TLS\r\n211 End\r\n")
                    .await?;
            }
            "TYPE" => {
                reader
                    .get_mut()
                    .write_all(b"200 Switching to Binary mode\r\n")
                    .await?;
            }
            "PWD" => {
                let display = if cwd.is_empty() {
                    "/".to_owned()
                } else {
                    format!("/{cwd}")
                };
                reader
                    .get_mut()
                    .write_all(format!("257 \"{display}\" is current directory\r\n").as_bytes())
                    .await?;
            }
            "CWD" => {
                if !logged_in {
                    reader.get_mut().write_all(b"530 Not logged in\r\n").await?;
                    continue;
                }
                let next = resolve_file_key(&cwd, arg);
                let has_dir = state
                    .files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .keys()
                    .any(|k| k == &next || next.is_empty() || k.starts_with(&format!("{next}/")));
                if has_dir {
                    cwd = next;
                    reader
                        .get_mut()
                        .write_all(b"250 Directory changed\r\n")
                        .await?;
                } else {
                    reader
                        .get_mut()
                        .write_all(b"550 Failed to change directory\r\n")
                        .await?;
                }
            }
            "SIZE" => {
                if !logged_in {
                    reader.get_mut().write_all(b"530 Not logged in\r\n").await?;
                    continue;
                }
                if !state.options.support_size {
                    reader
                        .get_mut()
                        .write_all(b"502 Command not implemented\r\n")
                        .await?;
                    continue;
                }
                let key = resolve_file_key(&cwd, arg);
                let size = state
                    .files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&key)
                    .map(Vec::len);
                if let Some(len) = size {
                    reader
                        .get_mut()
                        .write_all(format!("213 {len}\r\n").as_bytes())
                        .await?;
                } else {
                    reader
                        .get_mut()
                        .write_all(b"550 File not found\r\n")
                        .await?;
                }
            }
            "MDTM" => {
                if !logged_in {
                    reader.get_mut().write_all(b"530 Not logged in\r\n").await?;
                    continue;
                }
                let key = resolve_file_key(&cwd, arg);
                let exists = state
                    .files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains_key(&key);
                if exists {
                    reader
                        .get_mut()
                        .write_all(b"213 20260101120000\r\n")
                        .await?;
                } else {
                    reader
                        .get_mut()
                        .write_all(b"550 File not found\r\n")
                        .await?;
                }
            }
            "EPSV" => {
                let data_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
                let port = data_listener.local_addr()?.port();
                passive_listener = Some(data_listener);
                reader
                    .get_mut()
                    .write_all(
                        format!("229 Entering Extended Passive Mode (|||{port}|)\r\n").as_bytes(),
                    )
                    .await?;
            }
            "PASV" => {
                let data_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
                let port = data_listener.local_addr()?.port();
                passive_listener = Some(data_listener);
                reader
                    .get_mut()
                    .write_all(
                        format!(
                            "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                            port / 256,
                            port % 256
                        )
                        .as_bytes(),
                    )
                    .await?;
            }
            "REST" => {
                if !state.options.allow_restart {
                    restart_offset = 0;
                    reader
                        .get_mut()
                        .write_all(b"502 REST not implemented\r\n")
                        .await?;
                    continue;
                }
                match arg.parse::<usize>() {
                    Ok(offset) => {
                        restart_offset = offset;
                        reader
                            .get_mut()
                            .write_all(format!("350 Restarting at {offset}\r\n").as_bytes())
                            .await?;
                    }
                    Err(_) => {
                        reader
                            .get_mut()
                            .write_all(b"501 Invalid REST offset\r\n")
                            .await?;
                    }
                }
            }
            "RETR" => {
                let offset = std::mem::take(&mut restart_offset);
                if !logged_in {
                    reader.get_mut().write_all(b"530 Not logged in\r\n").await?;
                    continue;
                }
                let Some(listener) = passive_listener.take() else {
                    reader
                        .get_mut()
                        .write_all(b"425 Use PASV or EPSV first\r\n")
                        .await?;
                    continue;
                };
                let key = resolve_file_key(&cwd, arg);
                let data = state
                    .files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&key)
                    .cloned();
                let Some(bytes) = data else {
                    reader
                        .get_mut()
                        .write_all(b"550 File not found\r\n")
                        .await?;
                    continue;
                };
                if offset > bytes.len() {
                    reader
                        .get_mut()
                        .write_all(b"550 Invalid restart offset\r\n")
                        .await?;
                    continue;
                }
                reader
                    .get_mut()
                    .write_all(b"150 Opening BINARY mode data connection\r\n")
                    .await?;
                reader.get_mut().flush().await?;

                let (data_tcp, _) = listener.accept().await?;
                let mut data_stream = if is_tls && prot_private {
                    SessionStream::Tls(Box::new(acceptor.accept(data_tcp).await?))
                } else {
                    SessionStream::Plain(data_tcp)
                };

                let slice = &bytes[offset..];
                let should_drop = state.options.drop_first_after_bytes.is_some_and(|limit| {
                    limit < slice.len() && !state.dropped_once.swap(true, Ordering::AcqRel)
                });
                if should_drop {
                    let limit = state.options.drop_first_after_bytes.unwrap_or(0);
                    data_stream.write_all(&slice[..limit]).await?;
                    data_stream.flush().await?;
                    let _ = data_stream.shutdown().await;
                    reader
                        .get_mut()
                        .write_all(b"426 Connection closed; transfer aborted\r\n")
                        .await?;
                    return Ok(());
                }

                data_stream.write_all(slice).await?;
                data_stream.flush().await?;
                data_stream.shutdown().await?;
                drop(data_stream);
                reader
                    .get_mut()
                    .write_all(b"226 Transfer complete\r\n")
                    .await?;
            }
            "QUIT" => {
                reader.get_mut().write_all(b"221 Goodbye\r\n").await?;
                return Ok(());
            }
            _ => {
                reader
                    .get_mut()
                    .write_all(b"502 Command not implemented\r\n")
                    .await?;
            }
        }
    }
}
