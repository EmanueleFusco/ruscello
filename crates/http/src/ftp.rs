use std::{
    fmt,
    ops::Range,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use reqwest::{Url, header::HeaderValue};
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::CertificateDer;
use rustls_platform_verifier::BuilderVerifierExt;
use suppaftp::{
    FtpError,
    tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream, AsyncRustlsStream, TransferStream},
    types::FileType as FtpFileType,
};
use tokio::{io::AsyncReadExt, time};
use tokio_rustls::TlsConnector;
use tracing::{debug, trace};

use crate::{
    BandwidthLimit, Connectivity, DownloadOptions, DownloadProgress, HttpError, Probe, Validators,
    WriterError,
    download::TransferLimits,
    filename::suggest_filename,
    retry::{ErrorClass, RetryState, wait},
    writer::{FileWriter, Recovery, TransferKey},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const READ_BUFFER_BYTES: usize = 32 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(120);
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(2);
const CHECKPOINT_BYTES: u64 = 16 * 1024 * 1024;

/// Optional credentials for authenticated FTP/FTPS downloads.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct FtpCredentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for FtpCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpCredentials")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Strips any embedded password and URL fragment so credentials never reach persistence,
/// `.rscpart.ctl` checkpoints, or logs.
pub fn sanitize_ftp_url(url: &Url) -> Url {
    let mut sanitized = url.clone();
    sanitized.set_fragment(None);
    if sanitized.password().is_some() {
        let _ = sanitized.set_password(None);
    }
    sanitized
}

pub(crate) async fn download(
    url: &Url,
    options: DownloadOptions,
    global_bandwidth: &BandwidthLimit,
    connectivity: &Connectivity,
) -> Result<PathBuf, HttpError> {
    if !options.mirrors.is_empty() && options.expected_checksum.is_none() {
        return Err(HttpError::MirrorChecksumRequired);
    }
    let clean_url = sanitize_ftp_url(url);
    let credentials = resolve_credentials(url, options.ftp_credentials.as_ref());
    let target = parse_target(&clean_url)?;
    let tls = build_tls_connector(&options.ftp_root_certificates)?;
    let limits = TransferLimits {
        global: global_bandwidth.clone(),
        local: options.bandwidth.clone(),
        individual: options
            .download_bandwidth
            .map(|rate| BandwidthLimit::new(Some(rate))),
    };

    let mut state = RetryState::default();
    let mut restarted = false;
    loop {
        if !connectivity.is_online() {
            options.waiting(true);
            connectivity.wait_online(&options.cancellation).await?;
            options.waiting(false);
        }
        let error = match Box::pin(attempt_transfer(
            &clean_url,
            &target,
            &credentials,
            &tls,
            &options,
            &limits,
            &mut state,
            &mut restarted,
        ))
        .await
        {
            Ok(path) => return Ok(path),
            Err(error) => error,
        };
        if error.classification() != ErrorClass::Transient || options.cancellation.is_cancelled() {
            return Err(error);
        }
        let Some(delay) = state.next_delay(&error, &options.retry, connectivity.is_online()) else {
            return Err(error);
        };
        wait(delay, &options.cancellation).await?;
    }
}

#[derive(Debug, Clone)]
struct FtpTarget {
    host: String,
    port: u16,
    secure: bool,
    implicit_tls: bool,
    directories: Vec<String>,
    filename: String,
}

fn parse_target(url: &Url) -> Result<FtpTarget, HttpError> {
    let secure = match url.scheme() {
        "ftp" => false,
        "ftps" => true,
        _ => return Err(HttpError::UnsupportedScheme),
    };
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or(HttpError::UnsupportedScheme)?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = url.port().unwrap_or(21);
    let implicit_tls = secure && url.port() == Some(990);

    let mut segments: Vec<String> = url
        .path_segments()
        .map(|parts| {
            parts
                .filter(|part| !part.is_empty())
                .map(|part| {
                    percent_encoding::percent_decode_str(part)
                        .decode_utf8_lossy()
                        .into_owned()
                })
                .collect()
        })
        .unwrap_or_default();

    let Some(filename) = segments.pop().filter(|name| !name.is_empty()) else {
        return Err(HttpError::Ftp {
            message: "FTP URL does not name a file".into(),
            transient: false,
        });
    };
    if filename == "." || filename == ".." || segments.iter().any(|seg| seg == "." || seg == "..") {
        return Err(HttpError::Ftp {
            message: "invalid FTP path".into(),
            transient: false,
        });
    }
    Ok(FtpTarget {
        host,
        port,
        secure,
        implicit_tls,
        directories: segments,
        filename,
    })
}

fn resolve_credentials(url: &Url, explicit: Option<&FtpCredentials>) -> FtpCredentials {
    if let Some(creds) = explicit {
        return creds.clone();
    }
    if !url.username().is_empty() || url.password().is_some() {
        let username = percent_encoding::percent_decode_str(url.username())
            .decode_utf8_lossy()
            .into_owned();
        let password = url
            .password()
            .map(|p| {
                percent_encoding::percent_decode_str(p)
                    .decode_utf8_lossy()
                    .into_owned()
            })
            .unwrap_or_default();
        return FtpCredentials {
            username: if username.is_empty() {
                "anonymous".into()
            } else {
                username
            },
            password,
        };
    }
    FtpCredentials {
        username: "anonymous".into(),
        password: "ruscello@example.invalid".into(),
    }
}

fn build_tls_connector(extra_roots: &[Vec<u8>]) -> Result<TlsConnector, HttpError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| HttpError::Ftp {
            message: error.to_string(),
            transient: false,
        })?;
    let config = if extra_roots.is_empty() {
        builder
            .with_platform_verifier()
            .map_err(|error| HttpError::Ftp {
                message: error.to_string(),
                transient: false,
            })?
            .with_no_client_auth()
    } else {
        let mut store = RootCertStore::empty();
        for der in extra_roots {
            store
                .add(CertificateDer::from(der.clone()))
                .map_err(|error| HttpError::Ftp {
                    message: error.to_string(),
                    transient: false,
                })?;
        }
        builder.with_root_certificates(store).with_no_client_auth()
    };
    Ok(TlsConnector::from(Arc::new(config)))
}

