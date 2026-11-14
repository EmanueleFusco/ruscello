use std::{
    fs::File,
    io::{self, Read, Write},
    ops::Range,
    path::{Path, PathBuf},
};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ExpectedChecksum, Probe};

use super::disk;

const LEGACY_HEADER: &[u8; 6] = b"RSCT\x01\x00";
const MANAGED_HEADER: &[u8; 6] = b"RSCT\x02\x00";
const MAX_CONTROL_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TransferKey {
    pub source: [u8; 32],
    final_url: [u8; 32],
    pub size: Option<u64>,
    etag: Option<Vec<u8>>,
    last_modified: Option<Vec<u8>>,
    if_range: Option<Vec<u8>>,
}

impl TransferKey {
    pub(crate) fn new(
        source: &Url,
        probe: &Probe,
        checksum: Option<&ExpectedChecksum>,
        resolved: bool,
    ) -> Self {
        let bytes = |value: &reqwest::header::HeaderValue| value.as_bytes().to_vec();
        // A plugin promises that refreshing a share URL selects the same logical file.
        // Strong entity tags remain scoped to the same download origin.
        let mut final_url =
            if resolved && probe.validators.etag.is_some() && probe.validators.if_range().is_some()
            {
                Sha256::digest(format!(
                    "resolved:{}:{}",
                    probe.final_url.origin().ascii_serialization(),
                    source
                ))
                .into()
            } else {
                url_identity(&probe.final_url)
            };
        if let Some(checksum) = checksum {
            // Checkpoints containing mirror data must retain the same verification requirement.
            final_url = checksum.bind_identity(final_url);
        }
        Self {
            source: url_identity(source),
            final_url,
            size: probe.size,
            etag: probe.validators.etag.as_ref().map(bytes),
            last_modified: probe.validators.last_modified.as_ref().map(bytes),
            if_range: probe
                .validators
                .if_range()
                .filter(|_| probe.supports_ranges)
                .map(bytes),
        }
    }

    pub(super) fn can_resume(&self, previous: &Self) -> bool {
        self.size.is_some()
            && self.if_range.is_some()
            && self.source == previous.source
            && self.final_url == previous.final_url
            && self.size == previous.size
            && self.if_range == previous.if_range
    }
}

pub(super) fn url_identity(url: &Url) -> [u8; 32] {
    let mut url = url.clone();
    url.set_fragment(None);
    Sha256::digest(url.as_str().as_bytes()).into()
}

/// Queue identity and destination needed to reconstruct a managed transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointJob {
    pub id: u64,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Recovery {
    pub job: CheckpointJob,
    pub source: Option<String>,
}

impl Recovery {
    pub(crate) fn new(job: CheckpointJob, url: &Url) -> Self {
        let mut url = url.clone();
        url.set_fragment(None);
        // Signed query strings and user information are never copied into a checkpoint.
        let source =
            (url.username().is_empty() && url.password().is_none() && url.query().is_none())
                .then(|| url.into());
        Self { job, source }
    }
}

#[derive(Debug)]
pub(super) struct Control {
    pub key: TransferKey,
    pub completed: Vec<Range<u64>>,
    pub recovery: Option<Recovery>,
}

#[derive(Serialize, Deserialize)]
struct Transfer {
    key: TransferKey,
    completed: Vec<Range<u64>>,
}

#[derive(Serialize)]
struct TransferRef<'a> {
    key: &'a TransferKey,
    completed: &'a [Range<u64>],
}

impl Control {
    pub(super) fn empty(key: TransferKey, recovery: Option<Recovery>) -> Self {
        Self {
            key,
            completed: Vec::new(),
            recovery,
        }
    }
}

pub(super) fn path(partial: &Path) -> PathBuf {
    let mut path = partial.as_os_str().to_os_string();
    path.push(".ctl");
    PathBuf::from(path)
}

pub(super) fn read(path: &Path) -> io::Result<Control> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_CONTROL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid download checkpoint");
    if bytes.len() > MAX_CONTROL_BYTES || bytes.len() < LEGACY_HEADER.len() + 4 {
        return Err(invalid());
    }
    let (record, checksum) = bytes.split_at(bytes.len() - 4);
    if crc32fast::hash(record).to_le_bytes().as_slice() != checksum {
        return Err(invalid());
    }
    let (transfer, recovery): (Transfer, Option<Recovery>) =
        if let Some(payload) = record.strip_prefix(LEGACY_HEADER) {
            (postcard::from_bytes(payload).map_err(|_| invalid())?, None)
        } else if let Some(payload) = record.strip_prefix(MANAGED_HEADER) {
            let (transfer, recovery) = postcard::from_bytes(payload).map_err(|_| invalid())?;
            (transfer, Some(recovery))
        } else {
            return Err(invalid());
        };
    let control = Control {
        key: transfer.key,
        completed: transfer.completed,
        recovery,
    };
    let mut previous_end = None;
    for range in &control.completed {
        if range.is_empty()
            || previous_end.is_some_and(|end| end >= range.start)
            || control.key.size.is_some_and(|size| range.end > size)
        {
            return Err(invalid());
        }
        previous_end = Some(range.end);
    }
    Ok(control)
}

/// The caller must synchronize the partial data before writing any checkpoint.
pub(super) fn write(directory: &Path, path: &Path, control: &Control) -> io::Result<()> {
    let transfer = TransferRef {
        key: &control.key,
        completed: &control.completed,
    };
    let (header, payload) = match &control.recovery {
        Some(recovery) => (MANAGED_HEADER, postcard::to_stdvec(&(transfer, recovery))),
        None => (LEGACY_HEADER, postcard::to_stdvec(&transfer)),
    };
    let payload = payload.map_err(io::Error::other)?;
    if payload.len() > MAX_CONTROL_BYTES - header.len() - 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "download checkpoint is too large",
        ));
    }
    let mut crc = crc32fast::Hasher::new();
    crc.update(header);
    crc.update(&payload);
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(header)?;
    temporary.write_all(&payload)?;
    temporary.write_all(&crc.finalize().to_le_bytes())?;
    temporary.as_file().sync_all()?;
    disk::replace_checkpoint(temporary.into_temp_path(), path)?;
    disk::sync_directory(directory)
}
