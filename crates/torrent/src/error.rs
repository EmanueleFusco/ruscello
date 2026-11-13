use std::io;

use ruscello_core::FailureReason;

/// Errors returned by the BitTorrent engine wrapper.
#[derive(Debug, thiserror::Error)]
pub enum TorrentError {
    #[error("invalid torrent source: {0}")]
    InvalidSource(String),
    #[error("invalid BitTorrent listening port")]
    InvalidPort,
    #[error("invalid BitTorrent seed limit: {0}")]
    InvalidLimit(String),
    #[error("invalid network interface '{0}'")]
    InvalidInterface(String),
    #[error("failed to load IP blocklist: {0}")]
    Blocklist(String),
    #[error("torrent operation was cancelled")]
    Cancelled,
    #[error("torrent is not registered in the session")]
    NotFound,
    #[error("torrent metadata is still being fetched")]
    NotResolved,
    #[error("torrent file I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("failed to fetch .torrent over HTTP: {0}")]
    Http(String),
    #[error("BitTorrent session error: {0}")]
    Session(String),
}

impl TorrentError {
    /// Maps a torrent failure into Ruscello's cross-engine failure reason.
    pub fn failure_reason(&self) -> FailureReason {
        match self {
            Self::InvalidSource(_) | Self::NotResolved => FailureReason::SourceUnavailable,
            Self::Io(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                FailureReason::AccessDenied
            }
            Self::Io(_) => FailureReason::FileSystem,
            Self::Http(_) | Self::Blocklist(_) => FailureReason::Network,
            Self::InvalidPort
            | Self::InvalidLimit(_)
            | Self::InvalidInterface(_)
            | Self::Cancelled
            | Self::NotFound
            | Self::Session(_) => FailureReason::Internal,
        }
    }
}
