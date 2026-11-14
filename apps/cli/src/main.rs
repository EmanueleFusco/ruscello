//! Headless command-line front-end for the HTTP download engine.

use std::{
    env,
    ffi::{OsStr, OsString},
    io::{self, IsTerminal, Write},
    iter::Peekable,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    process::ExitCode,
};

use ruscello_engine::{
    ChecksumAlgorithm, ContainerFile, DownloadOptions, DownloadProgress, EngineHandle,
    ExpectedChecksum, HttpDownloader, HttpError, PageAnalysis, ResolveError, Resolver, Url,
    resolve_download,
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
        return match list_links(command).await {
            Ok(links) => output(links.join("\n")),
            Err(failure) => {
                error(messages.resolve_failure(&failure));
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
            error(resolver_error.map_or_else(
                || messages.failure(http_error),
                |error| messages.resolve_failure(error),
            ));
            if matches!(http_error, Some(HttpError::Cancelled))
                || matches!(resolver_error, Some(ResolveError::Cancelled))
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
    let view = if show_progress || show_warnings {
        let (sender, receiver) = watch::channel(DownloadProgress::default());
        arguments.options.progress = Some(sender);
        Some(tokio::spawn(progress::render(
            receiver,
            language,
            terminal,
            show_progress,
        )))
    } else {
        None
    };
    let cancellation = arguments.options.cancellation.clone();
    let transfer = async {
        if let Some(resolved) =
            resolve_download(&resolver()?, &arguments.url, &cancellation).await?
        {
            resolved.apply(&mut arguments.options);
        }
        Ok::<_, anyhow::Error>(
            downloader
                .download(&arguments.url, arguments.options)
                .await?,
        )
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
    if let Some(view) = view {
        let _ = view.await;
    }
    result
}

fn resolver() -> Result<Resolver, ResolveError> {
    let configs = env::var_os("RUSCELLO_RECIPES_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            AppPaths::discover()
                .ok()
                .map(|paths| paths.config().join("recipes"))
        });
    Resolver::new(configs)
}

async fn list_links(command: Command) -> Result<Vec<String>, ResolveError> {
    match command {
        Command::Inspect(url) => {
            let analysis = resolver()?.inspect(&url, &Default::default()).await?;
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
        } => {
            match EngineHandle::import_links(&path, allow_fallback)
                .await
                .map_err(|error| match error {
                    ruscello_engine::EngineError::Resolver(error) => error,
                    _ => ResolveError::InvalidContainer,
                })? {
                ContainerFile::Packages(packages) => Ok(packages
                    .into_iter()
                    .flat_map(|package| package.links)
                    .map(|link| link.url.into())
                    .collect()),
                ContainerFile::Torrent => Err(ResolveError::InvalidContainer),
            }
        }
        _ => Err(ResolveError::InvalidLink),
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
}

enum Command {
    Help,
    Version,
    Get(Box<Arguments>),
    Inspect(Url),
    Import { path: PathBuf, allow_fallback: bool },
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
            return args
                .next()
                .is_none()
                .then_some(Command::Inspect(url))
                .ok_or(());
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
    };
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("-h" | "--help") => return Ok(Command::Help),
            Some("--progress") => arguments.progress = Some(true),
            Some("--quiet") => arguments.progress = Some(false),
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
