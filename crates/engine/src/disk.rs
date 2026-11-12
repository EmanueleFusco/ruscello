use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use ruscello_http::{DownloadPreparation, Probe, Url, WriterError, scan_checkpoints};

/// Reports the bytes available to the current process on a filesystem.
pub trait DiskSpaceProbe: std::fmt::Debug + Send + Sync {
    fn available_bytes(&self, directory: &Path) -> io::Result<u64>;
}

#[derive(Debug)]
struct SystemSpace;

impl DiskSpaceProbe for SystemSpace {
    fn available_bytes(&self, directory: &Path) -> io::Result<u64> {
        fs4::available_space(directory)
    }
}

#[derive(Debug, Clone)]
pub struct DiskPolicy {
    pub minimum_free_bytes: u64,
    pub check_interval: Duration,
    probe: Arc<dyn DiskSpaceProbe>,
}

impl Default for DiskPolicy {
    fn default() -> Self {
        Self {
            minimum_free_bytes: 64 * 1024 * 1024,
            check_interval: Duration::from_secs(1),
            probe: Arc::new(SystemSpace),
        }
    }
}

impl DiskPolicy {
    pub fn with_probe(mut self, probe: Arc<dyn DiskSpaceProbe>) -> Self {
        self.probe = probe;
        self
    }

    pub(super) fn interval(&self) -> Duration {
        self.check_interval
            .clamp(Duration::from_millis(50), Duration::from_secs(60))
    }

    pub(super) fn check(&self, directory: &Path, bytes_needed: u64) -> io::Result<()> {
        let available = self.probe.available_bytes(directory)?;
        if available < self.minimum_free_bytes.saturating_add(bytes_needed) {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct Preparation {
    pub(super) policy: DiskPolicy,
    pub(super) directory: PathBuf,
    pub(super) mime_path: PathBuf,
    pub(super) id: u64,
    pub(super) url: Url,
}

impl DownloadPreparation for Preparation {
    fn prepare(&self, metadata: &Probe) -> Result<(), WriterError> {
        let remaining = if let Some(size) = metadata.size {
            let persisted = scan_checkpoints(&self.directory)?
                .into_iter()
                .filter(|checkpoint| {
                    checkpoint.matches_url(&self.url)
                        && checkpoint.job.as_ref().is_none_or(|job| job.id == self.id)
                })
                .map(|checkpoint| checkpoint.downloaded_bytes)
                .max()
                .unwrap_or(0);
            size.saturating_sub(persisted)
        } else {
            0
        };
        self.policy.check(&self.directory, remaining)?;
        if let Some(mime) = &metadata.content_type {
            super::transfer::write_marker(&self.mime_path, mime, &self.directory)?;
        } else if let Err(error) = fs::remove_file(&self.mime_path)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error.into());
        }
        Ok(())
    }
}
