//! Non-blocking daily log files for each executable.

use std::path::{Component, Path, PathBuf};

use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{InitError, RollingFileAppender, Rotation},
};
use tracing_subscriber::{
    filter::{LevelFilter, Targets},
    fmt,
    prelude::*,
};

use crate::paths::{AppPaths, PathsUnavailable};

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error(transparent)]
    Paths(#[from] PathsUnavailable),
    #[error("could not open the rotating log")]
    Open(#[from] InitError),
    #[error("could not install the tracing subscriber")]
    Subscriber(#[from] tracing::subscriber::SetGlobalDefaultError),
    #[error("invalid application log directory name")]
    ApplicationName,
}

/// Installs the process subscriber. Keep the returned guard until the process exits.
pub fn init(application: &str) -> Result<WorkerGuard, LoggingError> {
    let paths = AppPaths::discover()?;
    init_in(
        application,
        &application_directory(paths.logs(), application)?,
    )
}

fn application_directory(base: &Path, application: &str) -> Result<PathBuf, LoggingError> {
    let mut components = Path::new(application).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) if name == application => Ok(base.join(name)),
        _ => Err(LoggingError::ApplicationName),
    }
}

fn log_filter() -> Targets {
    // Keep Ruscello diagnostics at DEBUG while silencing chatty third-party HTTP/2, GPU, and UI
    // frame traces (e.g. `h2::codec::framed_read`) that otherwise bloat log files to hundreds of MB.
    Targets::new()
        .with_default(LevelFilter::WARN)
        .with_targets([
            ("ruscello", LevelFilter::DEBUG),
            ("ruscello_api", LevelFilter::DEBUG),
            ("ruscello_browser", LevelFilter::DEBUG),
            ("ruscello_cli", LevelFilter::DEBUG),
            ("ruscello_core", LevelFilter::DEBUG),
            ("ruscello_engine", LevelFilter::DEBUG),
            ("ruscello_gui", LevelFilter::DEBUG),
            ("ruscello_http", LevelFilter::DEBUG),
            ("ruscello_media", LevelFilter::DEBUG),
            ("ruscello_platform", LevelFilter::DEBUG),
            ("ruscello_resolver", LevelFilter::DEBUG),
            ("ruscello_storage", LevelFilter::DEBUG),
            ("ruscello_torrent", LevelFilter::DEBUG),
            ("ruscello_webview", LevelFilter::DEBUG),
        ])
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
    let subscriber = tracing_subscriber::registry()
        .with(fmt::layer().with_ansi(false).with_writer(writer))
        .with(log_filter());
    tracing::subscriber::set_global_default(subscriber)?;
    tracing::debug!("Logging initialized");
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_prefixes_have_independent_rotation_directories() -> Result<(), LoggingError> {
        let base = Path::new("logs");
        let gui = application_directory(base, "ruscello")?;
        let cli = application_directory(base, "ruscello-cli")?;
        assert_eq!(gui.parent(), Some(base));
        assert_eq!(cli.parent(), Some(base));
        assert_ne!(gui, cli);
        assert!(matches!(
            application_directory(base, "../outside"),
            Err(LoggingError::ApplicationName)
        ));
        Ok(())
    }

    #[test]
    fn log_filter_keeps_ruscello_debug_and_silences_noisy_third_party_debug() {
        let filter = log_filter();
        assert!(filter.would_enable("ruscello_engine::scheduler", &tracing::Level::DEBUG));
        assert!(filter.would_enable("ruscello_gui::app", &tracing::Level::DEBUG));
        assert!(filter.would_enable("h2::codec::framed_read", &tracing::Level::WARN));
        assert!(!filter.would_enable("h2::codec::framed_read", &tracing::Level::DEBUG));
        assert!(!filter.would_enable("wgpu_core::device", &tracing::Level::INFO));
    }
}
