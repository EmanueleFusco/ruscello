use std::{collections::BTreeSet, error::Error, fs, num::NonZeroUsize, time::Duration};

use ruscello_core::{DownloadId, FailureReason, Priority, Source, Status};
use ruscello_engine::{
    AddDownload, EngineEvent, EngineHandle, EngineOptions, Limits, RECOVERY_FOLDER,
};
use ruscello_http::scan_checkpoints;
use ruscello_testkit::{FileSpec, TestServer};
use tokio::{sync::broadcast, time::timeout};

type TestResult = Result<(), Box<dyn Error>>;
const SIZE: u64 = 384 * 1024;
const THREE: NonZeroUsize = NonZeroUsize::MIN.saturating_add(2);

#[tokio::test]
async fn persists_expected_checksum_and_rejects_mismatched_output() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(4, SIZE));
    let temp = tempfile::tempdir()?;
    let options = EngineOptions::new(temp.path().join("queue.db"), temp.path().join("downloads"));
    let engine = EngineHandle::start(options.clone()).await?;
    let mut request = add(file.url());
    request.start_immediately = false;
    request.checksum = Some(format!("sha256:{}", "0".repeat(64)));
    let id = engine.add(request).await?;
    engine.shutdown().await?;
    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();
    engine.resume(id).await?;
    timeout(Duration::from_secs(10), async {
        loop {
            if let EngineEvent::StatusChanged {
                id: changed,
                status,
            } = events.recv().await?
                && changed == id
                && matches!(status, Status::Completed | Status::Failed { .. })
            {
                assert_eq!(
                    status,
                    Status::Failed {
                        reason: FailureReason::IntegrityCheck
                    }
                );
                return Ok::<_, broadcast::error::RecvError>(());
            }
        }
    })
    .await??;
    assert!(
        engine.snapshot().await?.downloads[0]
            .completed_path
            .is_none()
    );
    engine.shutdown().await?;
    Ok(())
}

fn add(url: &str) -> AddDownload {
    AddDownload::new(Source::Http { url: url.into() })
}

fn track(
    event: EngineEvent,
    active: &mut BTreeSet<DownloadId>,
    completed: &mut BTreeSet<DownloadId>,
) {
    if let EngineEvent::StatusChanged { id, status } = event {
        if status.is_active() {
            active.insert(id);
        } else {
            active.remove(&id);
        }
        if status == Status::Completed {
            completed.insert(id);
        }
        assert!(
            !matches!(status, Status::Failed { .. }),
            "download {id:?} failed: {status:?}"
        );
    }
}

#[tokio::test]
async fn completes_fifty_downloads_with_three_slots_and_a_mid_queue_restart() -> TestResult {
    timeout(Duration::from_secs(90), run_fifty()).await?
}

async fn run_fifty() -> TestResult {
    let server = TestServer::start().await?;
    let files: Vec<_> = (0..50)
        .map(|seed| server.file(FileSpec::random(seed, SIZE).throttle_per_conn(512 * 1024)))
        .collect();
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("downloads");
    let mut options = EngineOptions::new(temp.path().join("ruscello.db"), output.clone());
    options.limits = Limits {
        concurrent_downloads: THREE,
        connections_per_host: THREE,
        connections_per_download: NonZeroUsize::MIN,
        ..Limits::default()
    };
    let engine = EngineHandle::start(options.clone()).await?;
    let mut events = engine.subscribe();
    let ids = engine
        .add_many(files.iter().map(|file| add(file.url())).collect())
        .await?;
    let mut active = BTreeSet::new();
    let mut completed = BTreeSet::new();
    let mut peak = 0;
    loop {
        let event = events.recv().await?;
        let mid_transfer = match &event {
            EngineEvent::Progress(updates) => updates
                .iter()
                .find(|update| {
                    update.progress.completed > 0 && update.progress.completed < SIZE / 2
                })
                .map(|update| update.id),
            _ => None,
        };
        track(event, &mut active, &mut completed);
        peak = peak.max(active.len());
        assert!(peak <= 3);
        if completed.len() >= 25
            && let Some(id) = mid_transfer.filter(|id| active.contains(id))
        {
            assert!(completed.len() < 45);
            engine.pause(id).await?;
            let working = output.join(RECOVERY_FOLDER).join(id.0.to_string());
            if working.is_dir()
                && scan_checkpoints(&working)?
                    .iter()
                    .any(|checkpoint| checkpoint.downloaded_bytes > 0)
            {
                break;
            }
        }
    }
    assert_eq!(peak, 3);
    engine.shutdown().await?;
    let mut resumed = Vec::new();
    for (id, file) in ids.iter().zip(&files) {
        let working = output.join(RECOVERY_FOLDER).join(id.0.to_string());
        if working.is_dir() {
            for checkpoint in scan_checkpoints(&working)? {
                if checkpoint.downloaded_bytes > 0 {
                    resumed.push((*id, checkpoint.downloaded_bytes, file.requests().len()));
                }
            }
        }
    }
    assert!(
        !resumed.is_empty(),
        "restart must occur with a durable partial download"
    );

    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();
    let snapshot = engine.snapshot().await?;
    for download in &snapshot.downloads {
        if download.status() == Status::Paused {
            engine.resume(download.id).await?;
        }
    }
    active.clear();
    completed.clear();
    assert!(
        snapshot
            .downloads
            .iter()
            .filter(|download| download.status().is_active())
            .count()
            <= 3
    );
    for download in &snapshot.downloads {
        if download.status() == Status::Completed {
            completed.insert(download.id);
        }
    }
    while completed.len() < files.len() {
        track(events.recv().await?, &mut active, &mut completed);
        assert!(active.len() <= 3);
    }
    let snapshot = engine.snapshot().await?;
    let mut paths = BTreeSet::new();
    for (id, file) in ids.into_iter().zip(files) {
        let download = snapshot
            .downloads
            .iter()
            .find(|download| download.id == id)
            .ok_or("download missing after restart")?;
        assert_eq!(download.status(), Status::Completed);
        let path = download
            .completed_path
            .as_ref()
            .ok_or("missing published path")?;
        assert!(paths.insert(path));
        assert_eq!(fs::read(path)?, file.payload().bytes(0..SIZE));
        if let Some((_, persisted, previous_requests)) =
            resumed.iter().find(|(candidate, _, _)| *candidate == id)
        {
            let range = format!("bytes={persisted}-{}", SIZE - 1);
            assert!(
                file.requests()
                    .iter()
                    .skip(*previous_requests)
                    .any(|request| {
                        request.if_range.is_some()
                            && request.range.as_deref() == Some(range.as_str())
                    })
            );
        }
    }
    engine.shutdown().await?;
    Ok(())
}

