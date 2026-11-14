use std::io;

use crate::SidecarKind;

/// Errors returned by media inspection, sidecar management, and stream downloads.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("invalid media URL: {0}")]
    InvalidUrl(String),
    #[error("I/O error during media operation: {0}")]
    Io(#[from] io::Error),
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("unexpected HTTP status {0}")]
    HttpStatus(u16),
    #[error("invalid media manifest: {0}")]
    ManifestParse(String),
    #[error("unsupported media stream: {0}")]
    UnsupportedManifest(String),
    #[error("failed to decrypt encrypted media segment: {0}")]
    Decryption(String),
    #[error("explicit user consent in Settings is required before downloading {0}")]
    ConsentRequired(SidecarKind),
    #[error("{0} sidecar binary is not available")]
    SidecarMissing(SidecarKind),
    #[error("sidecar SHA-256 verification failed: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("failed to extract sidecar archive: {0}")]
    ArchiveExtract(String),
    #[error("{kind} failed: {detail}")]
    SidecarProcess { kind: SidecarKind, detail: String },
    #[error("no suitable media format available for this selection")]
    NoSuitableFormat,
    #[error("download cancelled")]
    Cancelled,
}

impl MediaError {
    /// Returns whether this error represents user cancellation.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// Returns whether this error indicates insufficient disk space or quota.
    pub fn is_low_disk(&self) -> bool {
        matches!(
            self,
            Self::Io(error)
                if matches!(error.kind(), io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded)
        )
    }

    /// Returns whether an HTTP 401/403 status caused the failure.
    pub fn is_access_denied(&self) -> bool {
        matches!(self, Self::HttpStatus(401 | 403))
    }

    /// Returns whether an HTTP error might succeed if retried later.
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            Self::Request(_) | Self::HttpStatus(408 | 429 | 500..=599)
        )
    }
}
