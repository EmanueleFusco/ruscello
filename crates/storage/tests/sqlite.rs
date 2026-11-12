use std::{error::Error, fs};

use ruscello_core::{Category, CategoryId, Download, DownloadId, Package, PackageId, Source};
use ruscello_storage::{Change, DatabaseRecovery, QueueEntry, Storage, StorageError};
use rusqlite::Connection;
use serde_json::json;

type TestResult = Result<(), Box<dyn Error>>;

#[tokio::test]
async fn upgrades_existing_database_and_round_trips_queue_metadata() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ruscello.db");
    {
        let connection = Connection::open(&path)?;
        connection.execute_batch(
            "CREATE TABLE downloads (id INTEGER PRIMARY KEY, position INTEGER NOT NULL, record TEXT NOT NULL);
             CREATE INDEX downloads_position ON downloads(position, id);
             CREATE TABLE settings (key TEXT PRIMARY KEY, record TEXT NOT NULL);
             CREATE TABLE queue_recovery (id INTEGER PRIMARY KEY CHECK(id=1), mode TEXT NOT NULL);
             INSERT INTO settings VALUES ('language', '\"it\"');
             PRAGMA user_version=1;",
        )?;
    }
    let (storage, recovery) = Storage::open(path.clone()).await?;
    assert_eq!(recovery, DatabaseRecovery::Unchanged);
    assert!(matches!(
        Storage::open(path.clone()).await,
        Err(StorageError::Busy)
    ));
    let category = Category {
        id: CategoryId(2),
        name: "Work".into(),
        destination: None,
    };
    let package = Package {
        id: PackageId(3),
        name: "Reports".into(),
    };
    let mut download = Download::new(
        DownloadId(5),
        Source::Http {
            url: "https://example.test/report.pdf".into(),
        },
        temp.path().join("Relazioni – 2026"),
    );
    download.package = Some(package.id);
    download.category = Some(category.id);
    let entry = QueueEntry {
        download,
        position: 1,
        working_directory: temp.path().to_owned(),
    };
    storage
        .apply(vec![
            Change::SaveDownload(Box::new(entry.clone())),
            Change::SavePackage(package.clone()),
            Change::SaveCategory(category.clone()),
            Change::SaveSetting {
                key: "max_downloads".into(),
                value: json!(3),
            },
        ])
        .await?;

    let snapshot = storage.snapshot().await?;
    assert_eq!(snapshot.queue, [entry]);
    assert_eq!(snapshot.packages, [package]);
    assert_eq!(snapshot.categories, [category]);
    assert_eq!(snapshot.settings.get("language"), Some(&json!("it")));
    assert_eq!(snapshot.settings.get("max_downloads"), Some(&json!(3)));
    storage.close().await?;
    let connection = Connection::open(&path)?;
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let mode: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    assert_eq!(version, 2);
    assert_eq!(mode, "wal");
    assert!(temp.path().join("ruscello.db.bak").is_file());
    Ok(())
}

#[tokio::test]
async fn isolates_failed_writes_and_restores_last_committed_backup() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ruscello.db");
    let (storage, recovery) = Storage::open(path.clone()).await?;
    assert_eq!(recovery, DatabaseRecovery::Created);
    let (invalid, valid) = tokio::join!(
        storage.apply(vec![
            Change::SaveSetting {
                key: "invalid".into(),
                value: json!(1)
            },
            Change::RemoveDownload(DownloadId(u64::MAX)),
        ]),
        storage.apply(vec![Change::SaveSetting {
            key: "valid".into(),
            value: json!(2)
        }]),
    );
    assert!(invalid.is_err());
    valid?;
    storage.close().await?;
    fs::write(&path, b"not a SQLite database")?;
    let (storage, recovery) = Storage::open(path).await?;
    assert_eq!(recovery, DatabaseRecovery::RestoredBackup);
    let snapshot = storage.snapshot().await?;
    assert_eq!(snapshot.settings.get("valid"), Some(&json!(2)));
    assert_eq!(snapshot.settings.get("invalid"), None);
    assert!(fs::read_dir(temp.path())?.any(|entry| {
        entry.is_ok_and(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("ruscello-corrupt-")
                && entry.path().join("ruscello.db").is_file()
        })
    }));
    storage.close().await?;
    Ok(())
}
