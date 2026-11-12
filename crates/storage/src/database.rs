use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, backup::Backup, params};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;

use crate::{Change, Command, DatabaseRecovery, MAILBOX, Reply, Snapshot, StorageError};

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE downloads (id INTEGER PRIMARY KEY, position INTEGER NOT NULL, record TEXT NOT NULL);
     CREATE INDEX downloads_position ON downloads(position, id);
     CREATE TABLE settings (key TEXT PRIMARY KEY, record TEXT NOT NULL);
     CREATE TABLE queue_recovery (id INTEGER PRIMARY KEY CHECK(id=1), mode TEXT NOT NULL);
     INSERT INTO queue_recovery VALUES (1, '\"Created\"');",
    "CREATE TABLE packages (id INTEGER PRIMARY KEY, record TEXT NOT NULL);
     CREATE TABLE categories (id INTEGER PRIMARY KEY, record TEXT NOT NULL);",
];

pub(super) fn run(
    path: &Path,
    mut receiver: mpsc::Receiver<Command>,
    ready: Reply<DatabaseRecovery>,
) {
    let (mut connection, lock, backup, recovery) = match open(path) {
        Ok(opened) => opened,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(recovery)).is_err() {
        return;
    }

    let mut next = None;
    while let Some(command) = next.take().or_else(|| receiver.blocking_recv()) {
        match command {
            Command::Read(reply) => {
                let _ = reply.send(snapshot(&connection));
            }
            Command::Write(changes, reply) => {
                let mut requests = vec![(changes, reply)];
                while requests.len() < MAILBOX {
                    match receiver.try_recv() {
                        Ok(Command::Write(changes, reply)) => requests.push((changes, reply)),
                        Ok(other) => {
                            next = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                write_batch(&mut connection, &backup, requests);
            }
            Command::Close(reply) => {
                let result = save_backup(&connection, &backup, None);
                drop(connection);
                drop(lock);
                let _ = reply.send(result);
                return;
            }
        }
    }
}

fn open(path: &Path) -> Result<(Connection, File, PathBuf, DatabaseRecovery), StorageError> {
    let directory = parent(path);
    fs::create_dir_all(directory)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(with_suffix(path, ".lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Err(StorageError::Busy),
        Err(fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    let backup = with_suffix(path, ".bak");
    let (mut connection, recovery) = if path.try_exists()? {
        match checked_open(path, OpenFlags::SQLITE_OPEN_READ_WRITE) {
            Ok(connection) => (connection, DatabaseRecovery::Unchanged),
            Err(error) if is_corruption(&error) => {
                // Verify any fallback before touching the primary database.
                let fallback = open_backup(&backup)?;
                quarantine(path)?;
                recover(path, fallback)?
            }
            Err(error) => return Err(error),
        }
    } else {
        if with_suffix(path, "-wal").try_exists()? || with_suffix(path, "-shm").try_exists()? {
            quarantine(path)?;
        }
        match open_backup(&backup)? {
            Some(fallback) => recover(path, Some(fallback))?,
            None => (Connection::open(path)?, DatabaseRecovery::Created),
        }
    };
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    migrate(&mut connection)?;
    let recovery = match recovery {
        DatabaseRecovery::Unchanged => pending_recovery(&connection)?,
        DatabaseRecovery::Rebuilt => {
            set_recovery(&connection, recovery)?;
            recovery
        }
        _ => recovery,
    };
    save_backup(&connection, &backup, None)?;
    tracing::debug!(?recovery, "Opened the queue database");
    Ok((connection, lock, backup, recovery))
}

fn checked_open(path: &Path, flags: OpenFlags) -> Result<Connection, StorageError> {
    let connection = Connection::open_with_flags(path, flags)?;
    let check: String = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if check == "ok" {
        Ok(connection)
    } else {
        Err(StorageError::Corrupt)
    }
}

fn is_corruption(error: &StorageError) -> bool {
    matches!(
        error,
        StorageError::Corrupt
            | StorageError::Sql(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase,
                    ..
                },
                _
            ))
    )
}

fn open_backup(path: &Path) -> Result<Option<Connection>, StorageError> {
    if !path.try_exists()? {
        return Ok(None);
    }
    match checked_open(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => Ok(Some(connection)),
        Err(error) if is_corruption(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn recover(
    path: &Path,
    backup: Option<Connection>,
) -> Result<(Connection, DatabaseRecovery), StorageError> {
    if let Some(source) = backup {
        save_backup(&source, path, Some(DatabaseRecovery::RestoredBackup))?;
        Ok((Connection::open(path)?, DatabaseRecovery::RestoredBackup))
    } else {
        Ok((Connection::open(path)?, DatabaseRecovery::Rebuilt))
    }
}

fn quarantine(path: &Path) -> Result<(), StorageError> {
    let quarantine = tempfile::Builder::new()
        .prefix("ruscello-corrupt-")
        .tempdir_in(parent(path))?
        .keep();
    for file in [
        with_suffix(path, "-wal"),
        with_suffix(path, "-shm"),
        path.to_owned(),
    ] {
        let name = file
            .file_name()
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        match fs::rename(&file, quarantine.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    tracing::warn!("Quarantined a corrupt queue database");
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<(), StorageError> {
    let current: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let Some(pending) = MIGRATIONS.get(current as usize..) else {
        return Err(StorageError::NewerSchema(current));
    };
    for (index, migration) in pending.iter().enumerate() {
        let transaction = connection.transaction()?;
        transaction.execute_batch(migration)?;
        transaction.pragma_update(None, "user_version", current + index as u32 + 1)?;
        transaction.commit()?;
    }
    Ok(())
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn pending_recovery(connection: &Connection) -> Result<DatabaseRecovery, StorageError> {
    let record: Option<String> = connection
        .query_row("SELECT mode FROM queue_recovery", [], |row| row.get(0))
        .optional()?;
    match record {
        Some(record) => Ok(serde_json::from_str(&record)?),
        None => Ok(DatabaseRecovery::Unchanged),
    }
}

fn set_recovery(connection: &Connection, mode: DatabaseRecovery) -> Result<(), StorageError> {
    connection.execute(
        "INSERT OR REPLACE INTO queue_recovery VALUES (1, ?1)",
        [serde_json::to_string(&mode)?],
    )?;
    Ok(())
}

fn save_backup(
    source: &Connection,
    path: &Path,
    recovery: Option<DatabaseRecovery>,
) -> Result<(), StorageError> {
    let temporary = tempfile::NamedTempFile::new_in(parent(path))?.into_temp_path();
    let mut target = Connection::open(&temporary)?;
    {
        let backup = Backup::new(source, &mut target)?;
        backup.run_to_completion(128, Duration::ZERO, None)?;
    }
    target.pragma_update(None, "journal_mode", "DELETE")?;
    if let Some(recovery) = recovery {
        migrate(&mut target)?;
        set_recovery(&target, recovery)?;
    }
    drop(target);
    OpenOptions::new()
        .write(true)
        .open(&temporary)?
        .sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(parent(path))?.sync_all()?;
    Ok(())
}

fn write_batch(
    connection: &mut Connection,
    backup: &Path,
    requests: Vec<(Vec<Change>, Reply<()>)>,
) {
    let mut transaction = match connection.transaction() {
        Ok(transaction) => transaction,
        Err(error) => {
            let error = Arc::new(error.into());
            for (_, reply) in requests {
                let _ = reply.send(Err(StorageError::Batch(Arc::clone(&error))));
            }
            return;
        }
    };
    let replies: Vec<_> = requests
        .into_iter()
        .map(|(changes, reply)| {
            let result = (|| {
                let savepoint = transaction.savepoint()?;
                for change in &changes {
                    apply(&savepoint, change)?;
                }
                savepoint.commit()?;
                Ok(())
            })();
            (reply, result)
        })
        .collect();
    let committed = transaction.commit().map_err(|error| Arc::new(error.into()));
    if committed.is_ok()
        && replies.iter().any(|(_, result)| result.is_ok())
        && let Err(error) = save_backup(connection, backup, None)
    {
        // The authoritative transaction is already committed. Retry the backup on the next write.
        tracing::warn!(%error, "Could not refresh the queue database backup");
    }
    for (reply, result) in replies {
        let result = result.and_then(|()| {
            committed
                .as_ref()
                .map(|_| ())
                .map_err(|error| StorageError::Batch(Arc::clone(error)))
        });
        let _ = reply.send(result);
    }
}

fn apply(connection: &Connection, change: &Change) -> Result<(), StorageError> {
    match change {
        Change::SaveDownload(entry) => connection.execute(
            "INSERT INTO downloads(id, position, record) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET position=excluded.position, record=excluded.record",
            params![
                entry.download.id.0,
                entry.position,
                serde_json::to_string(entry)?
            ],
        ),
        Change::RemoveDownload(id) => {
            connection.execute("DELETE FROM downloads WHERE id=?1", [id.0])
        }
        Change::SavePackage(package) => connection.execute(
            "INSERT INTO packages(id, record) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET record=excluded.record",
            params![package.id.0, serde_json::to_string(package)?],
        ),
        Change::RemovePackage(id) => connection.execute("DELETE FROM packages WHERE id=?1", [id.0]),
        Change::SaveCategory(category) => connection.execute(
            "INSERT INTO categories(id, record) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET record=excluded.record",
            params![category.id.0, serde_json::to_string(category)?],
        ),
        Change::RemoveCategory(id) => {
            connection.execute("DELETE FROM categories WHERE id=?1", [id.0])
        }
        Change::SaveSetting { key, value } => connection.execute(
            "INSERT INTO settings(key, record) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET record=excluded.record",
            params![key, serde_json::to_string(value)?],
        ),
        Change::FinishCheckpointRecovery => connection.execute("DELETE FROM queue_recovery", []),
    }?;
    Ok(())
}

fn snapshot(connection: &Connection) -> Result<Snapshot, StorageError> {
    let mut settings = connection.prepare("SELECT key, record FROM settings")?;
    let mut rows = settings.query([])?;
    let mut snapshot = Snapshot {
        queue: records(
            connection,
            "SELECT record FROM downloads ORDER BY position, id",
        )?,
        packages: records(connection, "SELECT record FROM packages ORDER BY id")?,
        categories: records(connection, "SELECT record FROM categories ORDER BY id")?,
        ..Snapshot::default()
    };
    while let Some(row) = rows.next()? {
        let record: String = row.get(1)?;
        snapshot
            .settings
            .insert(row.get(0)?, serde_json::from_str(&record)?);
    }
    Ok(snapshot)
}

fn records<T: DeserializeOwned>(
    connection: &Connection,
    sql: &str,
) -> Result<Vec<T>, StorageError> {
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query([])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let record: String = row.get(0)?;
        result.push(serde_json::from_str(&record)?);
    }
    Ok(result)
}
