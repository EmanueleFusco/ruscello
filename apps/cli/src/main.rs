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
    ChecksumAlgorithm, DownloadOptions, DownloadProgress, ExpectedChecksum, HttpDownloader,
    HttpError, Url,
};
use ruscello_platform::{language::Language, logging};
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
    let arguments = match parse(argv) {
        Ok(Command::Help) => return output(messages.usage),
        Ok(Command::Version) => return output(concat!("ruscello-cli ", env!("CARGO_PKG_VERSION"))),
        Ok(Command::Get(arguments)) => arguments,
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
    match run(*arguments, language).await {
        Ok(path) => output(path.display()),
        Err(failure) => {
            tracing::debug!(error = %failure, "Download command failed");
            let http_error = failure.downcast_ref::<HttpError>();
            error(messages.failure(http_error));
            if matches!(http_error, Some(HttpError::Cancelled)) {
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
    let transfer = downloader.download(&arguments.url, arguments.options);
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
    Ok(result?)
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
