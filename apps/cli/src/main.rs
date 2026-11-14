//! Headless command-line front-end for the HTTP download engine.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{self, IsTerminal, Write},
    iter::Peekable,
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    process::ExitCode,
};

use ruscello_engine::{
    BandwidthContext, BrowserLocale, BrowserOptions, ChecksumAlgorithm, ContainerFile,
    DownloadOptions, DownloadProgress, EngineHandle, ExpectedChecksum, HttpDownloader, HttpError,
    MediaEngine, MediaError, MediaKind, MediaRequest, MediaTransferJob, PageAnalysis, ResolveError,
    Resolver, SidecarEndpoints, SidecarSettings, Url, is_dash_url, is_hls_url, is_manifest_url,
    prepare_http_download,
};
use ruscello_platform::{language::Language, logging, paths::AppPaths};
use tokio::sync::watch;

mod messages;
mod progress;

#[tokio::main(worker_threads = 2)]
async fn main() -> ExitCode {
    let mut argv = env::args_os().skip(1).peekable();
    let language = match language(&mut argv) {
        Ok(language) => language,
        Err(()) => {
            error(messages::get(Language::system()).usage);
            return ExitCode::from(2);
        }
    };
    let messages = messages::get(language);
    let command = match parse(argv) {
        Ok(Command::Help) => return output(messages.usage),
        Ok(Command::Version) => return output(concat!("ruscello-cli ", env!("CARGO_PKG_VERSION"))),
        Ok(command) => command,
        Err(()) => {
            error(messages.usage);
            return ExitCode::from(2);
        }
    };
    let log = if let Some(directory) = env::var_os("RUSCELLO_LOG_DIR") {
        logging::init_in("ruscello-cli", &PathBuf::from(directory))
    } else {
        logging::init("ruscello-cli")
    };
    let Ok(_log) = log else {
        error(language.logging_unavailable());
        return ExitCode::FAILURE;
    };
    let Command::Get(arguments) = command else {
        return match list_links(command, language).await {
            Ok(links) => output(links.join("\n")),
            Err(failure) => {
                if let Some(media_error) = failure.downcast_ref::<MediaError>() {
                    error(messages.media_failure(media_error));
                } else if let Some(resolve_error) = failure.downcast_ref::<ResolveError>() {
                    error(messages.resolve_failure(resolve_error));
                } else {
                    error(&failure.to_string());
                }
                ExitCode::FAILURE
            }
        };
    };
    match run(*arguments, language).await {
        Ok(path) => output(path.display()),
        Err(failure) => {
            tracing::debug!(error = %failure, "Download command failed");
            let http_error = failure.downcast_ref::<HttpError>();
            let resolver_error = failure.downcast_ref::<ResolveError>();
            let media_error = failure.downcast_ref::<MediaError>();
            if let Some(media) = media_error {
                error(messages.media_failure(media));
            } else if let Some(resolve) = resolver_error {
                error(messages.resolve_failure(resolve));
            } else {
                error(messages.failure(http_error));
            }
            if matches!(http_error, Some(HttpError::Cancelled))
                || matches!(resolver_error, Some(ResolveError::Cancelled))
                || media_error.is_some_and(MediaError::is_cancelled)
            {
                ExitCode::from(130)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

async fn run(mut arguments: Arguments, language: Language) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(&arguments.options.directory).await?;
    let downloader = HttpDownloader::new()?;
    let terminal = io::stderr().is_terminal();
    let show_progress = arguments.progress.unwrap_or(terminal);
    let show_warnings = arguments.progress != Some(false);
    let progress_sender = if show_progress || show_warnings {
        let (sender, receiver) = watch::channel(DownloadProgress::default());
        arguments.options.progress = Some(sender.clone());
        let handle = tokio::spawn(progress::render(
            receiver,
            language,
            terminal,
            show_progress,
        ));
        Some((sender, handle))
    } else {
        None
    };
    let cancellation = arguments.options.cancellation.clone();
    let resolver = resolver()?;
    let is_media =
        arguments.format.is_some() || arguments.ytdlp.is_some() || is_manifest_url(&arguments.url);
    let transfer = async {
        if is_media {
            run_media_download(&downloader, &resolver, &arguments).await
        } else {
            prepare_http_download(
                &resolver,
                &arguments.url,
                &mut arguments.options,
                false,
                arguments.browser.then_some(browser_locale(language)),
            )
            .await?;
            Ok(downloader
                .download(&arguments.url, arguments.options)
                .await?)
        }
    };
    tokio::pin!(transfer);
    let result = tokio::select! {
        result = &mut transfer => result,
        signal = tokio::signal::ctrl_c() => {
            if signal.is_ok() {
                cancellation.cancel();
            }
            transfer.await
        }
    };
    // Dropping options inside transfer (or explicitly closing sender) lets progress render finish.
    if let Some((sender, view)) = progress_sender {
        drop(sender);
        let _ = view.await;
    }
    resolver.stop_browser_helper().await;
    result
}

async fn run_media_download(
    downloader: &HttpDownloader,
    resolver: &Resolver,
    arguments: &Arguments,
) -> anyhow::Result<PathBuf> {
    let settings = sidecar_settings(arguments.ytdlp.clone(), arguments.ffmpeg.clone());
    let data_root =
        discovered_data_dir().unwrap_or_else(|| arguments.options.directory.join(".ruscello-data"));
    let media = MediaEngine::new(downloader.clone(), data_root, SidecarEndpoints::default())?;
    let cancellation = &arguments.options.cancellation;
    let headers = resolver.session_headers(&arguments.url);
    let kind = if is_hls_url(&arguments.url) {
        MediaKind::Hls
    } else if is_dash_url(&arguments.url) {
        MediaKind::Dash
    } else {
        MediaKind::YtDlp
    };
    let work_root = arguments
        .options
        .directory
        .join(format!(".ruscello-media-{:016x}", url_hash(&arguments.url)));
    let ready_dir = work_root.join("ready");
    let request = MediaRequest {
        kind,
        format_selector: arguments.format.clone(),
        ..MediaRequest::default()
    };
    let bandwidth = BandwidthContext {
        global: arguments.options.bandwidth.clone(),
        ..BandwidthContext::default()
    };
    let staged = media
        .download(MediaTransferJob {
            url: &arguments.url,
            request: &request,
            session_headers: &headers,
            working_dir: &work_root,
            ready_dir: &ready_dir,
            filename_hint: arguments.options.filename_hint.as_deref(),
            concurrency: arguments.options.connections.get(),
            bandwidth: &bandwidth,
            settings: &settings,
            cancellation,
            progress: arguments.options.progress.as_ref(),
            on_phase: &|_| {},
        })
        .await?;
    let published = publish_staged_file(&staged, &arguments.options.directory)?;
    let _ = fs::remove_dir_all(&work_root);
    Ok(published)
}

fn sidecar_settings(ytdlp: Option<PathBuf>, ffmpeg: Option<PathBuf>) -> SidecarSettings {
    SidecarSettings {
        ytdlp_consent: ytdlp.is_some(),
        ytdlp_custom_path: ytdlp,
        ffmpeg_consent: ffmpeg.is_some(),
        ffmpeg_custom_path: ffmpeg,
        ..SidecarSettings::default()
    }
}

fn url_hash(url: &Url) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in url.as_str().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn publish_staged_file(staged: &Path, destination_dir: &Path) -> io::Result<PathBuf> {
    let name = staged
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("media.bin");
    let mut target = destination_dir.join(name);
    if target.exists() {
        let stem = Path::new(name)
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or("media");
        let ext = Path::new(name).extension().and_then(OsStr::to_str);
        for index in 1_u32..10_000 {
            let candidate_name = match ext {
                Some(ext) => format!("{stem} ({index}).{ext}"),
                None => format!("{stem} ({index})"),
            };
            let candidate = destination_dir.join(candidate_name);
            if !candidate.exists() {
                target = candidate;
                break;
            }
        }
    }
    if fs::rename(staged, &target).is_err() {
        fs::copy(staged, &target)?;
        fs::File::options().write(true).open(&target)?.sync_all()?;
        let _ = fs::remove_file(staged);
    }
    Ok(target)
}

fn discovered_data_dir() -> Option<PathBuf> {
    env::var_os("RUSCELLO_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            AppPaths::discover()
                .ok()
                .map(|paths| paths.data().to_owned())
        })
}

fn resolver() -> Result<Resolver, ResolveError> {
    let configs = env::var_os("RUSCELLO_RECIPES_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            AppPaths::discover()
                .ok()
                .map(|paths| paths.config().join("recipes"))
        });
    let resolver = Resolver::new(configs)?;
    if let Some(data) = discovered_data_dir()
        && let Ok(options) = BrowserOptions::installed(data)
    {
        Ok(resolver.with_browser_helper(options, true))
    } else {
        Ok(resolver)
    }
}

fn browser_locale(language: Language) -> BrowserLocale {
    match language {
        Language::English => BrowserLocale::En,
        Language::Italian => BrowserLocale::It,
    }
}

async fn list_links(command: Command, language: Language) -> anyhow::Result<Vec<String>> {
    let messages = messages::get(language);
    match command {
        Command::Inspect {
            url,
            browser,
            ytdlp,
            ffmpeg,
        } => {
            if is_manifest_url(&url) || ytdlp.is_some() {
                let downloader = HttpDownloader::new()?;
                let data_root = discovered_data_dir().unwrap_or_else(env::temp_dir);
                let media = MediaEngine::new(downloader, data_root, SidecarEndpoints::default())?;
                let resolver = resolver()?;
                let cancel = Default::default();
                let headers = resolver.session_headers(&url);
                let inspected = media
                    .inspect(&url, &headers, &sidecar_settings(ytdlp, ffmpeg), &cancel)
                    .await?;
                let mut lines = vec![format!(
                    "{} ({:?}): {}",
                    messages.media_label, inspected.kind, inspected.title
                )];
                if !inspected.formats.is_empty() {
                    lines.push(format!("{}:", messages.formats_heading));
                    for fmt in inspected.formats {
                        lines.push(format!("  - {}: {}", fmt.id, fmt.label));
                    }
                }
                if !inspected.playlist_items.is_empty() {
                    let pl_title = inspected
                        .playlist_title
                        .as_deref()
                        .unwrap_or(&inspected.title);
                    lines.push(format!(
                        "{} ({}): {}",
                        messages.playlist_heading,
                        inspected.playlist_items.len(),
                        pl_title
                    ));
                    for item in inspected.playlist_items {
                        lines.push(format!("  {}. {} - {}", item.index, item.title, item.url));
                    }
                }
                return Ok(lines);
            }
            let resolver = resolver()?;
            let cancellation = Default::default();
            let analysis = if browser {
                let mut analysis = resolver.inspect_browser(&url, &cancellation).await;
                if matches!(analysis, Err(ResolveError::VerificationRequired)) {
                    resolver
                        .verify(&url, browser_locale(language), &cancellation)
                        .await?;
                    analysis = resolver.inspect_browser(&url, &cancellation).await;
                }
                analysis?
            } else {
                resolver.inspect(&url, &cancellation).await?
            };
            Ok(match analysis {
                PageAnalysis::Direct(candidate) | PageAnalysis::Hoster { candidate, .. } => {
                    vec![candidate.url.into()]
                }
                PageAnalysis::Page { candidates, .. } => {
                    candidates.into_iter().map(|item| item.url.into()).collect()
                }
            })
        }
        Command::Import {
            path,
            allow_fallback,
        } => match EngineHandle::import_links(&path, allow_fallback).await? {
            ContainerFile::Packages(packages) => Ok(packages
                .into_iter()
                .flat_map(|package| package.links)
                .map(|link| link.url.into())
                .collect()),
            ContainerFile::Torrent => Err(ResolveError::InvalidContainer.into()),
        },
        Command::Queue { urls, paused } => {
            let data_dir = discovered_data_dir()
                .ok_or_else(|| anyhow::anyhow!(messages.local_api_unavailable))?;
            let endpoint = ruscello_api::discover_local_endpoint(&data_dir)
                .ok_or_else(|| anyhow::anyhow!(messages.local_api_unavailable))?;
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?;
            let body = serde_json::json!({
                "urls": urls,
                "start_immediately": !paused,
            });
            let response = client
                .post(format!(
                    "http://127.0.0.1:{}/api/v1/downloads",
                    endpoint.port
                ))
                .bearer_auth(&endpoint.token)
                .json(&body)
                .send()
                .await?
                .error_for_status()?
                .json::<ruscello_api::AddDownloadsResponse>()
                .await?;
            Ok(vec![format!(
                "{}: {}",
                messages.local_api_queued, response.count
            )])
        }
        Command::Status => {
            let data_dir = discovered_data_dir()
                .ok_or_else(|| anyhow::anyhow!(messages.local_api_unavailable))?;
            let endpoint = ruscello_api::discover_local_endpoint(&data_dir)
                .ok_or_else(|| anyhow::anyhow!(messages.local_api_unavailable))?;
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?;
            let snapshot = client
                .get(format!("http://127.0.0.1:{}/api/v1/queue", endpoint.port))
                .bearer_auth(&endpoint.token)
                .send()
                .await?
                .error_for_status()?
                .json::<ruscello_api::QueueSnapshotDto>()
                .await?;
            let mut lines = vec![format!(
                "downloads={} down={}B/s up={}B/s alt_speed={} alt_limit={:?}",
                snapshot.downloads.len(),
                snapshot.total_download_bps,
                snapshot.total_upload_bps,
                snapshot.alternate_speed_enabled,
                snapshot.alternate_bandwidth_bps,
            )];
            for item in snapshot.downloads {
                lines.push(format!(
                    "  #{} [{}] {}",
                    item.id, item.status, item.filename
                ));
            }
            Ok(lines)
        }
        _ => Err(ResolveError::InvalidLink.into()),
    }
}

fn output(value: impl std::fmt::Display) -> ExitCode {
    if writeln!(io::stdout().lock(), "{value}").is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn error(message: &str) {
    let _ = writeln!(io::stderr().lock(), "{message}");
}

struct Arguments {
    url: Url,
    options: DownloadOptions,
    progress: Option<bool>,
    browser: bool,
    format: Option<String>,
    ytdlp: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
}

enum Command {
    Help,
    Version,
    Get(Box<Arguments>),
    Inspect {
        url: Url,
        browser: bool,
        ytdlp: Option<PathBuf>,
        ffmpeg: Option<PathBuf>,
    },
    Import {
        path: PathBuf,
        allow_fallback: bool,
    },
    Queue {
        urls: Vec<String>,
        paused: bool,
    },
    Status,
}

fn language(args: &mut Peekable<impl Iterator<Item = OsString>>) -> Result<Language, ()> {
    if !args.peek().is_some_and(|argument| argument == "--language") {
        return Ok(Language::system());
    }
    args.next();
    match args
        .next()
        .as_deref()
        .and_then(|argument| argument.to_str())
    {
        Some("en") => Ok(Language::English),
        Some("it") => Ok(Language::Italian),
        Some("system") => Ok(Language::system()),
        _ => Err(()),
    }
}

fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Command, ()> {
    match args
        .next()
        .as_deref()
        .and_then(|argument| argument.to_str())
    {
        None | Some("-h" | "--help") => return Ok(Command::Help),
        Some("-V" | "--version") => return Ok(Command::Version),
        Some("inspect") => {
            let url = url_arg(&args.next().ok_or(())?)?;
            let mut browser = false;
            let mut ytdlp = None;
            let mut ffmpeg = None;
            while let Some(flag) = args.next() {
                match flag.to_str() {
                    Some("--browser") => browser = true,
                    Some("--ytdlp") => ytdlp = Some(args.next().ok_or(())?.into()),
                    Some("--ffmpeg") => ffmpeg = Some(args.next().ok_or(())?.into()),
                    _ => return Err(()),
                }
            }
            return Ok(Command::Inspect {
                url,
                browser,
                ytdlp,
                ffmpeg,
            });
        }
        Some("import") => {
            let path = args.next().ok_or(())?.into();
            let allow_fallback = match args.next().as_deref().and_then(OsStr::to_str) {
                Some("--dlc-fallback") => true,
                None => false,
                _ => return Err(()),
            };
            return args
                .next()
                .is_none()
                .then_some(Command::Import {
                    path,
                    allow_fallback,
                })
                .ok_or(());
        }
        Some("queue") => {
            let mut urls = Vec::new();
            let mut paused = false;
            for arg in args {
                let text = arg.to_str().ok_or(())?;
                if text == "--paused" {
                    paused = true;
                } else if text.starts_with('-') {
                    return Err(());
                } else {
                    urls.push(text.to_owned());
                }
            }
            if urls.is_empty() {
                return Err(());
            }
            return Ok(Command::Queue { urls, paused });
        }
        Some("status") => {
            return args.next().is_none().then_some(Command::Status).ok_or(());
        }
        Some("get") => {}
        _ => return Err(()),
    }
    let url = args.next().ok_or(())?;
    if url == "-h" || url == "--help" {
        return Ok(Command::Help);
    }
    let mut arguments = Arguments {
        url: url_arg(&url)?,
        options: DownloadOptions::default(),
        progress: None,
        browser: false,
        format: None,
        ytdlp: None,
        ffmpeg: None,
    };
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("-h" | "--help") => return Ok(Command::Help),
            Some("--progress") => arguments.progress = Some(true),
            Some("--quiet") => arguments.progress = Some(false),
            Some("--browser") => arguments.browser = true,
            Some("--format") => {
                let value = args.next().ok_or(())?;
                let text = value.to_str().ok_or(())?;
                arguments.format = Some(text.to_owned());
            }
            Some("--ytdlp") => arguments.ytdlp = Some(args.next().ok_or(())?.into()),
            Some("--ffmpeg") => arguments.ffmpeg = Some(args.next().ok_or(())?.into()),
            Some("-o" | "--output") => arguments.options.directory = args.next().ok_or(())?.into(),
            Some("-x" | "--connections") => {
                let value = args.next().ok_or(())?;
                arguments.options.connections = value
                    .to_str()
                    .and_then(|value| value.parse::<NonZeroUsize>().ok())
                    .filter(|value| value.get() <= 32)
                    .ok_or(())?;
            }
            Some("--mirror") => arguments
                .options
                .mirrors
                .push(url_arg(&args.next().ok_or(())?)?),
            Some("--checksum") => {
                let value = args.next().ok_or(())?;
                let (algorithm, digest) = value
                    .to_str()
                    .and_then(|value| value.split_once(':'))
                    .ok_or(())?;
                let algorithm = algorithm.parse::<ChecksumAlgorithm>().map_err(|_| ())?;
                arguments.options.expected_checksum =
                    Some(ExpectedChecksum::from_hex(algorithm, digest).map_err(|_| ())?);
            }
            Some("--limit") => {
                let value = args.next().ok_or(())?;
                let limit = value
                    .to_str()
                    .and_then(|value| value.parse::<NonZeroU64>().ok())
                    .ok_or(())?;
                arguments.options.bandwidth.set_rate(Some(limit));
            }
            _ => return Err(()),
        }
    }
    Ok(Command::Get(Box::new(arguments)))
}

fn url_arg(value: &OsStr) -> Result<Url, ()> {
    let url = Url::parse(value.to_str().ok_or(())?).map_err(|_| ())?;
    if matches!(url.scheme(), "http" | "https") {
        Ok(url)
    } else {
        Err(())
    }
}