async fn next_started(
    events: &mut broadcast::Receiver<EngineEvent>,
) -> Result<DownloadId, Box<dyn Error>> {
    loop {
        if let EngineEvent::StatusChanged {
            id,
            status: Status::Downloading,
        } = events.recv().await?
        {
            return Ok(id);
        }
    }
}

#[tokio::test]
async fn honors_force_priority_and_manual_order_and_supports_both_cancel_modes() -> TestResult {
    timeout(Duration::from_secs(30), run_queue_commands()).await?
}

async fn run_queue_commands() -> TestResult {
    let server = TestServer::start().await?;
    let slow = server.file(FileSpec::random(0, SIZE).throttle_per_conn(64 * 1024));
    let fast = server.file(FileSpec::random(1, 16 * 1024).redirects(2));
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("downloads");
    let options = EngineOptions::new(temp.path().join("ruscello.db"), output.clone());
    let engine = EngineHandle::start(options.clone()).await?;
    let limits = Limits {
        concurrent_downloads: NonZeroUsize::MIN,
        connections_per_host: NonZeroUsize::MIN,
        ..Limits::default()
    };
    engine.set_limits(limits).await?;
    let mut events = engine.subscribe();
    let blocker = engine.add(add(slow.url())).await?;
    assert_eq!(next_started(&mut events).await?, blocker);
    let requests = (0..4).map(|_| add(fast.url())).collect();
    let ids = engine.add_many(requests).await?;
    let (normal, high, earlier, forced) = (ids[0], ids[1], ids[2], ids[3]);
    engine.set_priority(high, Priority::High).await?;
    engine.set_priority(earlier, Priority::High).await?;
    engine.move_before(earlier, Some(high)).await?;
    engine.force_start(forced).await?;
    loop {
        if matches!(events.recv().await?, EngineEvent::Progress(updates)
            if updates.iter().any(|update| update.id == blocker && update.progress.completed > 0))
        {
            break;
        }
    }
    engine.pause(blocker).await?;
    for expected in [forced, earlier, high, normal] {
        assert_eq!(next_started(&mut events).await?, expected);
    }
    let snapshot = loop {
        let snapshot = engine.snapshot().await?;
        if snapshot
            .downloads
            .iter()
            .any(|download| download.id == normal && download.status() == Status::Completed)
        {
            break snapshot;
        }
        events.recv().await?;
    };
    assert_eq!(
        snapshot
            .downloads
            .iter()
            .find(|download| download.id == blocker)
            .map(|download| download.status()),
        Some(Status::Paused)
    );
    let completed_path = snapshot
        .downloads
        .iter()
        .find(|download| download.id == forced)
        .and_then(|download| download.completed_path.clone())
        .ok_or("forced download missing")?;
    engine.cancel(forced, true).await?;
    assert!(!completed_path.try_exists()?);
    let working = output.join(RECOVERY_FOLDER).join(blocker.0.to_string());
    engine.cancel(blocker, false).await?;
    assert!(
        scan_checkpoints(&working)?
            .iter()
            .any(|checkpoint| checkpoint.downloaded_bytes > 0)
    );
    engine.shutdown().await?;

    let engine = EngineHandle::start(options).await?;
    let snapshot = engine.snapshot().await?;
    assert_eq!(snapshot.limits, limits);
    assert_eq!(
        snapshot
            .downloads
            .iter()
            .map(|download| download.id)
            .collect::<Vec<_>>(),
        [normal, earlier, high]
    );
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn stops_retrying_an_interrupted_transfer_after_the_configured_budget() -> TestResult {
    let server = TestServer::start().await?;
    let broken = server.file(FileSpec::random(2, SIZE).drop_after_bytes(32 * 1024));
    let temp = tempfile::tempdir()?;
    let mut options = EngineOptions::new(
        temp.path().join("ruscello.db"),
        temp.path().join("downloads"),
    );
    options.limits.connections_per_download = NonZeroUsize::MIN;
    options.limits.retries = 2;
    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();
    let id = engine.add(add(broken.url())).await?;
    let reason = timeout(Duration::from_secs(10), async {
        loop {
            if let EngineEvent::StatusChanged {
                id: actual,
                status: Status::Failed { reason },
            } = events.recv().await?
                && actual == id
            {
                return Ok::<_, broadcast::error::RecvError>(reason);
            }
        }
    })
    .await??;
    assert_eq!(reason, ruscello_core::FailureReason::Network);
    assert_eq!(
        broken
            .requests()
            .iter()
            .filter(|request| request.if_range.is_some())
            .count(),
        3
    );
    engine.shutdown().await?;
    Ok(())
}
