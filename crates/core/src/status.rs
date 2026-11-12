use serde::{Deserialize, Serialize};

/// Structured reasons allow consumers to present localized messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureReason {
    Network,
    AccessDenied,
    SourceUnavailable,
    UnsupportedSource,
    IntegrityCheck,
    FileSystem,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Queued,
    Resolving,
    WaitingUser,
    Downloading,
    Paused,
    Seeding,
    PostProcessing,
    Completed,
    Failed { reason: FailureReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid download status transition from {from:?} to {to:?}")]
pub struct InvalidTransition {
    pub from: Status,
    pub to: Status,
}

impl Status {
    /// Whether this status represented running work before a restart.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Resolving | Self::Downloading | Self::Seeding | Self::PostProcessing
        )
    }

    /// Validates one lifecycle step. Active work can return to the queue for retry or recovery.
    pub fn validate_transition(self, to: Self) -> Result<(), InvalidTransition> {
        use Status::{
            Completed, Downloading, Failed, Paused, PostProcessing, Queued, Resolving, Seeding,
            WaitingUser,
        };
        let allowed = match self {
            Queued => matches!(to, Resolving | Downloading | Paused | Failed { .. }),
            Resolving => matches!(
                to,
                Queued | WaitingUser | Downloading | Paused | Failed { .. }
            ),
            WaitingUser => matches!(to, Queued | Resolving | Paused | Failed { .. }),
            Downloading => matches!(
                to,
                Queued
                    | Resolving
                    | WaitingUser
                    | Paused
                    | Seeding
                    | PostProcessing
                    | Completed
                    | Failed { .. }
            ),
            Paused | Failed { .. } => matches!(to, Queued),
            Seeding => matches!(to, Queued | Paused | Completed | Failed { .. }),
            PostProcessing => matches!(to, Queued | Paused | Seeding | Completed | Failed { .. }),
            Completed => false,
        };
        if self == to || allowed {
            Ok(())
        } else {
            Err(InvalidTransition { from: self, to })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Download, DownloadId, Source};
    use Status::*;

    const FAILED: Status = Failed {
        reason: FailureReason::Network,
    };

    #[test]
    fn lifecycle_transition_matrix() {
        let states = [
            Queued,
            Resolving,
            WaitingUser,
            Downloading,
            Paused,
            Seeding,
            PostProcessing,
            Completed,
            FAILED,
        ];
        // Each row uses the column order above, including idempotent requests.
        let expected = [
            [true, true, false, true, true, false, false, false, true],
            [true, true, true, true, true, false, false, false, true],
            [true, true, true, false, true, false, false, false, true],
            [true, true, true, true, true, true, true, true, true],
            [true, false, false, false, true, false, false, false, false],
            [true, false, false, false, true, true, false, true, true],
            [true, false, false, false, true, true, true, true, true],
            [false, false, false, false, false, false, false, true, false],
            [true, false, false, false, false, false, false, false, true],
        ];
        for (row, from) in states.into_iter().enumerate() {
            for (column, to) in states.into_iter().enumerate() {
                let result = from.validate_transition(to);
                if expected[row][column] {
                    assert_eq!(result, Ok(()));
                } else {
                    assert_eq!(result, Err(InvalidTransition { from, to }));
                }
            }
        }
    }

    #[test]
    fn download_preserves_status_on_invalid_transition() {
        let mut download = Download::new(
            DownloadId(1),
            Source::Http {
                url: "https://example.test/file".into(),
            },
            "downloads".into(),
        );
        assert_eq!(download.status(), Queued);
        assert!(download.transition(Downloading).is_ok());
        assert!(download.transition(Paused).is_ok());
        assert!(download.transition(Completed).is_err());
        assert_eq!(download.status(), Paused);
        assert!(download.transition(Queued).is_ok());
        assert!(download.transition(Downloading).is_ok());
        assert!(download.transition(Completed).is_ok());
        assert!(download.transition(Queued).is_err());
        assert_eq!(download.status(), Completed);
    }

    #[test]
    fn only_running_work_is_active() {
        for status in [Resolving, Downloading, Seeding, PostProcessing] {
            assert!(status.is_active());
        }
        for status in [Queued, WaitingUser, Paused, Completed, FAILED] {
            assert!(!status.is_active());
        }
    }
}
