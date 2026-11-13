use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::Duration,
};

use ruscello_core::{Download, DownloadId, FailureReason, Package, PackageId, Source, Status};
use ruscello_http::{DownloadProgress, HostConnections, HttpDownloader};
use ruscello_storage::{Change, DatabaseRecovery, QueueEntry, Snapshot, Storage};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::{Id, JoinSet},
    time::{self, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

use crate::{
    AddDownload, CategorySettings, DiskPolicy, EngineError, EngineEvent, EngineLog, EngineOptions,
    EngineSnapshot, Limits, ProgressUpdate,
    categories::CategoryBook,
    handle::{Command, LIMITS_KEY, Reply, Update},
    transfer::{self, Published, TransferError, source_url},
};

const PROGRESS_PERIOD: Duration = Duration::from_millis(250);
type Outcome = Result<Published, TransferError>;

pub(super) struct Scheduler {
    storage: Storage,
    recovery: DatabaseRecovery,
    destination: PathBuf,
    temporary_directory: Option<PathBuf>,
    disk: DiskPolicy,
    categories: CategoryBook,
    limits: Limits,
    events: broadcast::Sender<EngineEvent>,
    hosts: HostConnections,
    http: HttpDownloader,
    queue: BTreeMap<DownloadId, QueueEntry>,
    packages: BTreeMap<PackageId, Package>,
    forced: BTreeSet<DownloadId>,
    active: BTreeMap<DownloadId, Active>,
    workers: JoinSet<Outcome>,
}

struct Active {
    task: Id,
    host: String,
    cancellation: CancellationToken,
    progress: watch::Receiver<DownloadProgress>,
    stop: Option<(Stop, Reply<()>)>,
}

#[derive(Clone, Copy)]
enum Stop {
    Pause,
    Cancel { delete_files: bool },
}

impl Scheduler {
    pub(super) fn new(
        storage: Storage,
        snapshot: Snapshot,
        recovery: DatabaseRecovery,
        options: EngineOptions,
        categories: CategoryBook,
        events: broadcast::Sender<EngineEvent>,
    ) -> Result<Self, EngineError> {
        let limits = options.limits;
        let hosts = HostConnections::new(limits.connections_per_host);
        let http = HttpDownloader::with_host_connections(hosts.clone())?;
        http.global_bandwidth().set_rate(limits.bandwidth);
        Ok(Self {
            storage,
            recovery,
            destination: options.destination,
            temporary_directory: options.temporary_directory,
            disk: options.disk,
            categories,
            limits,
            events,
            hosts,
            http,
            queue: snapshot
                .queue
                .into_iter()
                .map(|entry| (entry.download.id, entry))
                .collect(),
            packages: snapshot
                .packages
                .into_iter()
                .map(|package| (package.id, package))
                .collect(),
            forced: BTreeSet::new(),
            active: BTreeMap::new(),
            workers: JoinSet::new(),
        })
    }

    pub(super) async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let mut interval = time::interval(PROGRESS_PERIOD);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut shutdown = None;
        let mut closing = false;
        let mut schedule = true;
        loop {
            if closing && self.workers.is_empty() {
                break;
            }
            if schedule && !closing {
                schedule = false;
                if let Err(error) = self.dispatch().await {
                    self.storage_error(None, &error);
                }
            }
            tokio::select! {
                command = commands.recv(), if !closing => {
                    match command {
                        Some(Command::Shutdown(reply)) => {
                            shutdown = Some(reply);
                            closing = true;
                        }
                        None => closing = true,
                        Some(command) => {
                            self.command(command).await;
                            schedule = true;
                        }
                    }
                    if closing {
                        commands.close();
                        for active in self.active.values() {
                            active.cancellation.cancel();
                        }
                    }
                }
                joined = self.workers.join_next_with_id(), if !self.workers.is_empty() => {
                    if let Some(joined) = joined {
                        let (task, result) = match joined {
                            Ok((task, result)) => (task, result),
                            Err(error) => (error.id(), Err(error.into())),
                        };
                        self.finish(task, result, closing).await;
                        schedule = true;
                    }
                }
                _ = interval.tick(), if !closing && !self.active.is_empty() => self.progress(),
            }
        }
        let closed = self.storage.close().await.map_err(EngineError::from);
        if let Some(reply) = shutdown {
            let _ = reply.send(closed);
        }
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Add(requests, package, reply) => {
                let _ = reply.send(self.add(requests, package).await);
            }
            Command::Update(Update::Pause(id), reply) => self.stop(id, Stop::Pause, reply).await,
            Command::Update(Update::Cancel(id, delete_files), reply) => {
                self.stop(id, Stop::Cancel { delete_files }, reply).await;
            }
            Command::Update(update, reply) => {
                let _ = reply.send(self.update(update).await);
            }
            Command::Snapshot(reply) => {
                let downloads = self
                    .ordered()
                    .into_iter()
                    .map(|entry| entry.download.clone())
                    .collect();
                let _ = reply.send(Ok(EngineSnapshot {
                    downloads,
                    packages: self.packages.values().cloned().collect(),
                    limits: self.limits,
                    recovery: self.recovery,
                    categories: self.categories.settings(),
                }));
            }
            Command::Shutdown(_) => {}
        }
    }

    async fn add(
        &mut self,
        requests: Vec<AddDownload>,
        package_name: Option<String>,
    ) -> Result<Vec<DownloadId>, EngineError> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let package = package_name
            .map(|name| {
                let last = self.packages.keys().next_back().map_or(0, |id| id.0);
                let id = last
                    .checked_add(1)
                    .filter(|id| i64::try_from(*id).is_ok())
                    .ok_or(EngineError::Identifier)?;
                Ok::<_, EngineError>(Package {
                    id: PackageId(id),
                    name,
                })
            })
            .transpose()?;
        let mut entries = Vec::with_capacity(requests.len());
        let mut ids = BTreeSet::new();
        let first = self
            .queue
            .values()
            .map(|entry| entry.position)
            .max()
            .map_or(0, |max| max.saturating_add(1));
        for (offset, request) in requests.into_iter().enumerate() {
            let url = source_url(&request.source)?;
            if let Some(checksum) = &request.checksum {
                crate::parse_checksum(checksum)?;
            }
            let id = self.allocate_id(&mut ids)?;
            let category = self.categories.assign(
                request.category,
                request.source.kind(),
                &url,
                request.filename_hint.as_deref(),
            )?;
            let destination = request
                .destination
                .as_deref()
                .map(std::path::absolute)
                .transpose()?
                .or_else(|| category.and_then(|policy| policy.category.destination.clone()))
                .unwrap_or_else(|| self.destination.clone());
            let working_directory = transfer::working_directory(
                self.temporary_directory.as_deref().unwrap_or(&destination),
                id,
            );
            let mut download = Download::new(id, Source::Http { url: url.into() }, destination);
            download.package = package
                .as_ref()
                .map(|package| package.id)
                .or(request.package);
            download.category = category.map(|policy| policy.category.id);
            download.filename = request.filename_hint;
            download.tags = request.tags;
            download.priority = request.priority;
            download.max_connections = request.max_connections;
            download.checksum = request.checksum.map(String::into_boxed_str);
            if !request.start_immediately {
                download.transition(Status::Paused)?;
            }
            entries.push(QueueEntry {
                download,
                position: first + offset as u64,
                working_directory,
            });
        }
        let mut changes = Vec::with_capacity(entries.len() + 1);
        if let Some(package) = &package {
            changes.push(Change::SavePackage(package.clone()));
        }
        changes.extend(entries.iter().map(save_entry));
        self.storage.apply(changes).await?;
        if let Some(package) = package {
            self.packages.insert(package.id, package);
        }
        let mut ids = Vec::with_capacity(entries.len());
        for entry in entries {
            let id = entry.download.id;
            self.emit(EngineEvent::Added(Box::new(entry.download.clone())));
            self.queue.insert(id, entry);
            ids.push(id);
        }
        Ok(ids)
    }

    fn allocate_id(&self, assigned: &mut BTreeSet<DownloadId>) -> Result<DownloadId, EngineError> {
        for _ in 0..8 {
            let id = DownloadId(
                getrandom::u64().map_err(|_| EngineError::Identifier)? & i64::MAX as u64,
            );
            if id.0 != 0 && !self.queue.contains_key(&id) && assigned.insert(id) {
                return Ok(id);
            }
        }
        Err(EngineError::Identifier)
    }

    fn ordered(&self) -> Vec<&QueueEntry> {
        let mut entries: Vec<_> = self.queue.values().collect();
        entries.sort_unstable_by_key(|entry| (entry.position, entry.download.id));
        entries
    }

    async fn update(&mut self, update: Update) -> Result<(), EngineError> {
        match update {
            Update::Resume(id) => self.resume(id).await,
            Update::ForceStart(id) => {
                self.resume(id).await?;
                if !self.active.contains_key(&id) {
                    self.forced.insert(id);
                }
                Ok(())
            }
            Update::Priority(id, priority) => {
                let mut entry = self.entry(id)?.clone();
                entry.download.priority = priority;
                self.save(entry).await?;
                self.emit(EngineEvent::QueueChanged);
                Ok(())
            }
            Update::Move(id, before) => self.move_entry(id, before).await,
            Update::Limits(limits) => {
                self.storage
                    .apply(vec![Change::SaveSetting {
                        key: LIMITS_KEY.into(),
                        value: serde_json::to_value(limits)?,
                    }])
                    .await?;
                self.limits = limits;
                self.hosts.set_limit(limits.connections_per_host);
                self.http.global_bandwidth().set_rate(limits.bandwidth);
                self.emit(EngineEvent::QueueChanged);
                Ok(())
            }
            Update::Categories(settings) => self.set_categories(settings).await,
            Update::Pause(_) | Update::Cancel(_, _) => Err(EngineError::Stopped),
        }
    }

    async fn set_categories(&mut self, settings: CategorySettings) -> Result<(), EngineError> {
        let mut categories =
            tokio::task::spawn_blocking(move || CategoryBook::new(settings)).await??;
        let mut changes = categories.changes(Some(&self.categories))?;
        let mut uncategorized = Vec::new();
        for entry in self.queue.values() {
            if entry
                .download
                .category
                .is_some_and(|id| categories.get(id).is_none())
            {
                let mut entry = entry.clone();
                entry.download.category = None;
                changes.push(save_entry(&entry));
                uncategorized.push(entry);
            }
        }
        self.storage.apply(changes).await?;
        categories.reuse_buckets(&mut self.categories);
        self.categories = categories;
        for entry in uncategorized {
            self.record(entry);
        }
        self.emit(EngineEvent::QueueChanged);
        Ok(())
    }

    async fn resume(&mut self, id: DownloadId) -> Result<(), EngineError> {
        if let Some(active) = self.active.get(&id) {
            return if active.stop.is_some() {
                Err(EngineError::DownloadBusy)
            } else {
                Ok(())
            };
        }
        let mut entry = self.entry(id)?.clone();
        entry.download.transition(Status::Queued)?;
        self.save(entry).await
    }

    async fn move_entry(
        &mut self,
        id: DownloadId,
        before: Option<DownloadId>,
    ) -> Result<(), EngineError> {
        let entry = self.entry(id)?;
        if before == Some(id) {
            return Ok(());
        }
        let mut order = self.ordered();
        order.retain(|entry| entry.download.id != id);
        let position = match before {
            Some(before) => order
                .iter()
                .position(|entry| entry.download.id == before)
                .ok_or(EngineError::UnknownDownload)?,
            None => order.len(),
        };
        order.insert(position, entry);
        let changed: Vec<_> = order
            .into_iter()
            .enumerate()
            .filter_map(|(position, entry)| {
                if entry.position == position as u64 {
                    return None;
                }
                let mut entry = entry.clone();
                entry.position = position as u64;
                Some(entry)
            })
            .collect();
        self.storage
            .apply(changed.iter().map(save_entry).collect())
            .await?;
        for entry in changed {
            self.record(entry);
        }
        self.emit(EngineEvent::QueueChanged);
        Ok(())
    }

    async fn dispatch(&mut self) -> Result<(), EngineError> {
        let free = self
            .limits
            .concurrent_downloads
            .get()
            .saturating_sub(self.active.len());
        if free == 0 {
            return Ok(());
        }
        let mut candidates: Vec<_> = self
            .queue
            .values()
            .filter(|entry| {
                entry.download.status() == Status::Queued
                    && !self.active.contains_key(&entry.download.id)
            })
            .collect();
        candidates.sort_unstable_by_key(|entry| {
            let id = entry.download.id;
            (
                Reverse(self.forced.contains(&id)),
                Reverse(entry.download.priority),
                entry.position,
                id,
            )
        });
        let mut host_load = BTreeMap::<String, usize>::new();
        let mut category_load = BTreeMap::new();
        for (id, active) in &self.active {
            *host_load.entry(active.host.clone()).or_default() += 1;
            if let Some(category) = self.queue.get(id).and_then(|entry| entry.download.category) {
                *category_load.entry(category).or_insert(0) += 1;
            }
        }
        let mut launches = Vec::new();
        let mut rejected = Vec::new();
        for entry in candidates {
            let url = match source_url(&entry.download.source) {
                Ok(url) => url,
                Err(_) => {
                    let mut entry = entry.clone();
                    entry.download.transition(Status::Failed {
                        reason: FailureReason::UnsupportedSource,
                    })?;
                    rejected.push(entry);
                    continue;
                }
            };
            let host = url
                .host_str()
                .ok_or(EngineError::InvalidUrl)?
                .trim_end_matches('.')
                .to_owned();
            let running = host_load.entry(host.clone()).or_default();
            if *running >= self.limits.connections_per_host.get() {
                continue;
            }
            if let Some(category) = entry.download.category {
                let category_running = category_load.entry(category).or_insert(0);
                if self
                    .categories
                    .get(category)
                    .and_then(|policy| policy.limits.concurrent_downloads)
                    .is_some_and(|limit| *category_running >= limit.get())
                {
                    continue;
                }
                *category_running += 1;
            }
            *running += 1;
            let mut entry = entry.clone();
            entry.download.transition(Status::Downloading)?;
            launches.push((entry, url, host));
            if launches.len() == free {
                break;
            }
        }
        self.storage
            .apply(
                launches
                    .iter()
                    .map(|(entry, _, _)| save_entry(entry))
                    .chain(rejected.iter().map(save_entry))
                    .collect(),
            )
            .await?;
        for entry in rejected {
            self.record(entry);
        }
        for (entry, url, host) in launches {
            let id = entry.download.id;
            let cancellation = CancellationToken::new();
            let (sender, progress) = watch::channel(DownloadProgress::default());
            let task = self.workers.spawn(Box::pin(transfer::run(
                transfer::Context {
                    http: self.http.clone(),
                    limits: self.limits,
                    bandwidth: self.categories.bucket(entry.download.category),
                    disk: self.disk.clone(),
                },
                entry.clone(),
                url,
                cancellation.clone(),
                sender,
            )));
            self.active.insert(
                id,
                Active {
                    task: task.id(),
                    host,
                    cancellation,
                    progress,
                    stop: None,
                },
            );
            self.forced.remove(&id);
            self.record(entry);
        }
        Ok(())
    }

    fn progress(&mut self) {
        let mut updates = Vec::new();
        for (id, active) in &mut self.active {
            if !active.progress.has_changed().is_ok_and(|changed| changed) {
                continue;
            }
            let progress = *active.progress.borrow_and_update();
            if let Some(entry) = self.queue.get_mut(id) {
                entry.download.downloaded_bytes = progress.completed;
                entry.download.total_bytes = progress.total;
            }
            updates.push(ProgressUpdate { id: *id, progress });
        }
        if !updates.is_empty() {
            self.emit(EngineEvent::Progress(updates));
        }
    }

    async fn stop(&mut self, id: DownloadId, stop: Stop, reply: Reply<()>) {
        if let Some(active) = self.active.get_mut(&id) {
            if active.stop.is_some() {
                let _ = reply.send(Err(EngineError::DownloadBusy));
            } else {
                active.stop = Some((stop, reply));
                active.cancellation.cancel();
            }
            return;
        }
        let result = match self.entry(id).cloned() {
            Ok(entry) => self.apply_stop(entry, stop).await,
            Err(error) => Err(error),
        };
        let _ = reply.send(result);
    }

    async fn apply_stop(&mut self, mut entry: QueueEntry, stop: Stop) -> Result<(), EngineError> {
        let id = entry.download.id;
        self.forced.remove(&id);
        match stop {
            Stop::Pause => {
                entry.download.transition(Status::Paused)?;
                self.save(entry).await
            }
            Stop::Cancel { delete_files } => {
                if delete_files {
                    tokio::task::spawn_blocking(move || {
                        if let Some(path) = &entry.download.completed_path {
                            transfer::discard_output(path)?;
                        }
                        transfer::discard_work(&entry)
                    })
                    .await??;
                }
                self.storage.apply(vec![Change::RemoveDownload(id)]).await?;
                self.queue.remove(&id);
                self.emit(EngineEvent::Removed(id));
                Ok(())
            }
        }
    }

    async fn finish(&mut self, task: Id, outcome: Outcome, closing: bool) {
        let Some(id) = self
            .active
            .iter()
            .find_map(|(id, active)| (active.task == task).then_some(*id))
        else {
            return;
        };
        let Some(active) = self.active.remove(&id) else {
            return;
        };
        let Some(mut entry) = self.queue.get(&id).cloned() else {
            return;
        };
        let progress = *active.progress.borrow();
        if progress.total.is_some() || progress.completed > 0 {
            entry.download.downloaded_bytes = progress.completed;
            entry.download.total_bytes = progress.total;
        }
        match active.stop {
            Some((stop, reply)) => {
                let result = match outcome {
                    Ok(published) => {
                        complete_entry(&mut entry, published);
                        match stop {
                            Stop::Pause => self.complete(entry).await,
                            Stop::Cancel { .. } => self.apply_stop(entry, stop).await,
                        }
                    }
                    Err(error) if !error.cancelled() && matches!(stop, Stop::Pause) => {
                        self.transfer_failed(entry, error).await
                    }
                    Err(_) => self.apply_stop(entry, stop).await,
                };
                let _ = reply.send(result);
            }
            None => {
                let result = match outcome {
                    Ok(published) => {
                        complete_entry(&mut entry, published);
                        self.complete(entry).await
                    }
                    Err(error) if error.cancelled() && closing => Ok(()),
                    Err(error) => {
                        tracing::debug!(id = id.0, %error, "Download worker failed");
                        self.transfer_failed(entry, error).await
                    }
                };
                if let Err(error) = result {
                    self.storage_error(Some(id), &error);
                }
            }
        }
    }

    async fn transfer_failed(
        &mut self,
        mut entry: QueueEntry,
        error: TransferError,
    ) -> Result<(), EngineError> {
        if error.low_disk() {
            let id = entry.download.id;
            entry.download.transition(Status::Paused)?;
            self.save(entry).await?;
            self.emit(EngineEvent::Log {
                id: Some(id),
                message: EngineLog::DiskSpaceLow,
            });
            Ok(())
        } else {
            self.fail(entry, error.reason()).await
        }
    }

    async fn complete(&mut self, mut entry: QueueEntry) -> Result<(), EngineError> {
        let id = entry.download.id;
        entry.download.transition(Status::Completed)?;
        self.save(entry.clone()).await?;
        if let Err(error) =
            tokio::task::spawn_blocking(move || transfer::discard_work(&entry)).await?
        {
            tracing::debug!(id = id.0, kind = ?error.kind(), "Could not clean completed staging files");
            self.emit(EngineEvent::Log {
                id: Some(id),
                message: EngineLog::CleanupFailed,
            });
        }
        Ok(())
    }

    async fn fail(
        &mut self,
        mut entry: QueueEntry,
        reason: FailureReason,
    ) -> Result<(), EngineError> {
        let id = entry.download.id;
        entry.download.transition(Status::Failed { reason })?;
        self.save(entry).await?;
        self.emit(EngineEvent::Log {
            id: Some(id),
            message: EngineLog::DownloadFailed(reason),
        });
        Ok(())
    }

    fn entry(&self, id: DownloadId) -> Result<&QueueEntry, EngineError> {
        self.queue.get(&id).ok_or(EngineError::UnknownDownload)
    }

    async fn save(&mut self, entry: QueueEntry) -> Result<(), EngineError> {
        self.storage.apply(vec![save_entry(&entry)]).await?;
        self.record(entry);
        Ok(())
    }

    fn record(&mut self, entry: QueueEntry) {
        let id = entry.download.id;
        let status = entry.download.status();
        let previous = self.queue.insert(id, entry);
        if previous.is_none_or(|entry| entry.download.status() != status) {
            self.emit(EngineEvent::StatusChanged { id, status });
        }
    }

    fn storage_error(&self, id: Option<DownloadId>, error: &EngineError) {
        tracing::warn!(%error, "Could not update the download queue");
        self.emit(EngineEvent::Log {
            id,
            message: EngineLog::DatabaseWriteFailed,
        });
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
    }
}

fn save_entry(entry: &QueueEntry) -> Change {
    Change::SaveDownload(Box::new(entry.clone()))
}

fn complete_entry(entry: &mut QueueEntry, published: Published) {
    entry.download.filename = published
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    entry.download.completed_path = Some(published.path);
    entry.download.total_bytes = Some(published.bytes);
    entry.download.downloaded_bytes = published.bytes;
    entry.download.file_type = Some(published.file_type);
}
