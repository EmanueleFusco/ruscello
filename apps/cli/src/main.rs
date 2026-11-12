//! Headless command-line front-end, also used by crash/resume tests.

use std::{
    env,
    io::{self, Write},
    num::NonZeroUsize,
    path::PathBuf,
    process::ExitCode,
};

use ruscello_engine::{DownloadOptions, HttpDownloader, Url};
use ruscello_platform::{language::Language, logging};

mod messages;

#[tokio::main]
async fn main() -> ExitCode {
    let language = Language::system();
    let log = if let Some(directory) = env::var_os("RUSCELLO_LOG_DIR") {
        logging::init_in("ruscello-cli", &PathBuf::from(directory))
    } else {
        logging::init("ruscello-cli")
    };
    let Ok(_log) = log else {
        eprintln!("{}", language.logging_unavailable());
        return ExitCode::FAILURE;
    };
    let messages = messages::get(language);
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if error.is::<InvalidArguments>() {
                eprintln!("{}", messages.usage);
                return ExitCode::from(2);
            }
            tracing::debug!(%error, "Download command failed");
            eprintln!("{}", messages.failed);
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let (url, options) = arguments()?;
    let path = HttpDownloader::new()?.download(&url, options).await?;
    writeln!(io::stdout().lock(), "{}", path.display())?;
    Ok(())
}

fn arguments() -> anyhow::Result<(Url, DownloadOptions)> {
    let invalid = || anyhow::Error::msg(InvalidArguments);
    let mut args = env::args_os().skip(1);
    if args.next().as_deref().and_then(|value| value.to_str()) != Some("get") {
        return Err(invalid());
    }
    let url = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(invalid)?;
    let url = Url::parse(&url).map_err(|_| invalid())?;
    let mut options = DownloadOptions::default();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(invalid)?;
        match flag.to_str() {
            Some("-o") => options.directory = value.into(),
            Some("-x") => {
                options.connections = value
                    .to_str()
                    .and_then(|value| value.parse::<NonZeroUsize>().ok())
                    .ok_or_else(invalid)?;
            }
            _ => return Err(invalid()),
        }
    }
    Ok((url, options))
}

#[derive(Debug)]
struct InvalidArguments;

impl std::fmt::Display for InvalidArguments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("invalid command arguments")
    }
}
