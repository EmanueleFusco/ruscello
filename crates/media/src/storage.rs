use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::MediaError;

const CONTROL_FILE: &str = "segments.ctl";
const INIT_FILE: &str = "init.bin";

/// Crash-safe, segment-level staging directory shared by HLS and DASH track downloads.
#[derive(Debug, Clone)]
pub(crate) struct SegmentStore {
    root: PathBuf,
    ctl_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SegmentCheckpoint {
    track_key: String,
    segment_count: usize,
    init_done: bool,
    completed: Vec<bool>,
    sizes: Vec<u64>,
}

impl SegmentStore {
    /// Opens or initializes a segment staging store at `root`.
    pub(crate) fn open(
        root: impl Into<PathBuf>,
        track_key: &str,
        segment_count: usize,
    ) -> Result<Self, MediaError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let ctl_path = root.join(CONTROL_FILE);
        let mut checkpoint = match fs::read(&ctl_path) {
            Ok(bytes) => serde_json::from_slice::<SegmentCheckpoint>(&bytes).ok(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };

        let reset = checkpoint.as_ref().is_none_or(|state| {
            state.track_key != track_key || state.segment_count != segment_count
        });
        if reset {
            clean_segments(&root)?;
            let initial = SegmentCheckpoint {
                track_key: track_key.to_owned(),
                segment_count,
                init_done: false,
                completed: vec![false; segment_count],
                sizes: vec![0; segment_count],
            };
            write_json_atomic(&ctl_path, &root, &initial)?;
        } else if let Some(ref mut state) = checkpoint {
            let mut repaired = false;
            if state.init_done && !root.join(INIT_FILE).is_file() {
                state.init_done = false;
                repaired = true;
            }
            for index in 0..segment_count {
                if state.completed[index] {
                    let path = segment_path(&root, index);
                    let valid = fs::metadata(&path)
                        .is_ok_and(|meta| meta.is_file() && meta.len() == state.sizes[index]);
                    if !valid {
                        state.completed[index] = false;
                        state.sizes[index] = 0;
                        repaired = true;
                    }
                }
            }
            if repaired {
                write_json_atomic(&ctl_path, &root, state)?;
            }
        }

        Ok(Self {
            root,
            ctl_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Returns which segments still need downloading and the bytes already durably stored.
    pub(crate) fn resume_plan(&self) -> Result<(bool, Vec<bool>, u64), MediaError> {
        let state = self.load_checkpoint()?;
        let mut init_bytes = 0_u64;
        if state.init_done
            && let Ok(meta) = fs::metadata(self.root.join(INIT_FILE))
        {
            init_bytes = meta.len();
        }
        let seg_bytes: u64 = state
            .completed
            .iter()
            .zip(state.sizes.iter())
            .filter(|(done, _)| **done)
            .map(|(_, size)| *size)
            .sum();
        Ok((state.init_done, state.completed, init_bytes + seg_bytes))
    }

    /// Durably writes and fsyncs the initialization segment before marking it complete in `segments.ctl`.
    pub(crate) fn write_init(&self, bytes: &[u8]) -> Result<(), MediaError> {
        let target = self.root.join(INIT_FILE);
        write_bytes_atomic(&target, &self.root, bytes)?;
        let _guard = self
            .ctl_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self.load_checkpoint()?;
        state.init_done = true;
        write_json_atomic(&self.root.join(CONTROL_FILE), &self.root, &state)
    }

    /// Durably writes and fsyncs one media segment before marking it complete in `segments.ctl`.
    pub(crate) fn commit_segment(&self, index: usize, bytes: &[u8]) -> Result<(), MediaError> {
        let target = segment_path(&self.root, index);
        write_bytes_atomic(&target, &self.root, bytes)?;
        let _guard = self
            .ctl_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self.load_checkpoint()?;
        if index < state.segment_count {
            state.completed[index] = true;
            state.sizes[index] = bytes.len() as u64;
            write_json_atomic(&self.root.join(CONTROL_FILE), &self.root, &state)?;
        }
        Ok(())
    }

    /// Concatenates init (if present) and all ordered segments into `destination` via temp+fsync+rename.
    pub(crate) fn concatenate_into(&self, destination: &Path) -> Result<u64, MediaError> {
        let state = self.load_checkpoint()?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_dir = destination.parent().unwrap_or(&self.root);
        let mut temp = NamedTempFile::new_in(temp_dir)?;
        let mut total = 0_u64;
        let mut buf = [0_u8; 64 * 1024];

        if state.init_done {
            total += copy_file_into(&self.root.join(INIT_FILE), temp.as_file_mut(), &mut buf)?;
        }
        for (index, done) in state.completed.iter().enumerate().take(state.segment_count) {
            if !done {
                return Err(MediaError::ManifestParse(format!(
                    "cannot concatenate incomplete segment {index}"
                )));
            }
            total += copy_file_into(
                &segment_path(&self.root, index),
                temp.as_file_mut(),
                &mut buf,
            )?;
        }
        temp.as_file().sync_all()?;
        temp.persist(destination).map_err(|error| error.error)?;
        sync_directory(temp_dir)?;
        Ok(total)
    }

    /// Removes temporary segment files after the staged output has been durably published.
    pub(crate) fn cleanup(&self) -> io::Result<()> {
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
        }
        Ok(())
    }

    fn load_checkpoint(&self) -> Result<SegmentCheckpoint, MediaError> {
        let bytes = fs::read(self.root.join(CONTROL_FILE))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| MediaError::ManifestParse(format!("corrupt segment state: {error}")))
    }
}

fn segment_path(root: &Path, index: usize) -> PathBuf {
    root.join(format!("seg-{index:06}.bin"))
}

fn copy_file_into(path: &Path, out: &mut File, buf: &mut [u8]) -> io::Result<u64> {
    let mut input = File::open(path)?;
    let mut copied = 0_u64;
    loop {
        let read = input.read(buf)?;
        if read == 0 {
            break;
        }
        out.write_all(&buf[..read])?;
        copied += read as u64;
    }
    Ok(copied)
}

fn clean_segments(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub(crate) fn write_bytes_atomic(target: &Path, dir: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut temp = NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(target).map_err(|error| error.error)?;
    sync_directory(dir)
}

fn write_json_atomic<T: Serialize>(target: &Path, dir: &Path, value: &T) -> Result<(), MediaError> {
    let json = serde_json::to_vec(value)
        .map_err(|error| MediaError::ManifestParse(format!("serialize checkpoint: {error}")))?;
    write_bytes_atomic(target, dir, &json)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}
