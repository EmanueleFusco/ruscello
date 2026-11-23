use std::{error::Error, time::Duration};

use ruscello_core::{DownloadId, FailureReason, Source, Status};
use ruscello_engine::{AddDownload, EngineEvent, EngineHandle, EngineOptions, Resolver};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

type TestResult = Result<(), Box<dyn Error>>;

async fn wait_for_completion(
    events: &mut broadcast::Receiver<EngineEvent>,
    target: DownloadId,
) -> Result<(Status, bool), Box<dyn Error>> {
    let result = tokio::time::timeout(Duration::from_secs(45), async {
        let mut processed = false;
        loop {
            if let EngineEvent::StatusChanged { id, status } = events.recv().await?
                && id == target
            {
                processed |= status == Status::PostProcessing;
                if matches!(status, Status::Completed | Status::Failed { .. }) {
                    return Ok::<_, broadcast::error::RecvError>((status, processed));
                }
            }
        }
    })
    .await??;
    Ok(result)
}

#[tokio::test]
#[ignore = "requires the public GitHub release asset"]
async fn github_release_manifest_completes_and_persists() -> TestResult {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("files");
    let mut options = EngineOptions::new(temp.path().join("queue.db"), output.clone());
    options.resolver = Some(Resolver::new(None)?);
    options.automation.prevent_sleep_when_active = false;
    options.automation.post_processing.auto_verify_checksums = true;
    let engine = EngineHandle::start(options.clone()).await?;
    let mut events = engine.subscribe();
    let id = engine
        .add(AddDownload::new(Source::Http {
            url: "https://github.com/EmanueleFusco/ruscello/releases/download/v1.0.0/SHA256SUMS"
                .into(),
        }))
        .await?;
    assert_eq!(
        wait_for_completion(&mut events, id).await?,
        (Status::Completed, true)
    );
    let published = std::fs::read(output.join("SHA256SUMS"))?;
    assert_eq!(published.len(), 1009);
    assert_eq!(
        hex::encode(Sha256::digest(published)),
        "c733eeb7478e035ee06ae36e178f30e4b992b5e4475e81f9da4db8ba4ff9b0eb"
    );
    engine.shutdown().await?;
    let restored = EngineHandle::start(options).await?;
    assert_eq!(
        restored.snapshot().await?.downloads[0].status(),
        Status::Completed
    );
    restored.shutdown().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the public Alpine minirootfs fixture"]
async fn unsupported_tar_links_preserve_the_verified_alpine_archive() -> TestResult {
    const CHECKSUM: &str = "18879884e35b0718f017a50ff85b5e6568279e97233fc42822229585feb2fa4d";
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("files");
    let mut options = EngineOptions::new(temp.path().join("queue.db"), output.clone());
    options.automation.prevent_sleep_when_active = false;
    options.automation.post_processing.auto_verify_checksums = true;
    options.automation.post_processing.auto_extract = true;
    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();
    let mut download = AddDownload::new(Source::Http {
        url: "https://dl-cdn.alpinelinux.org/alpine/v3.22/releases/x86_64/alpine-minirootfs-3.22.0-x86_64.tar.gz".into(),
    });
    download.checksum = Some(format!("sha256:{CHECKSUM}"));
    let id = engine.add(download).await?;
    assert_eq!(
        wait_for_completion(&mut events, id).await?,
        (
            Status::Failed {
                reason: FailureReason::ArchiveExtraction
            },
            true
        )
    );
    let published = std::fs::read(output.join("alpine-minirootfs-3.22.0-x86_64.tar.gz"))?;
    assert_eq!(hex::encode(Sha256::digest(published)), CHECKSUM);
    engine.shutdown().await?;
    Ok(())
}
