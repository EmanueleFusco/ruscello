use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{InvalidTransition, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DownloadId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryId(pub u64);

/// Original input used to start or resolve a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Http { url: String },
    Torrent(TorrentSource),
    Media { url: String },
    Hoster { url: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TorrentSource {
    Magnet(String),
    File(PathBuf),
    Url(String),
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub id: PackageId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: CategoryId,
    pub name: String,
    pub destination: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tag(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    pub id: DownloadId,
    pub source: Source,
    pub package: Option<PackageId>,
    pub category: Option<CategoryId>,
    pub tags: Vec<Tag>,
    pub priority: Priority,
    pub destination: PathBuf,
    pub filename: Option<String>,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub completed_path: Option<PathBuf>,
    status: Status,
}

impl Download {
    pub fn new(id: DownloadId, source: Source, destination: PathBuf) -> Self {
        Self {
            id,
            source,
            package: None,
            category: None,
            tags: Vec::new(),
            priority: Priority::default(),
            destination,
            filename: None,
            total_bytes: None,
            downloaded_bytes: 0,
            completed_path: None,
            status: Status::Queued,
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn transition(&mut self, next: Status) -> Result<(), InvalidTransition> {
        self.status.validate_transition(next)?;
        self.status = next;
        Ok(())
    }
}
