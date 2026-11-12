//! Non-blocking daily log files for each executable.

use std::path::Path;

use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{InitError, RollingFileAppender, Rotation},
};
use tracing_subscriber::{filter::LevelFilter, fmt};

use crate::paths::{AppPaths, PathsUnavailable};

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error(transparent)]
    Paths(#[from] PathsUnavailable),
    #[error("could not open the rotating log")]
    Open(#[from] InitError),
    #[error("could not install the tracing subscriber")]
    Subscriber(#[from] tracing::subscriber::SetGlobalDefaultError),
}

/// Installs the process subscriber. Keep the returned guard until the process exits.
pub fn init(application: &str) -> Result<WorkerGuard, LoggingError> {
    let paths = AppPaths::discover()?;
    init_in(application, paths.logs())
}

/// Installs the subscriber in an explicitly chosen directory.
pub fn init_in(application: &str, directory: &Path) -> Result<WorkerGuard, LoggingError> {
    let log = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(application)
        .filename_suffix("log")
        .max_log_files(14)
        .build(directory)?;
    let (writer, guard) = tracing_appender::non_blocking(log);
    let subscriber = fmt()
        .with_ansi(false)
        .with_max_level(LevelFilter::DEBUG)
        .with_writer(writer)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    tracing::debug!("Logging initialized");
    Ok(guard)
}