async fn connect_and_prepare(
    target: &FtpTarget,
    credentials: &FtpCredentials,
    tls: &TlsConnector,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<(AsyncRustlsFtpStream, Option<u64>, Option<String>), HttpError> {
    let addr = format!("{}:{}", target.host, target.port);
    let mut stream = tokio::select! {
        () = cancellation.cancelled() => return Err(HttpError::Cancelled),
        result = time::timeout(CONNECT_TIMEOUT, async {
            if target.implicit_tls {
                AsyncRustlsFtpStream::connect_secure_implicit(
                    addr.as_str(),
                    AsyncRustlsConnector::from(tls.clone()),
                    &target.host,
                )
                .await
            } else {
                let plain = AsyncRustlsFtpStream::connect(addr.as_str()).await?;
                if target.secure {
                    plain
                        .into_secure(AsyncRustlsConnector::from(tls.clone()), &target.host)
                        .await
                } else {
                    Ok(plain)
                }
            }
        }) => match result {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => return Err(map_ftp_error(error)),
            Err(_) => {
                return Err(HttpError::Ftp {
                    message: "FTP connection timed out".into(),
                    transient: true,
                });
            }
        }
    };

    if let Err(error) = stream
        .login(&credentials.username, &credentials.password)
        .await
    {
        return Err(map_ftp_error(error));
    }
    if let Err(error) = stream.transfer_type(FtpFileType::Binary).await {
        return Err(map_ftp_error(error));
    }
    for dir in &target.directories {
        if let Err(error) = stream.cwd(dir).await {
            return Err(map_ftp_error(error));
        }
    }
    let size = match stream.size(&target.filename).await {
        Ok(size) => u64::try_from(size).ok(),
        Err(_) => None,
    };
    let mdtm = stream
        .mdtm(&target.filename)
        .await
        .ok()
        .map(|dt| dt.format("%Y%m%d%H%M%S").to_string());
    Ok((stream, size, mdtm))
}

#[allow(clippy::too_many_arguments)]
async fn attempt_transfer(
    clean_url: &Url,
    target: &FtpTarget,
    credentials: &FtpCredentials,
    tls: &TlsConnector,
    options: &DownloadOptions,
    limits: &TransferLimits,
    state: &mut RetryState,
    restarted: &mut bool,
) -> Result<PathBuf, HttpError> {
    let (mut ftp, size, mdtm) = Box::pin(connect_and_prepare(
        target,
        credentials,
        tls,
        &options.cancellation,
    ))
    .await?;
    let validator_tag = mdtm
        .as_deref()
        .map(|ts| format!("\"ftp-mdtm:{ts}\""))
        .or_else(|| size.map(|len| format!("\"ftp-size:{len}\"")));
    let mut validators = Validators::default();
    if let Some(tag) = validator_tag.and_then(|raw| HeaderValue::from_str(&raw).ok()) {
        validators.etag = Some(tag);
    }
    let filename = options
        .filename_hint
        .as_deref()
        .map(crate::sanitize_filename)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| suggest_filename(&reqwest::header::HeaderMap::new(), clean_url));
    let probe = Probe {
        final_url: clean_url.clone(),
        size,
        supports_ranges: size.is_some(),
        filename: filename.clone(),
        content_type: None,
        validators,
        attachment: false,
    };
    if let Some(preparation) = &options.preparation {
        preparation.prepare(&probe)?;
    }

    let key = TransferKey::new(clean_url, &probe, options.expected_checksum.as_ref(), false);
    let recovery = options
        .checkpoint_job
        .clone()
        .map(|job| Recovery::new(job, clean_url));
    let (mut writer, completed, opened_fresh) =
        FileWriter::open_download(&options.directory, &filename, key, recovery, false).await?;
    if opened_fresh {
        *restarted = true;
    }

    let mut resume_offset = completed_prefix(&completed);
    let mut progress = DownloadProgress {
        completed: resume_offset,
        total: size,
        resumable: size.is_some(),
        restarted: *restarted,
        ..DownloadProgress::default()
    };
    options.report(progress);

    if let Some(total) = size
        && resume_offset >= total
    {
        let _ = ftp.quit().await;
        return finalize_writer(writer, options, progress).await;
    }

    let stream = if resume_offset > 0 {
        let Some(offset_usize) = usize::try_from(resume_offset).ok() else {
            return Err(HttpError::Ftp {
                message: "resume offset exceeds platform limit".into(),
                transient: false,
            });
        };
        if let Err(error) = ftp.resume_transfer(offset_usize).await {
            debug!(%error, "FTP REST command refused; restarting from offset 0");
            writer.invalidate_checkpoint().await?;
            writer.close().await;
            *restarted = true;
            return Box::pin(restart_from_zero(
                &mut ftp, clean_url, target, size, &probe, options, limits, state,
            ))
            .await;
        }
        match ftp.retr_as_stream(&target.filename).await {
            Ok(stream) => stream,
            Err(error) if is_restart_rejected(&error) => {
                debug!(%error, "FTP server rejected REST on RETR; restarting from offset 0");
                let _ = ftp.quit().await;
                writer.invalidate_checkpoint().await?;
                writer.close().await;
                *restarted = true;
                let (mut fresh_ftp, _, _) = Box::pin(connect_and_prepare(
                    target,
                    credentials,
                    tls,
                    &options.cancellation,
                ))
                .await?;
                return Box::pin(restart_from_zero(
                    &mut fresh_ftp,
                    clean_url,
                    target,
                    size,
                    &probe,
                    options,
                    limits,
                    state,
                ))
                .await;
            }
            Err(error) => {
                writer.checkpoint().await?;
                writer.close().await;
                return Err(map_ftp_error(error));
            }
        }
    } else {
        match ftp.retr_as_stream(&target.filename).await {
            Ok(stream) => stream,
            Err(error) => {
                writer.checkpoint().await?;
                writer.close().await;
                return Err(map_ftp_error(error));
            }
        }
    };

    let transfer_result = stream_to_writer(
        &mut ftp,
        stream,
        &mut writer,
        &mut resume_offset,
        size,
        options,
        limits,
        state,
        &mut progress,
    )
    .await;

    match transfer_result {
        Ok(()) => finalize_writer(writer, options, progress).await,
        Err(error) => {
            if !matches!(error, HttpError::Storage(_)) {
                let _ = writer.checkpoint().await;
            }
            writer.close().await;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn restart_from_zero(
    ftp: &mut AsyncRustlsFtpStream,
    clean_url: &Url,
    target: &FtpTarget,
    size: Option<u64>,
    probe: &Probe,
    options: &DownloadOptions,
    limits: &TransferLimits,
    state: &mut RetryState,
) -> Result<PathBuf, HttpError> {
    let key = TransferKey::new(clean_url, probe, options.expected_checksum.as_ref(), false);
    let recovery = options
        .checkpoint_job
        .clone()
        .map(|job| Recovery::new(job, clean_url));
    let (mut writer, _, _) =
        FileWriter::open_download(&options.directory, &probe.filename, key, recovery, false)
            .await?;
    writer.invalidate_checkpoint().await?;
    let mut offset = 0u64;
    let mut progress = DownloadProgress {
        completed: 0,
        total: size,
        resumable: false,
        restarted: true,
        ..DownloadProgress::default()
    };
    options.report(progress);
    let stream = match ftp.retr_as_stream(&target.filename).await {
        Ok(stream) => stream,
        Err(error) => {
            writer.close().await;
            return Err(map_ftp_error(error));
        }
    };
    match stream_to_writer(
        ftp,
        stream,
        &mut writer,
        &mut offset,
        size,
        options,
        limits,
        state,
        &mut progress,
    )
    .await
    {
        Ok(()) => finalize_writer(writer, options, progress).await,
        Err(error) => {
            let _ = writer.checkpoint().await;
            writer.close().await;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_to_writer(
    ftp: &mut AsyncRustlsFtpStream,
    mut stream: TransferStream<AsyncRustlsStream>,
    writer: &mut FileWriter,
    offset: &mut u64,
    expected_size: Option<u64>,
    options: &DownloadOptions,
    limits: &TransferLimits,
    state: &mut RetryState,
    progress: &mut DownloadProgress,
) -> Result<(), HttpError> {
    let mut buf = vec![0u8; READ_BUFFER_BYTES];
    let mut last_report = Instant::now();
    let mut last_checkpoint = Instant::now();
    let mut uncheckpointed_bytes = 0u64;

    loop {
        let read = tokio::select! {
            () = options.cancellation.cancelled() => return Err(HttpError::Cancelled),
            res = time::timeout(READ_TIMEOUT, stream.read(&mut buf)) => match res {
                Ok(Ok(n)) => n,
                Ok(Err(error)) => {
                    return Err(HttpError::Ftp {
                        message: error.to_string(),
                        transient: true,
                    });
                }
                Err(_) => {
                    return Err(HttpError::Ftp {
                        message: "FTP data read timed out".into(),
                        transient: true,
                    });
                }
            }
        };
        if read == 0 {
            break;
        }
        let chunk = Bytes::copy_from_slice(&buf[..read]);
        let mut cursor = 0usize;
        while cursor < chunk.len() {
            let slice = limits.next_chunk(&chunk, cursor).await;
            let len = slice.len();
            writer.queue_at(*offset, slice).await?;
            *offset = offset.saturating_add(len as u64);
            cursor += len;
            uncheckpointed_bytes = uncheckpointed_bytes.saturating_add(len as u64);
        }
        state.record_progress(*offset);
        progress.completed = *offset;

        let now = Instant::now();
        if uncheckpointed_bytes >= CHECKPOINT_BYTES
            || now.duration_since(last_checkpoint) >= CHECKPOINT_INTERVAL
        {
            writer.checkpoint().await?;
            last_checkpoint = now;
            uncheckpointed_bytes = 0;
        }
        if now.duration_since(last_report) >= PROGRESS_INTERVAL {
            options.report(*progress);
            last_report = now;
        }
    }

    writer.checkpoint().await?;
    let finalize_res = stream.finish().await;
    if let Some(expected) = expected_size {
        if *offset < expected {
            trace!(
                downloaded = *offset,
                expected, "FTP stream terminated before declared SIZE"
            );
            return Err(HttpError::Truncated);
        }
    } else if let Err(error) = finalize_res {
        return Err(map_ftp_error(error));
    }
    let _ = ftp.quit().await;
    Ok(())
}

async fn finalize_writer(
    writer: FileWriter,
    options: &DownloadOptions,
    progress: DownloadProgress,
) -> Result<PathBuf, HttpError> {
    let path = if let Some(checksum) = options.expected_checksum.clone() {
        match writer
            .finish_verified(
                checksum,
                options.cancellation.clone(),
                options.completion_directory.clone(),
            )
            .await
        {
            Err(WriterError::Cancelled) => return Err(HttpError::Cancelled),
            result => result?,
        }
    } else {
        writer
            .finish_into(options.completion_directory.clone())
            .await?
    };
    options.report(progress);
    Ok(path)
}

fn completed_prefix(ranges: &[Range<u64>]) -> u64 {
    let mut cursor = 0u64;
    for range in ranges {
        if range.start <= cursor && range.end > cursor {
            cursor = range.end;
        }
    }
    cursor
}

fn is_restart_rejected(error: &FtpError) -> bool {
    matches!(error, FtpError::UnexpectedResponse(resp) if matches!(
        resp.status,
        suppaftp::Status::NotImplemented
            | suppaftp::Status::NotImplementedParameter
            | suppaftp::Status::BadCommand
            | suppaftp::Status::BadArguments
            | suppaftp::Status::BadSequence
    ))
}

fn map_ftp_error(error: FtpError) -> HttpError {
    match error {
        FtpError::UnexpectedResponse(resp) => {
            let code = resp.status.code();
            match code {
                530 | 332 => HttpError::FtpAuthentication,
                400..=499 => HttpError::Ftp {
                    message: resp.to_string(),
                    transient: true,
                },
                _ => HttpError::Ftp {
                    message: resp.to_string(),
                    transient: false,
                },
            }
        }
        FtpError::ConnectionError(io_err) => HttpError::Ftp {
            message: io_err.to_string(),
            transient: true,
        },
        FtpError::BadResponse | FtpError::InvalidAddress(_) => HttpError::Ftp {
            message: error.to_string(),
            transient: true,
        },
        FtpError::SecureError(msg) => HttpError::Ftp {
            message: msg,
            transient: false,
        },
        other => HttpError::Ftp {
            message: other.to_string(),
            transient: false,
        },
    }
}
