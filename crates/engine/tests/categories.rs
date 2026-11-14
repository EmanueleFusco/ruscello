use std::{collections::BTreeSet, error::Error, fs, num::NonZeroUsize, time::Duration};

use ruscello_core::{DefaultCategory, Download, DownloadId, FileType, Source, SourceKind, Status};
use ruscello_engine::{
    AddDownload, CategoryMatch, CategoryRule, EngineError, EngineEvent, EngineHandle, EngineOptions,
};
use ruscello_storage::{Change, QueueEntry, Storage};
use ruscello_testkit::{FileSpec, TestServer};
use tokio::time::timeout;

type TestResult = Result<(), Box<dyn Error>>;

fn request(url: &str) -> AddDownload {
    AddDownload::new(Source::Http { url: url.into() })
}

#[tokio::test]
async fn classifies_completed_entries_from_an_older_queue() -> TestResult {
    let temp = tempfile::tempdir()?;
    let options = EngineOptions::new(temp.path().join("ruscello.db"), temp.path().to_owned());
    let path = temp.path().join("picture.bin");
    fs::write(&path, b"\x89PNG\r\n\x1a\n")?;
    let mut download = Download::new(
        DownloadId(1),
        request("https://example.test/picture.bin").source,
        temp.path().to_owned(),
    );
    download.transition(Status::Downloading)?;
    download.transition(Status::Completed)?;
    download.completed_path = Some(path);
    let (storage, _) = Storage::open(options.database.clone()).await?;
    storage
        .apply(vec![Change::SaveDownload(Box::new(QueueEntry {
            download,
            position: 0,
            working_directory: temp.path().to_owned(),
            torrent: None,
            http: Default::default(),
        }))])
        .await?;
    storage.close().await?;
    let engine = EngineHandle::start(options.clone()).await?;
    assert_eq!(
        engine.snapshot().await?.downloads[0].file_type,
        Some(FileType::Images)
    );
    engine.shutdown().await?;
    let (storage, _) = Storage::open(options.database).await?;
    assert_eq!(
        storage.snapshot().await?.queue[0].download.file_type,
        Some(FileType::Images)
    );
    storage.close().await?;
    Ok(())
}

