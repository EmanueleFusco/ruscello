use std::{
    error::Error,
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use ruscello_core::{DownloadId, Source, Status};
use ruscello_engine::{
    AddDownload, DiskPolicy, DiskSpaceProbe, EngineEvent, EngineHandle, EngineLog, EngineOptions,
    RECOVERY_FOLDER,
};
use ruscello_http::scan_checkpoints;
use ruscello_testkit::{FileSpec, TestServer};
use tokio::{sync::broadcast, time::timeout};

type TestResult = Result<(), Box<dyn Error>>;
const SIZE: u64 = 512 * 1024;
const RESERVE: u64 = 64 * 1024;

#[derive(Debug)]
struct Space {
    temporary_directory: PathBuf,
    temporary_bytes: AtomicU64,
}

impl DiskSpaceProbe for Space {
    fn available_bytes(&self, directory: &Path) -> io::Result<u64> {
        if directory.starts_with(&self.temporary_directory) {
            Ok(self.temporary_bytes.load(Ordering::SeqCst))
        } else {
            Ok(u64::MAX)
        }
    }
}

fn options(directory: &Path, available: u64) -> (EngineOptions, Arc<Space>) {
    let staging = directory.join("staging");
    let space = Arc::new(Space {
        temporary_directory: staging.clone(),
        temporary_bytes: AtomicU64::new(available),
    });
    let mut disk = DiskPolicy::default().with_probe(space.clone());
    disk.minimum_free_bytes = RESERVE;
    disk.check_interval = Duration::from_millis(50);
    let mut options =
        EngineOptions::new(directory.join("ruscello.db"), directory.join("downloads"));
    options.temporary_directory = Some(staging);
    options.disk = disk;
    options.limits.connections_per_download = NonZeroUsize::MIN;
    (options, space)
}

async fn warning(events: &mut broadcast::Receiver<EngineEvent>, id: DownloadId) -> TestResult {
    loop {
        if matches!(events.recv().await?, EngineEvent::Log { id: Some(actual), message: EngineLog::DiskSpaceLow } if actual == id)
        {
            return Ok(());
        }
    }
}

async fn completed(events: &mut broadcast::Receiver<EngineEvent>, id: DownloadId) -> TestResult {
    loop {
        if let EngineEvent::StatusChanged { id: actual, status } = events.recv().await?
            && actual == id
        {
            match status {
                Status::Completed => return Ok(()),
                Status::Failed { .. } | Status::Paused => {
                    return Err(format!("unexpected status {status:?}").into());
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn checks_known_size_before_opening_a_partial_file() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(1, SIZE));
    let temp = tempfile::tempdir()?;
    let (options, space) = options(temp.path(), SIZE);
    let engine = EngineHandle::start(options.clone()).await?;
    let mut events = engine.subscribe();
    let id = engine
        .add(AddDownload::new(Source::Http {
            url: file.url().into(),
        }))
        .await?;
    timeout(Duration::from_secs(10), warning(&mut events, id)).await??;
    assert_eq!(
        engine.snapshot().await?.downloads[0].status(),
        Status::Paused
    );
    let working = space
        .temporary_directory
        .join(RECOVERY_FOLDER)
        .join(id.0.to_string());
    assert!(scan_checkpoints(&working)?.is_empty());
    assert!(
        file.requests()
            .iter()
            .all(|request| request.if_range.is_none())
    );

    space.temporary_bytes.store(u64::MAX, Ordering::SeqCst);
    engine.resume(id).await?;
    timeout(Duration::from_secs(10), completed(&mut events, id)).await??;
    let path = options.destination.join("payload.bin");
    assert_eq!(fs::read(path)?, file.payload().bytes(0..SIZE));
    engine.shutdown().await?;
    assert!(!working.exists());
    Ok(())
}

#[tokio::test]
async fn pauses_an_active_transfer_and_keeps_its_checkpoint_in_a_separate_directory() -> TestResult
{
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(2, SIZE).throttle_per_conn(192 * 1024));
    let temp = tempfile::tempdir()?;
    let (options, space) = options(temp.path(), u64::MAX);
    let engine = EngineHandle::start(options.clone()).await?;
    let mut events = engine.subscribe();
    let id = engine
        .add(AddDownload::new(Source::Http {
            url: file.url().into(),
        }))
        .await?;
    timeout(Duration::from_secs(10), async {
        loop {
            if matches!(events.recv().await?, EngineEvent::Progress(updates)
                if updates.iter().any(|update| update.id == id && update.progress.completed > 0))
            {
                return Ok::<_, broadcast::error::RecvError>(());
            }
        }
    })
    .await??;
    space.temporary_bytes.store(RESERVE - 1, Ordering::SeqCst);
    timeout(Duration::from_secs(10), warning(&mut events, id)).await??;
    assert_eq!(
        engine.snapshot().await?.downloads[0].status(),
        Status::Paused
    );
    let working = space
        .temporary_directory
        .join(RECOVERY_FOLDER)
        .join(id.0.to_string());
    let checkpoints = scan_checkpoints(&working)?;
    assert_eq!(checkpoints.len(), 1);
    let persisted = checkpoints[0].downloaded_bytes;
    assert!(persisted > 0 && persisted < SIZE);
    assert!(!options.destination.join("payload.bin").exists());
    engine.shutdown().await?;

    space.temporary_bytes.store(u64::MAX, Ordering::SeqCst);
    let engine = EngineHandle::start(options.clone()).await?;
    assert_eq!(
        engine.snapshot().await?.downloads[0].status(),
        Status::Paused
    );
    let mut events = engine.subscribe();
    engine.resume(id).await?;
    timeout(Duration::from_secs(10), completed(&mut events, id)).await??;
    let range = format!("bytes={persisted}-{}", SIZE - 1);
    assert!(
        file.requests()
            .iter()
            .any(|request| request.if_range.is_some()
                && request.range.as_deref() == Some(range.as_str()))
    );
    assert_eq!(
        fs::read(options.destination.join("payload.bin"))?,
        file.payload().bytes(0..SIZE)
    );
    engine.shutdown().await?;
    assert!(!working.exists());
    Ok(())
}
