use std::{
    fs::File,
    io::{self, Read, Write},
    ops::Range,
    path::{Path, PathBuf},
};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Probe;

use super::disk;

const HEADER: &[u8; 6] = b"RSCT\x01\x00";
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
    pub(crate) fn new(source: &Url, probe: &Probe) -> Self {
        let bytes = |value: &reqwest::header::HeaderValue| value.as_bytes().to_vec();
        Self {
            source: url_identity(source),
            final_url: url_identity(&probe.final_url),
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

fn url_identity(url: &Url) -> [u8; 32] {
    let mut url = url.clone();
    url.set_fragment(None);
    Sha256::digest(url.as_str().as_bytes()).into()
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Control {
    pub key: TransferKey,
    pub completed: Vec<Range<u64>>,
}

impl Control {
    pub(super) fn empty(key: TransferKey) -> Self {
        Self {
            key,
            completed: Vec::new(),
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
    if bytes.len() > MAX_CONTROL_BYTES || bytes.len() < HEADER.len() + 4 {
        return Err(invalid());
    }
    let (record, checksum) = bytes.split_at(bytes.len() - 4);
    if !record.starts_with(HEADER) || crc32fast::hash(record).to_le_bytes().as_slice() != checksum {
        return Err(invalid());
    }
    let control: Control = postcard::from_bytes(&record[HEADER.len()..]).map_err(|_| invalid())?;
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
    let payload = postcard::to_stdvec(control).map_err(io::Error::other)?;
    if payload.len() > MAX_CONTROL_BYTES - HEADER.len() - 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "download checkpoint is too large",
        ));
    }
    let mut crc = crc32fast::Hasher::new();
    crc.update(HEADER);
    crc.update(&payload);
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(HEADER)?;
    temporary.write_all(&payload)?;
    temporary.write_all(&crc.finalize().to_le_bytes())?;
    temporary.as_file().sync_all()?;
    disk::replace_checkpoint(temporary.into_temp_path(), path)?;
    disk::sync_directory(directory)
}