#[tokio::test]
async fn applies_ordered_rules_and_explicit_overrides_and_persists_settings() -> TestResult {
    let temp = tempfile::tempdir()?;
    let options = EngineOptions::new(
        temp.path().join("ruscello.db"),
        temp.path().join("downloads"),
    );
    let engine = EngineHandle::start(options.clone()).await?;
    let mut settings = engine.snapshot().await?.categories;
    assert_eq!(
        settings
            .categories
            .iter()
            .map(|policy| policy.category.name.as_str())
            .collect::<Vec<_>>(),
        ["Games", "Movies & TV", "Music", "Software", "Work"]
    );
    for policy in &mut settings.categories {
        policy.category.destination = Some(temp.path().join(&policy.category.name));
    }
    let games = DefaultCategory::Games.id();
    let music = DefaultCategory::Music.id();
    let software = DefaultCategory::Software.id();
    let work = DefaultCategory::Work.id();
    settings.rules = vec![
        CategoryRule {
            matcher: CategoryMatch::Domain("example.test".into()),
            category: games,
        },
        CategoryRule {
            matcher: CategoryMatch::Extension(".mp3".into()),
            category: music,
        },
        CategoryRule {
            matcher: CategoryMatch::Regex(r"demo-\d+".into()),
            category: software,
        },
        CategoryRule {
            matcher: CategoryMatch::Source(SourceKind::Http),
            category: work,
        },
    ];
    engine.set_categories(settings.clone()).await?;
    let mut invalid = settings.clone();
    invalid.rules[0].matcher = CategoryMatch::Regex("[".into());
    assert!(matches!(
        engine.set_categories(invalid).await,
        Err(EngineError::InvalidCategoryRule(1))
    ));

    let mut requests = [
        request("https://files.example.test/track.mp3"),
        request("https://other.test/track%20name.MP3"),
        request("https://other.test/demo-42"),
        request("https://notexample.test/file"),
        request("https://other.test/download"),
        request("https://example.test/movie"),
    ];
    for request in &mut requests {
        request.start_immediately = false;
    }
    requests[4].filename_hint = Some("Track.Mp3".into());
    requests[5].category = Some(work);
    requests[5].destination = Some(temp.path().join("explicit"));
    let ids = engine.add_many(requests.into()).await?;
    assert_eq!(ids.len(), 6);
    let downloads = engine.snapshot().await?.downloads;
    for (download, expected) in downloads
        .iter()
        .zip([games, music, software, work, music, work])
    {
        assert_eq!(download.category, Some(expected));
        assert_eq!(download.status(), Status::Paused);
        assert_eq!(download.file_type, None);
    }
    assert_eq!(downloads[1].destination, temp.path().join("Music"));
    assert_eq!(downloads[5].destination, temp.path().join("explicit"));
    engine.shutdown().await?;

    let engine = EngineHandle::start(options).await?;
    let snapshot = engine.snapshot().await?;
    assert_eq!(snapshot.categories, settings);
    assert_eq!(snapshot.downloads, downloads);
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn category_slots_limit_matching_jobs_without_blocking_other_categories() -> TestResult {
    timeout(Duration::from_secs(30), run_category_slots()).await?
}

async fn run_category_slots() -> TestResult {
    let server = TestServer::start().await?;
    let music = server.file(
        FileSpec::random(17, 256 * 1024)
            .filename("audio")
            .content_type("audio/flac")
            .throttle_per_conn(256 * 1024),
    );
    let other = server.file(FileSpec::random(18, 32 * 1024).filename("document.pdf"));
    let temp = tempfile::tempdir()?;
    let options = EngineOptions::new(
        temp.path().join("ruscello.db"),
        temp.path().join("downloads"),
    );
    let engine = EngineHandle::start(options.clone()).await?;
    let mut settings = engine.snapshot().await?.categories;
    let category = DefaultCategory::Music.id();
    let policy = settings
        .categories
        .iter_mut()
        .find(|policy| policy.category.id == category)
        .ok_or("missing default music category")?;
    let music_folder = temp.path().join("Music");
    policy.category.destination = Some(music_folder.clone());
    policy.limits.concurrent_downloads = Some(NonZeroUsize::MIN);
    engine.set_categories(settings).await?;
    let mut events = engine.subscribe();
    let mut track = request(music.url());
    track.category = Some(category);
    let ids = engine
        .add_many(vec![track.clone(), track, request(other.url())])
        .await?;
    let mut music_active = BTreeSet::new();
    let mut completed = BTreeSet::new();
    let mut overlapped = false;
    while completed.len() != 3 {
        if let EngineEvent::StatusChanged { id, status } = events.recv().await? {
            if id == ids[0] || id == ids[1] {
                if status.is_active() {
                    music_active.insert(id);
                } else {
                    music_active.remove(&id);
                }
            }
            if id == ids[2] && status.is_active() && !music_active.is_empty() {
                overlapped = true;
            }
            assert!(music_active.len() <= 1);
            assert!(!matches!(status, Status::Failed { .. } | Status::Paused));
            if status == Status::Completed {
                completed.insert(id);
            }
        }
    }
    assert!(overlapped);
    let snapshot = engine.snapshot().await?;
    for download in &snapshot.downloads[..2] {
        assert_eq!(download.category, Some(category));
        assert_eq!(download.file_type, Some(FileType::Audio));
        assert_eq!(
            download
                .completed_path
                .as_deref()
                .and_then(std::path::Path::parent),
            Some(music_folder.as_path())
        );
    }
    assert_eq!(snapshot.downloads[2].file_type, Some(FileType::Documents));
    engine.shutdown().await?;
    let restarted = EngineHandle::start(options).await?;
    assert_eq!(restarted.snapshot().await?.downloads, snapshot.downloads);
    restarted.shutdown().await?;
    Ok(())
}
