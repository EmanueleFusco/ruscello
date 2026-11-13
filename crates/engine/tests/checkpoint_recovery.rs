use std::{error::Error, fs, num::NonZeroUsize, path::Path, time::Duration};

use ruscello_core::{Download, DownloadId, Source, Status};
use ruscello_engine::{RECOVERY_FOLDER, recover_queue};
use ruscello_http::{
    CheckpointJob, DownloadOptions, DownloadProgress, HttpDownloader, HttpError, Url,
    scan_checkpoints,
};
use ruscello_storage::{Change, DatabaseRecovery, QueueEntry, Storage};
use ruscello_testkit::{FileSpec, TestServer};
use tokio::{sync::watch, time::timeout};

type TestResult = Result<(), Box<dyn Error>>;
const SIZE: u64 = 2 * 1024 * 1024;

async fn interrupt(
    http: &HttpDownloader,
    url: &Url,
    working: &Path,
    job: Option<CheckpointJob>,
) -> Result<u64, Box<dyn Error>> {
    fs::create_dir_all(working)?;
    let (progress, mut changes) = watch::channel(DownloadProgress::default());
    let options = DownloadOptions {
        directory: working.to_owned(),
        connections: NonZeroUsize::MIN,
        progress: Some(progress),
        checkpoint_job: job,
        ..DownloadOptions::default()
    };
    let cancellation = options.cancellation.clone();
    let transfer = http.download(url, options);
    tokio::pin!(transfer);
    tokio::select! {
        result = &mut transfer => return Err(format!("completed before interruption: {result:?}").into()),
        result = timeout(Duration::from_secs(20), changes.wait_for(|progress| progress.completed > 0)) => {
            result??;
        }
    }
    cancellation.cancel();
    assert!(matches!(transfer.await, Err(HttpError::Cancelled)));
    let checkpoints = scan_checkpoints(working)?;
    assert_eq!(checkpoints.len(), 1);
    let persisted = checkpoints[0].downloaded_bytes;
    assert!(persisted > 0 && persisted < SIZE);
    Ok(persisted)
}

#[tokio::test]
async fn reconciles_over_and_under_reported_bytes_from_a_legacy_checkpoint() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(7, SIZE).throttle_per_conn(SIZE / 2));
    let url = Url::parse(file.url())?;
    let temp = tempfile::tempdir()?;
    let working = temp.path().join("downloads");
    let persisted = interrupt(&HttpDownloader::new()?, &url, &working, None).await?;

    let path = temp.path().join("ruscello.db");
    let (storage, _) = Storage::open(path.clone()).await?;
    for recorded in [0, SIZE] {
        let mut download = Download::new(
            DownloadId(1),
            Source::Http {
                url: url.to_string(),
            },
            working.clone(),
        );
        download.transition(Status::Downloading)?;
        download.downloaded_bytes = recorded;
        let entry = QueueEntry {
            download,
            position: 0,
            working_directory: working.clone(),
            torrent: None,
        };
        storage
            .apply(vec![Change::SaveDownload(Box::new(entry))])
            .await?;

        let snapshot = recover_queue(&storage, DatabaseRecovery::Unchanged, vec![]).await?;
        assert_eq!(snapshot.queue[0].download.downloaded_bytes, persisted);
        assert_eq!(snapshot.queue[0].download.total_bytes, Some(SIZE));
        assert_eq!(snapshot.queue[0].download.status(), Status::Queued);
        assert_eq!(storage.snapshot().await?.queue, snapshot.queue);
    }

    fs::write(
        working.join("payload.bin.rscpart.ctl"),
        b"invalid checkpoint",
    )?;
    let snapshot = recover_queue(&storage, DatabaseRecovery::Unchanged, vec![]).await?;
    assert_eq!(snapshot.queue[0].download.downloaded_bytes, 0);
    assert!(working.join("payload.bin.rscpart").is_file());
    storage.close().await?;
    Ok(())
}

#[tokio::test]
async fn rebuilds_a_corrupt_queue_from_a_managed_checkpoint_and_resumes() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(77, SIZE).throttle_per_conn(SIZE / 2));
    let url = Url::parse(file.url())?;
    let temp = tempfile::tempdir()?;
    let destination = temp.path().join("downloads");
    let working = destination.join(RECOVERY_FOLDER).join("21");
    let job = CheckpointJob {
        id: 21,
        destination: destination.clone(),
    };
    let http = HttpDownloader::new()?;
    let persisted = interrupt(&http, &url, &working, Some(job.clone())).await?;
    let path = temp.path().join("ruscello.db");
    fs::write(&path, b"corrupt primary database")?;
    fs::write(temp.path().join("ruscello.db.bak"), b"corrupt backup")?;

    let (storage, recovery) = Storage::open(path.clone()).await?;
    assert_eq!(recovery, DatabaseRecovery::Rebuilt);
    storage.close().await?;
    // A second startup must finish reconciliation if the first stopped after opening SQLite.
    let (storage, recovery) = Storage::open(path).await?;
    assert_eq!(recovery, DatabaseRecovery::Rebuilt);
    let snapshot = recover_queue(&storage, recovery, vec![destination.clone()]).await?;
    assert_eq!(snapshot.queue.len(), 1);
    let entry = &snapshot.queue[0];
    assert_eq!(entry.download.id, DownloadId(21));
    assert_eq!(entry.download.status(), Status::Paused);
    assert_eq!(entry.download.destination, destination);
    assert_eq!(entry.download.downloaded_bytes, persisted);
    assert_eq!(entry.working_directory, working);
    assert_eq!(storage.snapshot().await?.queue, snapshot.queue);

    let path = http
        .download(
            &url,
            DownloadOptions {
                directory: working,
                connections: NonZeroUsize::MIN,
                checkpoint_job: Some(job),
                ..DownloadOptions::default()
            },
        )
        .await?;
    assert_eq!(fs::read(path)?, file.payload().bytes(0..SIZE));
    assert!(file.requests().iter().any(|record| {
        record.if_range.is_some()
            && record.range.as_deref() == Some(&format!("bytes={persisted}-{}", SIZE - 1))
    }));
    storage.close().await?;
    Ok(())
}
