use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use ruscello_core::{Download, DownloadId, Source, Status};
use ruscello_http::{Checkpoint, Url, WriterError, scan_checkpoints};
use ruscello_storage::{Change, DatabaseRecovery, QueueEntry, Snapshot, Storage, StorageError};

/// Contains per-job working directories. Startup scans only this one level below recovery roots.
pub const RECOVERY_FOLDER: &str = ".ruscello-incoming";

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("checkpoint reconciliation stopped")]
    Worker(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Transition(#[from] ruscello_core::InvalidTransition),
}

/// Loads and durably reconciles the queue before scheduling any transfer.
pub async fn recover_queue(
    storage: &Storage,
    recovery: DatabaseRecovery,
    directories: Vec<PathBuf>,
) -> Result<Snapshot, StartupError> {
    let snapshot = storage.snapshot().await?;
    let (snapshot, changes) =
        tokio::task::spawn_blocking(move || reconcile(snapshot, recovery, directories)).await??;
    storage.apply(changes).await?;
    Ok(snapshot)
}

fn reconcile(
    mut snapshot: Snapshot,
    recovery: DatabaseRecovery,
    directories: Vec<PathBuf>,
) -> Result<(Snapshot, Vec<Change>), StartupError> {
    let mut roots = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for directory in directories {
        seen.insert(directory.clone());
        match fs::read_dir(directory.join(RECOVERY_FOLDER)) {
            Ok(children) => {
                for child in children.flatten() {
                    if child.file_type().is_ok_and(|kind| kind.is_dir()) {
                        seen.insert(child.path());
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "Could not enumerate working folders")
            }
        }
        roots.insert(directory);
    }
    seen.extend(
        snapshot
            .queue
            .iter()
            .map(|entry| entry.working_directory.clone()),
    );
    let checkpoints: BTreeMap<_, _> = seen
        .into_iter()
        .map(|directory| {
            let checkpoints = match scan_checkpoints(&directory) {
                Ok(checkpoints) => checkpoints,
                Err(WriterError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    Vec::new()
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not scan a working folder");
                    Vec::new()
                }
            };
            (directory, checkpoints)
        })
        .collect();
    let mut changes = Vec::new();
    let mut ids: BTreeSet<_> = snapshot
        .queue
        .iter()
        .map(|entry| entry.download.id)
        .collect();
    let mut consumed = BTreeSet::new();

    for entry in &mut snapshot.queue {
        let before = entry.clone();
        let download = &mut entry.download;
        if download.status().is_active() {
            download.transition(Status::Queued)?;
        }
        if download.status() == Status::Completed && download.file_type.is_none() {
            let path = download
                .completed_path
                .as_deref()
                .or_else(|| download.filename.as_deref().map(Path::new))
                .unwrap_or(Path::new(""));
            download.file_type = Some(super::file_types::detect(
                path,
                None,
                download.source.kind(),
            ));
        }
        if download.status() != Status::Completed
            && let Source::Http { url } | Source::Hoster { url } = &download.source
        {
            let checkpoint = Url::parse(url).ok().and_then(|url| {
                checkpoints
                    .get(&entry.working_directory)?
                    .iter()
                    .filter(|checkpoint| {
                        !consumed.contains(&checkpoint.partial)
                            && checkpoint.matches_url(&url)
                            && checkpoint
                                .job
                                .as_ref()
                                .is_none_or(|job| job.id == download.id.0)
                    })
                    .max_by_key(|checkpoint| {
                        (checkpoint.job.is_some(), checkpoint.downloaded_bytes)
                    })
            });
            if let Some(checkpoint) = checkpoint {
                apply_checkpoint(download, checkpoint);
                consumed.insert(checkpoint.partial.clone());
            } else {
                download.downloaded_bytes = 0;
            }
        }
        if *entry != before {
            changes.push(Change::SaveDownload(Box::new(entry.clone())));
        }
    }

    if recovery != DatabaseRecovery::Unchanged {
        let mut position = snapshot
            .queue
            .last()
            .map_or(0, |entry| entry.position.saturating_add(1));
        for (working_directory, checkpoints) in &checkpoints {
            if !in_recovery_root(working_directory, &roots) {
                continue;
            }
            for checkpoint in checkpoints {
                let Some(job) = &checkpoint.job else { continue };
                let Some(url) = checkpoint.recovery_url() else {
                    continue;
                };
                let id = DownloadId(job.id);
                if job.id == 0
                    || job.id > i64::MAX as u64
                    || consumed.contains(&checkpoint.partial)
                    || !job.destination.is_absolute()
                    || !ids.insert(id)
                {
                    continue;
                }
                let mut download = Download::new(
                    id,
                    Source::Http {
                        url: url.to_string(),
                    },
                    job.destination.clone(),
                );
                download.transition(Status::Paused)?;
                apply_checkpoint(&mut download, checkpoint);
                let entry = QueueEntry {
                    download,
                    position,
                    working_directory: working_directory.clone(),
                    torrent: None,
                    http: Default::default(),
                    media: None,
                };
                position = position.saturating_add(1);
                changes.push(Change::SaveDownload(Box::new(entry.clone())));
                snapshot.queue.push(entry);
            }
        }
        changes.push(Change::FinishCheckpointRecovery);
    }
    tracing::debug!(
        updates = changes.len(),
        "Reconciled the queue with download checkpoints"
    );
    Ok((snapshot, changes))
}

fn in_recovery_root(directory: &Path, roots: &BTreeSet<PathBuf>) -> bool {
    roots.contains(directory)
        || directory.parent().is_some_and(|parent| {
            parent.ends_with(RECOVERY_FOLDER)
                && parent.parent().is_some_and(|root| roots.contains(root))
        })
}

fn apply_checkpoint(download: &mut Download, checkpoint: &Checkpoint) {
    download.downloaded_bytes = checkpoint.downloaded_bytes;
    download.total_bytes = checkpoint.total_bytes;
    download.filename = checkpoint
        .partial
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".rscpart"))
        .map(str::to_owned);
}
