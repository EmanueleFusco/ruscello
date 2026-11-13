use std::{
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    Http,
    Torrent,
    Media,
    Hoster,
}

impl Source {
    pub fn kind(&self) -> SourceKind {
        match self {
            Self::Http { .. } => SourceKind::Http,
            Self::Torrent(_) => SourceKind::Torrent,
            Self::Media { .. } => SourceKind::Media,
            Self::Hoster { .. } => SourceKind::Hoster,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileType {
    Video,
    Audio,
    Images,
    Documents,
    Archives,
    Programs,
    Torrents,
    Other,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultCategory {
    Games,
    MoviesAndTv,
    Music,
    Software,
    Work,
}

impl DefaultCategory {
    pub const ALL: [Self; 5] = [
        Self::Games,
        Self::MoviesAndTv,
        Self::Music,
        Self::Software,
        Self::Work,
    ];

    pub fn id(self) -> CategoryId {
        CategoryId(i64::MAX as u64 - 4 + self as u64)
    }

    pub fn english_name(self) -> &'static str {
        match self {
            Self::Games => "Games",
            Self::MoviesAndTv => "Movies & TV",
            Self::Music => "Music",
            Self::Software => "Software",
            Self::Work => "Work",
        }
    }
}

impl Category {
    pub fn default_kind(&self) -> Option<DefaultCategory> {
        DefaultCategory::ALL
            .into_iter()
            .find(|kind| kind.id() == self.id && kind.english_name() == self.name)
    }
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
    #[serde(default)]
    pub file_type: Option<FileType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<NonZeroUsize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth: Option<NonZeroU64>,
    /// An optional algorithm name and expected hexadecimal digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Box<str>>,
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
            file_type: None,
            max_connections: None,
            bandwidth: None,
            checksum: None,
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
