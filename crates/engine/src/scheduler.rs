use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use ruscello_core::{
    Download, DownloadId, FailureReason, FileType, Package, PackageId, Source, SourceKind, Status,
    TorrentSource,
};
use ruscello_http::{DownloadProgress, HostConnections, HttpDownloader, HttpError};
use ruscello_storage::{Change, DatabaseRecovery, QueueEntry, Snapshot, Storage};
use ruscello_torrent::{
    StartedTorrent, TorrentError, TorrentManager, TorrentPhase, TorrentProgressSnapshot,
    TorrentSessionTelemetry, TorrentSettings, inspect_torrent_bytes,
};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::{Id, JoinSet},
    time::{self, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

use crate::{
    AddDownload, CategorySettings, DiskPolicy, EngineError, EngineEvent, EngineLog, EngineOptions,
    EngineSnapshot, Limits, MediaEngine, MediaPhase, ProgressUpdate, SidecarSettings,
    TorrentRuntimeProgress,
    categories::CategoryBook,
    handle::{Command, LIMITS_KEY, Reply, TORRENT_SETTINGS_KEY, Update},
    media_bridge::{self, SIDECAR_SETTINGS_KEY},
    torrent_bridge,
    transfer::{self, Published, TransferError, source_url},
};

const PROGRESS_PERIOD: Duration = Duration::from_millis(250);
const TORRENT_PERSIST_INTERVAL: Duration = Duration::from_secs(5);
type Outcome = Result<Published, TransferError>;

pub(super) struct Scheduler {
    storage: Storage,
    recovery: DatabaseRecovery,
    destination: PathBuf,
    temporary_directory: Option<PathBuf>,
    torrent_data_dir: PathBuf,
    disk: DiskPolicy,
    categories: CategoryBook,
    limits: Limits,
    torrent_settings: TorrentSettings,
    temporary_bandwidth: Option<std::num::NonZeroU64>,
    events: broadcast::Sender<EngineEvent>,
    hosts: HostConnections,
    http: HttpDownloader,
    media: MediaEngine,
    sidecar_settings: SidecarSettings,
    resolver: crate::Resolver,
    verifications: JoinSet<(ruscello_http::Url, Result<(), crate::ResolveError>)>,
    pending_verifications: BTreeMap<String, Vec<Reply<()>>>,
    verification_cancel: CancellationToken,
    torrent_manager: Option<Arc<TorrentManager>>,
    last_telemetry: Option<TorrentSessionTelemetry>,
    telemetry_ticks: u8,
    queue: BTreeMap<DownloadId, QueueEntry>,
    packages: BTreeMap<PackageId, Package>,
    forced: BTreeSet<DownloadId>,
    active: BTreeMap<DownloadId, Active>,
    workers: JoinSet<Outcome>,
    active_torrents: BTreeMap<DownloadId, ActiveTorrent>,
    torrent_starts: JoinSet<(DownloadId, Result<StartedTorrent, TorrentError>)>,
    torrent_stops: JoinSet<(
        DownloadId,
        Option<TorrentProgressSnapshot>,
        Result<(), TorrentError>,
    )>,
    pending_torrent_stops: BTreeMap<DownloadId, (TorrentStopKind, Option<Reply<()>>)>,
}

struct Active {
    task: Id,
    host: String,
    session_version: u64,
    cancellation: CancellationToken,
    progress: watch::Receiver<DownloadProgress>,
    media_phase: Option<watch::Receiver<MediaPhase>>,
    stop: Option<(Stop, Reply<()>)>,
}

struct ActiveTorrent {
    cancellation: CancellationToken,
    start_in_flight: bool,
    is_seeding: bool,
    is_recheck: bool,
    last_saved_at: Instant,
    last_disk_check: Instant,
}

#[derive(Clone, Copy)]
enum Stop {
    Pause,
    Cancel { delete_files: bool },
}

#[derive(Clone, Copy)]
enum TorrentStopKind {
    Pause,
    Cancel { delete_files: bool },
    CompleteSeeding,
}

impl Scheduler {
    pub(super) async fn new(
        storage: Storage,
        snapshot: Snapshot,
        recovery: DatabaseRecovery,
        options: EngineOptions,
        categories: CategoryBook,
        events: broadcast::Sender<EngineEvent>,
        resolver: crate::Resolver,
    ) -> Result<Self, EngineError> {
        let limits = options.limits;
        let hosts = HostConnections::new(limits.connections_per_host);
        let http = HttpDownloader::with_host_connections(hosts.clone())?;
        http.global_bandwidth().set_rate(limits.bandwidth);
        let engine_data_dir = options.data_dir.clone().unwrap_or_else(|| {
            options
                .database
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf()
        });
        let media = MediaEngine::new(
            http.clone(),
            &engine_data_dir,
            options.sidecar_endpoints.clone().unwrap_or_default(),
        )?;
        let torrent_data_dir = engine_data_dir.join("torrent");
        let sidecar_settings = options.sidecars;
        let torrent_settings = options.torrent;
        let queue: BTreeMap<DownloadId, QueueEntry> = snapshot
            .queue
            .into_iter()
            .map(|entry| (entry.download.id, entry))
            .collect();
        let packages: BTreeMap<PackageId, Package> = snapshot
            .packages
            .into_iter()
            .map(|package| (package.id, package))
            .collect();

        let has_torrents = queue
            .values()
            .any(|entry| matches!(entry.download.source, Source::Torrent(_)));
        let torrent_manager = if options.eager_torrent_session || has_torrents {
            let effective = torrent_bridge::effective_settings(&torrent_settings, limits.bandwidth);
            match TorrentManager::new(
                torrent_data_dir.clone(),
                options.destination.clone(),
                effective,
            )
            .await
            {
                Ok(manager) => {
                    let known: Vec<(DownloadId, Option<String>)> = queue
                        .values()
                        .filter(|e| {
                            matches!(e.download.source, Source::Torrent(_))
                                && e.download.status() != Status::Completed
                        })
                        .map(|e| {
                            (
                                e.download.id,
                                e.torrent.as_ref().and_then(|t| t.info_hash.clone()),
                            )
                        })
                        .collect();
                    manager.reconcile(&known).await;
                    Some(Arc::new(manager))
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not initialize BitTorrent session on startup");
                    let _ = events.send(EngineEvent::Log {
                        id: None,
                        message: EngineLog::TorrentSessionInitFailed,
                    });
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            storage,
            recovery,
            destination: options.destination,
            temporary_directory: options.temporary_directory,
            torrent_data_dir,
            disk: options.disk,
            categories,
            limits,
            torrent_settings,
            temporary_bandwidth: None,
            events,
            hosts,
            http,
            media,
            sidecar_settings,
            resolver,
            verifications: JoinSet::new(),
            pending_verifications: BTreeMap::new(),
            verification_cancel: CancellationToken::new(),
            torrent_manager,
            last_telemetry: None,
            telemetry_ticks: 0,
            queue,
            packages,
            forced: BTreeSet::new(),
            active: BTreeMap::new(),
            workers: JoinSet::new(),
            active_torrents: BTreeMap::new(),
            torrent_starts: JoinSet::new(),
            torrent_stops: JoinSet::new(),
            pending_torrent_stops: BTreeMap::new(),
        })
    }

    pub(super) async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let mut interval = time::interval(PROGRESS_PERIOD);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut shutdown = None;
        let mut closing = false;
        let mut schedule = true;
        loop {
            if closing
                && self.workers.is_empty()
                && self.torrent_starts.is_empty()
                && self.torrent_stops.is_empty()
                && self.verifications.is_empty()
            {
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
                        self.verification_cancel.cancel();
                        for active in self.active.values() {
                            active.cancellation.cancel();
                        }
                        for active in self.active_torrents.values() {
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
                joined = self.torrent_starts.join_next(), if !self.torrent_starts.is_empty() => {
                    if let Some(joined) = joined {
                        match joined {
                            Ok((id, result)) => self.finish_torrent_start(id, result, closing).await,
                            Err(error) => {
                                tracing::warn!(%error, "Torrent start task terminated unexpectedly");
                            }
                        }
                        schedule = true;
                    }
                }
                joined = self.torrent_stops.join_next(), if !self.torrent_stops.is_empty() => {
                    if let Some(joined) = joined {
                        if let Ok((id, snapshot, result)) = joined {
                            self.finish_torrent_stop(id, snapshot, result).await;
                        }
                        schedule = true;
                    }
                }
                joined = self.verifications.join_next(), if !self.verifications.is_empty() => {
                    if let Some(Ok((url, result))) = joined {
                        self.finish_verification(url, result).await;
                        schedule = true;
                    }
                }
                _ = interval.tick(), if !closing && (!self.active.is_empty() || !self.active_torrents.is_empty() || self.torrent_manager.is_some()) => {
                    self.progress();
                    self.tick_torrents().await;
                }
            }
        }
        self.persist_torrents_on_shutdown().await;
        if let Some(manager) = self.torrent_manager.take() {
            manager.shutdown().await;
        }
        self.resolver.stop_browser_helper().await;
        let closed = self.storage.close().await.map_err(EngineError::from);
        if let Some(reply) = shutdown {
            let _ = reply.send(closed);
        }
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Add(groups, reply) => {
                let _ = reply.send(self.add(groups).await);
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
                let telemetry = match &self.torrent_manager {
                    Some(manager) => manager.session_telemetry().await,
                    None => TorrentSessionTelemetry::default(),
                };
                let _ = reply.send(Ok(EngineSnapshot {
                    downloads,
                    packages: self.packages.values().cloned().collect(),
                    limits: self.limits,
                    recovery: self.recovery,
                    categories: self.categories.settings(),
                    torrent: self.torrent_settings.clone(),
                    telemetry,
                    sidecars: self.sidecar_settings.clone(),
                }));
            }
            Command::TorrentManager(reply) => {
                let _ = reply.send(self.ensure_torrent_manager().await);
            }
            Command::MediaContext(reply) => {
                let _ = reply.send(Ok((self.media.clone(), self.sidecar_settings.clone())));
            }
            Command::Verify(id, locale, reply) => self.start_verification(id, locale, reply),
            Command::ResumeVerified(url, reply) => {
                let _ = reply.send(self.resume_verified(&url).await);
            }
            Command::Shutdown(_) => {}
        }
    }

    fn start_verification(
        &mut self,
        id: DownloadId,
        locale: crate::BrowserLocale,
        reply: Reply<()>,
    ) {
        let url = self.entry(id).and_then(|entry| {
            if entry.download.status() == Status::WaitingUser {
                source_url(&entry.download.source)
            } else {
                Err(EngineError::VerificationNotPending)
            }
        });
        let url = match url {
            Ok(url) => url,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let key = url.origin().ascii_serialization();
        if let Some(waiters) = self.pending_verifications.get_mut(&key) {
            waiters.push(reply);
            return;
        }
        self.pending_verifications.insert(key, vec![reply]);
        let resolver = self.resolver.clone();
        let cancellation = self.verification_cancel.child_token();
        self.verifications.spawn(async move {
            let result = resolver.verify(&url, locale, &cancellation).await;
            (url, result)
        });
    }

    async fn finish_verification(
        &mut self,
        url: ruscello_http::Url,
        result: Result<(), crate::ResolveError>,
    ) {
        let Some(mut waiters) = self
            .pending_verifications
            .remove(&url.origin().ascii_serialization())
        else {
            return;
        };
        let result = match result {
            Ok(()) => self.resume_verified(&url).await,
            Err(error) => Err(error.into()),
        };
        // The initiating caller receives the result. The verification window is shared.
        if let Some(first) = waiters.pop() {
            for waiter in waiters {
                let reply = match &result {
                    Ok(()) => Ok(()),
                    Err(EngineError::Resolver(crate::ResolveError::Cancelled)) => {
                        Err(crate::ResolveError::Cancelled.into())
                    }
                    Err(_) => {
                        Err(crate::ResolveError::Browser(crate::BrowserFailure::Unavailable).into())
                    }
                };
                let _ = waiter.send(reply);
            }
            let _ = first.send(result);
        }
    }

    async fn resume_verified(&mut self, url: &ruscello_http::Url) -> Result<(), EngineError> {
        let origin = url.origin();
        let mut resumed = Vec::new();
        for entry in self
            .queue
            .values()
            .filter(|entry| entry.download.status() == Status::WaitingUser)
        {
            if source_url(&entry.download.source).is_ok_and(|other| other.origin() == origin) {
                let mut entry = entry.clone();
                entry.download.transition(Status::Queued)?;
                resumed.push(entry);
            }
        }
        if !resumed.is_empty() {
            self.storage
                .apply(resumed.iter().map(save_entry).collect())
                .await?;
            for entry in resumed {
                self.record(entry);
            }
        }
        Ok(())
    }

    async fn add(
        &mut self,
        groups: Vec<(Option<String>, Vec<AddDownload>)>,
    ) -> Result<Vec<DownloadId>, EngineError> {
        if groups.iter().all(|(_, requests)| requests.is_empty()) {
            return Ok(Vec::new());
        }
        let mut last = self.packages.keys().next_back().map_or(0, |id| id.0);
        let mut packages = Vec::new();
        let mut requests = Vec::new();
        for (name, group) in groups {
            if group.is_empty() {
                continue;
            }
            let package = if let Some(name) = name {
                last = last
                    .checked_add(1)
                    .filter(|id| i64::try_from(*id).is_ok())
                    .ok_or(EngineError::Identifier)?;
                let id = PackageId(last);
                packages.push(Package { id, name });
                Some(id)
            } else {
                None
            };
            requests.extend(group.into_iter().map(|request| (package, request)));
        }
        let mut entries = Vec::with_capacity(requests.len());
        let mut ids = BTreeSet::new();
        let first = self
            .queue
            .values()
            .map(|entry| entry.position)
            .max()
            .map_or(0, |max| max.saturating_add(1));
        let mut cached_files = Vec::new();
        for (offset, (package, request)) in requests.into_iter().enumerate() {
            if let Some(checksum) = &request.checksum {
                crate::parse_checksum(checksum)?;
            }
            if !request.mirrors.is_empty() && request.checksum.is_none() {
                return Err(HttpError::MirrorChecksumRequired.into());
            }
            let http = ruscello_storage::HttpQueueState {
                output_filename: request.output_filename,
                mirrors: request
                    .mirrors
                    .iter()
                    .map(|url| {
                        transfer::source_url(&Source::Http {
                            url: url.to_string(),
                        })
                        .map(Into::into)
                    })
                    .collect::<Result<_, _>>()?,
            };
            let media_state = request.media.as_ref().map(media_bridge::to_queue_state);
            let id = self.allocate_id(&mut ids)?;
            let mut download = match request.source {
                Source::Http { .. } | Source::Hoster { .. } => {
                    let url = source_url(&request.source)?;
                    let category = self.categories.assign(
                        request.category,
                        request.source.kind(),
                        url.as_str(),
                        Some(&url),
                        request.filename_hint.as_deref(),
                    )?;
                    let destination = request
                        .destination
                        .as_deref()
                        .map(std::path::absolute)
                        .transpose()?
                        .or_else(|| category.and_then(|policy| policy.category.destination.clone()))
                        .unwrap_or_else(|| self.destination.clone());
                    let source = match request.source {
                        Source::Hoster { .. } => Source::Hoster { url: url.into() },
                        _ => Source::Http { url: url.into() },
                    };
                    let mut dl = Download::new(id, source, destination);
                    dl.category = category.map(|policy| policy.category.id);
                    dl.filename = request.filename_hint;
                    dl.total_bytes = request.size_hint;
                    dl
                }
                Source::Media { .. } => {
                    let parsed = source_url(&request.source)?;
                    let hint = request
                        .filename_hint
                        .clone()
                        .or_else(|| request.media.as_ref().and_then(|m| m.title.clone()));
                    let category = self.categories.assign(
                        request.category,
                        SourceKind::Media,
                        parsed.as_str(),
                        Some(&parsed),
                        hint.as_deref(),
                    )?;
                    let destination = request
                        .destination
                        .as_deref()
                        .map(std::path::absolute)
                        .transpose()?
                        .or_else(|| category.and_then(|policy| policy.category.destination.clone()))
                        .unwrap_or_else(|| self.destination.clone());
                    let mut dl =
                        Download::new(id, Source::Media { url: parsed.into() }, destination);
                    dl.category = category.map(|policy| policy.category.id);
                    dl.file_type = Some(
                        if media_bridge::is_audio_selector(
                            request
                                .media
                                .as_ref()
                                .and_then(|m| m.format_selector.as_deref()),
                        ) {
                            FileType::Audio
                        } else {
                            FileType::Video
                        },
                    );
                    dl.filename = hint;
                    dl.total_bytes = request.size_hint;
                    dl
                }
                Source::Torrent(torrent_source) => {
                    let inspected = request
                        .cached_torrent_bytes
                        .as_deref()
                        .map(inspect_torrent_bytes)
                        .transpose()?;
                    let name_hint = inspected
                        .as_ref()
                        .map(|i| i.name.as_str())
                        .or(request.filename_hint.as_deref());
                    let (target, parsed_url, derived_hint) =
                        torrent_bridge::category_target(&torrent_source, name_hint);
                    let category = self.categories.assign(
                        request.category,
                        SourceKind::Torrent,
                        &target,
                        parsed_url.as_ref(),
                        derived_hint.as_deref(),
                    )?;
                    let destination = request
                        .destination
                        .as_deref()
                        .map(std::path::absolute)
                        .transpose()?
                        .or_else(|| category.and_then(|policy| policy.category.destination.clone()))
                        .unwrap_or_else(|| self.destination.clone());
                    let mut dl = Download::new(id, Source::Torrent(torrent_source), destination);
                    dl.category = category.map(|policy| policy.category.id);
                    dl.file_type = Some(FileType::Torrents);
                    dl.filename = derived_hint;
                    if let Some(info) = &inspected {
                        let total = match &request.only_files {
                            Some(indices) => info
                                .files
                                .iter()
                                .filter(|f| indices.contains(&f.index))
                                .map(|f| f.length)
                                .sum(),
                            None => info.total_bytes,
                        };
                        dl.total_bytes = Some(total);
                    }
                    if let Some(bytes) = request.cached_torrent_bytes {
                        cached_files.push((id.0, bytes));
                    }
                    let torrent_state = ruscello_storage::TorrentQueueState {
                        info_hash: inspected.map(|i| i.info_hash),
                        output_folder: request.resolved_output_folder,
                        only_files: request.only_files,
                        uploaded_bytes: 0,
                        seed_elapsed_secs: 0,
                        seed_ratio_millis: request.seed_ratio_millis,
                        seed_time_limit_minutes: request.seed_time_limit_minutes,
                        initial_peers: request.initial_peers,
                    };
                    if !request.start_immediately {
                        dl.transition(Status::Paused)?;
                    }
                    let working_directory = transfer::working_directory(
                        self.temporary_directory
                            .as_deref()
                            .unwrap_or(&dl.destination),
                        id,
                    );
                    dl.package = package.or(request.package);
                    dl.tags = request.tags;
                    dl.priority = request.priority;
                    dl.max_connections = request.max_connections;
                    dl.bandwidth = request.bandwidth;
                    dl.checksum = request.checksum.map(String::into_boxed_str);
                    entries.push(QueueEntry {
                        download: dl,
                        position: first + offset as u64,
                        working_directory,
                        torrent: Some(torrent_state),
                        http: Default::default(),
                        media: None,
                    });
                    continue;
                }
            };

            let working_directory = transfer::working_directory(
                self.temporary_directory
                    .as_deref()
                    .unwrap_or(&download.destination),
                id,
            );
            download.package = package.or(request.package);
            download.tags = request.tags;
            download.priority = request.priority;
            download.max_connections = request.max_connections;
            download.bandwidth = request.bandwidth;
            download.checksum = request.checksum.map(String::into_boxed_str);
            if !request.start_immediately {
                download.transition(Status::Paused)?;
            }
            entries.push(QueueEntry {
                download,
                position: first + offset as u64,
                working_directory,
                torrent: None,
                http,
                media: media_state,
            });
        }
        for (id, bytes) in cached_files {
            if let Err(error) =
                torrent_bridge::write_cached_torrent(&self.torrent_data_dir, id, &bytes).await
            {
                tracing::debug!(id, %error, "Could not cache pre-inspected torrent metadata");
            }
        }
        let mut changes = Vec::with_capacity(entries.len() + packages.len());
        changes.extend(packages.iter().cloned().map(Change::SavePackage));
        changes.extend(entries.iter().map(save_entry));
        self.storage.apply(changes).await?;
        for package in packages {
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
                self.http
                    .global_bandwidth()
                    .set_rate(self.temporary_bandwidth.or(limits.bandwidth));
                self.sync_torrent_ratelimits().await;
                self.emit(EngineEvent::QueueChanged);
                Ok(())
            }
            Update::TemporaryBandwidth(rate) => {
                self.temporary_bandwidth = rate;
                self.http
                    .global_bandwidth()
                    .set_rate(rate.or(self.limits.bandwidth));
                self.sync_torrent_ratelimits().await;
                self.emit(EngineEvent::QueueChanged);
                Ok(())
            }
            Update::Categories(settings) => self.set_categories(settings).await,
            Update::ForceRecheck(id) => self.force_recheck_torrent(id).await,
            Update::SetTorrentFiles(id, only_files) => self.set_torrent_files(id, only_files).await,
            Update::TorrentSettings(settings) => self.set_torrent_settings(settings).await,
            Update::SidecarSettings(settings) => {
                self.storage
                    .apply(vec![Change::SaveSetting {
                        key: SIDECAR_SETTINGS_KEY.into(),
                        value: serde_json::to_value(&settings)?,
                    }])
                    .await?;
                self.sidecar_settings = settings;
                self.emit(EngineEvent::QueueChanged);
                Ok(())
            }
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
        if self.active_torrents.contains_key(&id) {
            return if self.pending_torrent_stops.contains_key(&id) {
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
        let active_downloading = self.active.len()
            + self
                .active_torrents
                .values()
                .filter(|active| !active.is_seeding)
                .count();
        let free = self
            .limits
            .concurrent_downloads
            .get()
            .saturating_sub(active_downloading);
        if free == 0 {
            return Ok(());
        }
        let mut candidates: Vec<_> = self
            .queue
            .values()
            .filter(|entry| {
                let id = entry.download.id;
                entry.download.status() == Status::Queued
                    && !self.active.contains_key(&id)
                    && !self.active_torrents.contains_key(&id)
                    && !self.pending_torrent_stops.contains_key(&id)
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
        for (id, active) in &self.active_torrents {
            if !active.is_seeding
                && let Some(category) = self.queue.get(id).and_then(|entry| entry.download.category)
            {
                *category_load.entry(category).or_insert(0) += 1;
            }
        }

        enum Launch {
            Http(QueueEntry, ruscello_http::Url, String),
            Media(QueueEntry, ruscello_http::Url, String),
            Torrent(QueueEntry, TorrentSource),
        }

        let mut launches = Vec::new();
        let mut rejected = Vec::new();
        for entry in candidates {
            match &entry.download.source {
                Source::Http { .. } | Source::Hoster { .. } => {
                    let Ok(url) = source_url(&entry.download.source) else {
                        let mut entry = entry.clone();
                        entry.download.transition(Status::Failed {
                            reason: FailureReason::UnsupportedSource,
                        })?;
                        rejected.push(entry);
                        continue;
                    };
                    let host = url
                        .host_str()
                        .ok_or(EngineError::InvalidUrl)?
                        .trim_end_matches('.')
                        .trim_start_matches("www.")
                        .to_owned();
                    let running = host_load.entry(host.clone()).or_default();
                    let hoster_cap = match self.resolver.limits(&url).await {
                        Ok(limits) => limits.parallel_downloads,
                        Err(error) => {
                            tracing::debug!(%error, "Could not read resolver limits");
                            None
                        }
                    };
                    let hoster_cap = if matches!(entry.download.source, Source::Hoster { .. })
                        && self.resolver.builtin(&url).is_none()
                    {
                        hoster_cap.or(Some(std::num::NonZeroUsize::MIN))
                    } else {
                        hoster_cap
                    };
                    if *running
                        >= hoster_cap
                            .unwrap_or(self.limits.connections_per_host)
                            .min(self.limits.connections_per_host)
                            .get()
                    {
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
                    let status = if matches!(entry.download.source, Source::Hoster { .. })
                        || self.resolver.builtin(&url).is_some()
                    {
                        Status::Resolving
                    } else {
                        Status::Downloading
                    };
                    entry.download.transition(status)?;
                    launches.push(Launch::Http(entry, url, host));
                }
                Source::Media { .. } => {
                    let Ok(url) = source_url(&entry.download.source) else {
                        let mut entry = entry.clone();
                        entry.download.transition(Status::Failed {
                            reason: FailureReason::UnsupportedSource,
                        })?;
                        rejected.push(entry);
                        continue;
                    };
                    let host = url
                        .host_str()
                        .ok_or(EngineError::InvalidUrl)?
                        .trim_end_matches('.')
                        .trim_start_matches("www.")
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
                    launches.push(Launch::Media(entry, url, host));
                }
                Source::Torrent(torrent_source) => {
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
                    let mut entry = entry.clone();
                    let initial_status = if matches!(torrent_source, TorrentSource::Magnet(_))
                        && !torrent_bridge::meta_file_path(
                            &self.torrent_data_dir,
                            entry.download.id.0,
                        )
                        .exists()
                    {
                        Status::Resolving
                    } else {
                        Status::Downloading
                    };
                    entry.download.transition(initial_status)?;
                    launches.push(Launch::Torrent(entry, torrent_source.clone()));
                }
            }
            if launches.len() == free {
                break;
            }
        }

        let needs_torrent_manager = launches
            .iter()
            .any(|launch| matches!(launch, Launch::Torrent(_, _)));
        let torrent_manager = if needs_torrent_manager {
            match self.ensure_torrent_manager().await {
                Ok(manager) => Some(manager),
                Err(error) => {
                    let reason = match &error {
                        EngineError::Torrent(err) => err.failure_reason(),
                        _ => FailureReason::Internal,
                    };
                    for launch in launches {
                        if let Launch::Torrent(mut entry, _) = launch {
                            let id = entry.download.id;
                            if entry.download.transition(Status::Failed { reason }).is_ok() {
                                let _ = self.save(entry).await;
                            }
                            self.forced.remove(&id);
                            self.emit(EngineEvent::Log {
                                id: Some(id),
                                message: EngineLog::DownloadFailed(reason),
                            });
                        }
                    }
                    return Ok(());
                }
            }
        } else {
            None
        };

        self.storage
            .apply(
                launches
                    .iter()
                    .map(|launch| match launch {
                        Launch::Http(entry, _, _)
                        | Launch::Media(entry, _, _)
                        | Launch::Torrent(entry, _) => save_entry(entry),
                    })
                    .chain(rejected.iter().map(save_entry))
                    .collect(),
            )
            .await?;
        for entry in rejected {
            self.record(entry);
        }
        for launch in launches {
            match launch {
                Launch::Http(entry, url, host) => {
                    let id = entry.download.id;
                    let cancellation = CancellationToken::new();
                    let (sender, progress) = watch::channel(DownloadProgress::default());
                    let session_version = self.resolver.session_version(&url);
                    let task = self.workers.spawn(Box::pin(transfer::run(
                        transfer::Context {
                            http: self.http.clone(),
                            media: self.media.clone(),
                            sidecars: self.sidecar_settings.clone(),
                            resolver: self.resolver.clone(),
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
                            session_version,
                            cancellation,
                            progress,
                            media_phase: None,
                            stop: None,
                        },
                    );
                    self.forced.remove(&id);
                    self.record(entry);
                }
                Launch::Media(entry, url, host) => {
                    let id = entry.download.id;
                    let cancellation = CancellationToken::new();
                    let (sender, progress) = watch::channel(DownloadProgress::default());
                    let (phase_sender, phase_receiver) = watch::channel(MediaPhase::Downloading);
                    let session_version = self.resolver.session_version(&url);
                    let task = self.workers.spawn(Box::pin(transfer::run_media(
                        transfer::Context {
                            http: self.http.clone(),
                            media: self.media.clone(),
                            sidecars: self.sidecar_settings.clone(),
                            resolver: self.resolver.clone(),
                            limits: self.limits,
                            bandwidth: self.categories.bucket(entry.download.category),
                            disk: self.disk.clone(),
                        },
                        entry.clone(),
                        url,
                        cancellation.clone(),
                        sender,
                        phase_sender,
                    )));
                    self.active.insert(
                        id,
                        Active {
                            task: task.id(),
                            host,
                            session_version,
                            cancellation,
                            progress,
                            media_phase: Some(phase_receiver),
                            stop: None,
                        },
                    );
                    self.forced.remove(&id);
                    self.record(entry);
                }
                Launch::Torrent(entry, torrent_source) => {
                    if let Some(ref manager) = torrent_manager {
                        self.spawn_torrent_task(manager.clone(), entry, torrent_source, false)
                            .await;
                    }
                }
            }
        }
        Ok(())
    }

    fn progress(&mut self) {
        let mut updates = Vec::new();
        for (id, active) in &mut self.active {
            if let Some(ref mut phase_rx) = active.media_phase
                && phase_rx.has_changed().is_ok_and(|changed| changed)
                && *phase_rx.borrow_and_update() == MediaPhase::PostProcessing
                && let Some(entry) = self.queue.get_mut(id)
                && entry.download.status() == Status::Downloading
            {
                let _ = entry.download.transition(Status::PostProcessing);
                let _ = self.events.send(EngineEvent::StatusChanged {
                    id: *id,
                    status: Status::PostProcessing,
                });
            }
            if !active.progress.has_changed().is_ok_and(|changed| changed) {
                continue;
            }
            let progress = *active.progress.borrow_and_update();
            if let Some(entry) = self.queue.get_mut(id) {
                if entry.download.status() == Status::Resolving
                    && (progress.total.is_some() || progress.completed != 0)
                {
                    let _ = entry.download.transition(Status::Downloading);
                    let _ = self.events.send(EngineEvent::StatusChanged {
                        id: *id,
                        status: Status::Downloading,
                    });
                }
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
        if self.pending_torrent_stops.contains_key(&id) {
            let _ = reply.send(Err(EngineError::DownloadBusy));
            return;
        }
        if let Some(active) = self.active.get_mut(&id) {
            if active.stop.is_some() {
                let _ = reply.send(Err(EngineError::DownloadBusy));
            } else {
                active.stop = Some((stop, reply));
                active.cancellation.cancel();
            }
            return;
        }
        if let Some(active) = self.active_torrents.get_mut(&id) {
            let kind = match stop {
                Stop::Pause => TorrentStopKind::Pause,
                Stop::Cancel { delete_files } => TorrentStopKind::Cancel { delete_files },
            };
            active.cancellation.cancel();
            if active.start_in_flight {
                self.pending_torrent_stops.insert(id, (kind, Some(reply)));
            } else {
                self.spawn_torrent_stop(id, kind, Some(reply)).await;
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
        let is_torrent = matches!(entry.download.source, Source::Torrent(_));
        match stop {
            Stop::Pause => {
                entry.download.transition(Status::Paused)?;
                self.save(entry).await
            }
            Stop::Cancel { delete_files } => {
                if is_torrent {
                    if let Some(manager) = &self.torrent_manager {
                        match manager.remove(id, delete_files).await {
                            Ok(()) | Err(TorrentError::NotFound) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    torrent_bridge::remove_cached_torrent(&self.torrent_data_dir, id.0).await;
                }
                if delete_files {
                    tokio::task::spawn_blocking(move || {
                        if let Some(path) = &entry.download.completed_path {
                            if is_torrent && path.is_dir() {
                                if path.exists() {
                                    std::fs::remove_dir_all(path)?;
                                }
                            } else {
                                transfer::discard_output(path)?;
                            }
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
                    Err(error)
                        if !error.cancelled()
                            && !error.verification_required()
                            && matches!(stop, Stop::Pause) =>
                    {
                        self.transfer_failed(entry, error, active.session_version)
                            .await
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
                        self.transfer_failed(entry, error, active.session_version)
                            .await
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
        session_version: u64,
    ) -> Result<(), EngineError> {
        if error.verification_required() {
            let id = entry.download.id;
            let url = source_url(&entry.download.source)?;
            let origin = url.origin();
            if self.resolver.session_version(&url) > session_version {
                entry.download.transition(Status::Queued)?;
                return self.save(entry).await;
            }
            let first = !self.queue.values().any(|other| {
                other.download.status() == Status::WaitingUser
                    && source_url(&other.download.source).is_ok_and(|url| url.origin() == origin)
            });
            entry.download.transition(Status::WaitingUser)?;
            self.save(entry).await?;
            if first {
                self.emit(EngineEvent::Log {
                    id: Some(id),
                    message: EngineLog::VerificationRequired,
                });
            }
            return Ok(());
        }
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

    async fn ensure_torrent_manager(&mut self) -> Result<Arc<TorrentManager>, EngineError> {
        if let Some(manager) = &self.torrent_manager {
            return Ok(manager.clone());
        }
        let effective = torrent_bridge::effective_settings(
            &self.torrent_settings,
            self.temporary_bandwidth.or(self.limits.bandwidth),
        );
        let manager = TorrentManager::new(
            self.torrent_data_dir.clone(),
            self.destination.clone(),
            effective,
        )
        .await?;
        let known: Vec<(DownloadId, Option<String>)> = self
            .queue
            .values()
            .filter(|e| {
                matches!(e.download.source, Source::Torrent(_))
                    && e.download.status() != Status::Completed
            })
            .map(|e| {
                (
                    e.download.id,
                    e.torrent.as_ref().and_then(|t| t.info_hash.clone()),
                )
            })
            .collect();
        manager.reconcile(&known).await;
        let telemetry = manager.session_telemetry().await;
        self.last_telemetry = Some(telemetry);
        self.emit(EngineEvent::TorrentTelemetry(telemetry));
        let arc = Arc::new(manager);
        self.torrent_manager = Some(arc.clone());
        Ok(arc)
    }

    async fn sync_torrent_ratelimits(&self) {
        if let Some(manager) = &self.torrent_manager {
            let effective = torrent_bridge::effective_settings(
                &self.torrent_settings,
                self.temporary_bandwidth.or(self.limits.bandwidth),
            );
            if let Err(error) = manager.apply_settings(effective).await {
                tracing::debug!(%error, "Failed to update live torrent rate limits");
            }
        }
    }

    async fn set_torrent_settings(&mut self, settings: TorrentSettings) -> Result<(), EngineError> {
        let effective = torrent_bridge::effective_settings(
            &settings,
            self.temporary_bandwidth.or(self.limits.bandwidth),
        );
        if let Some(manager) = &self.torrent_manager {
            manager.apply_settings(effective).await?;
        } else {
            let manager = TorrentManager::new(
                self.torrent_data_dir.clone(),
                self.destination.clone(),
                effective,
            )
            .await?;
            let known: Vec<(DownloadId, Option<String>)> = self
                .queue
                .values()
                .filter(|e| {
                    matches!(e.download.source, Source::Torrent(_))
                        && e.download.status() != Status::Completed
                })
                .map(|e| {
                    (
                        e.download.id,
                        e.torrent.as_ref().and_then(|t| t.info_hash.clone()),
                    )
                })
                .collect();
            manager.reconcile(&known).await;
            self.torrent_manager = Some(Arc::new(manager));
        }

        self.storage
            .apply(vec![Change::SaveSetting {
                key: TORRENT_SETTINGS_KEY.into(),
                value: serde_json::to_value(&settings)?,
            }])
            .await?;
        self.torrent_settings = settings;
        if let Some(manager) = &self.torrent_manager {
            let telemetry = manager.session_telemetry().await;
            self.last_telemetry = Some(telemetry);
            self.emit(EngineEvent::TorrentTelemetry(telemetry));
        }
        self.emit(EngineEvent::QueueChanged);
        Ok(())
    }

    async fn set_torrent_files(
        &mut self,
        id: DownloadId,
        only_files: Vec<usize>,
    ) -> Result<(), EngineError> {
        let mut entry = self.entry(id)?.clone();
        if !matches!(entry.download.source, Source::Torrent(_)) {
            return Err(EngineError::UnsupportedSource);
        }
        if let Some(manager) = &self.torrent_manager {
            match manager.set_only_files(id, only_files.clone()).await {
                Ok(()) | Err(TorrentError::NotFound) => {}
                Err(error) => return Err(error.into()),
            }
        }
        torrent_bridge::ensure_torrent_state(&mut entry).only_files = Some(only_files);
        self.save(entry).await?;
        self.emit(EngineEvent::QueueChanged);
        Ok(())
    }

    async fn force_recheck_torrent(&mut self, id: DownloadId) -> Result<(), EngineError> {
        if self.pending_torrent_stops.contains_key(&id) {
            return Err(EngineError::DownloadBusy);
        }
        let mut entry = self.entry(id)?.clone();
        let Source::Torrent(torrent_source) = entry.download.source.clone() else {
            return Err(EngineError::UnsupportedSource);
        };
        if let Some(active) = self.active_torrents.get(&id)
            && active.start_in_flight
        {
            return Err(EngineError::DownloadBusy);
        }
        let manager = self.ensure_torrent_manager().await?;

        if let Some(active) = self.active_torrents.remove(&id) {
            active.cancellation.cancel();
        }
        if matches!(
            entry.download.status(),
            Status::Paused | Status::Failed { .. }
        ) {
            entry.download.transition(Status::Queued)?;
        }
        if entry.download.status() != Status::Resolving {
            entry.download.transition(Status::Resolving)?;
        }
        self.save(entry.clone()).await?;
        self.spawn_torrent_task(manager, entry, torrent_source, true)
            .await;
        Ok(())
    }

    async fn spawn_torrent_task(
        &mut self,
        manager: Arc<TorrentManager>,
        entry: QueueEntry,
        source: TorrentSource,
        force_recheck: bool,
    ) {
        let id = entry.download.id;
        let cached = torrent_bridge::read_cached_torrent(&self.torrent_data_dir, id.0).await;
        let category_bw = entry
            .download
            .category
            .and_then(|cid| self.categories.get(cid))
            .and_then(|p| p.limits.bandwidth);
        let request =
            torrent_bridge::build_request(&entry, &source, cached, category_bw, force_recheck);
        let cancellation = CancellationToken::new();
        let cancel_clone = cancellation.clone();
        self.torrent_starts.spawn(async move {
            let result = manager.start_or_resume(request, &cancel_clone).await;
            (id, result)
        });
        self.active_torrents.insert(
            id,
            ActiveTorrent {
                cancellation,
                start_in_flight: true,
                is_seeding: false,
                is_recheck: force_recheck,
                last_saved_at: Instant::now(),
                last_disk_check: Instant::now(),
            },
        );
        self.forced.remove(&id);
        self.record(entry);
    }

    async fn finish_torrent_start(
        &mut self,
        id: DownloadId,
        result: Result<StartedTorrent, TorrentError>,
        closing: bool,
    ) {
        if let Some(active) = self.active_torrents.get_mut(&id) {
            active.start_in_flight = false;
            active.is_recheck = false;
        }
        let pending_stop = self.pending_torrent_stops.remove(&id);
        let Some(mut entry) = self.queue.get(&id).cloned() else {
            if let Some((_, Some(reply))) = pending_stop {
                let _ = reply.send(Err(EngineError::UnknownDownload));
            }
            self.active_torrents.remove(&id);
            return;
        };

        match result {
            Ok(started) => {
                if let Err(error) = torrent_bridge::write_cached_torrent(
                    &self.torrent_data_dir,
                    id.0,
                    &started.torrent_bytes,
                )
                .await
                {
                    tracing::debug!(id = id.0, %error, "Could not persist resolved .torrent bytes");
                }
                entry.download.filename = Some(started.name);
                entry.download.total_bytes = Some(started.total_bytes);
                entry.download.completed_path = Some(started.completed_path);
                entry.download.file_type = Some(FileType::Torrents);
                let ts = torrent_bridge::ensure_torrent_state(&mut entry);
                ts.info_hash = Some(started.info_hash);
                ts.output_folder = Some(started.output_folder);
                ts.only_files = started.only_files;
                if let Err(error) = self.save(entry).await {
                    self.storage_error(Some(id), &error);
                }

                if let Some((kind, reply)) = pending_stop {
                    self.spawn_torrent_stop(id, kind, reply).await;
                    return;
                }
                if let Some(manager) = self.torrent_manager.as_ref().cloned()
                    && let Ok(snap) = manager.poll_progress(id)
                    && let Err(error) = self.apply_torrent_snapshot(id, snap).await
                {
                    self.storage_error(Some(id), &error);
                }
            }
            Err(error) => {
                self.active_torrents.remove(&id);
                if let Some((kind, reply)) = pending_stop {
                    let res = match kind {
                        TorrentStopKind::Pause => self.apply_stop(entry, Stop::Pause).await,
                        TorrentStopKind::Cancel { delete_files } => {
                            self.apply_stop(entry, Stop::Cancel { delete_files }).await
                        }
                        TorrentStopKind::CompleteSeeding => Ok(()),
                    };
                    if let Some(reply) = reply {
                        let _ = reply.send(res);
                    }
                    return;
                }
                if matches!(error, TorrentError::Cancelled) && closing {
                    return;
                }
                tracing::warn!(id = id.0, %error, "Torrent start failed");
                let reason = error.failure_reason();
                if let Err(err) = self.fail(entry, reason).await {
                    self.storage_error(Some(id), &err);
                }
            }
        }
    }

    async fn spawn_torrent_stop(
        &mut self,
        id: DownloadId,
        kind: TorrentStopKind,
        reply: Option<Reply<()>>,
    ) {
        self.pending_torrent_stops.insert(id, (kind, reply));
        let Some(manager) = self.torrent_manager.as_ref().cloned() else {
            self.finish_torrent_stop(id, None, Ok(())).await;
            return;
        };
        self.torrent_stops.spawn(async move {
            let snap = manager.poll_progress(id).ok();
            let result = match kind {
                TorrentStopKind::Pause => manager.pause(id).await,
                TorrentStopKind::Cancel { delete_files } => manager.remove(id, delete_files).await,
                TorrentStopKind::CompleteSeeding => manager.remove(id, false).await,
            };
            (id, snap, result)
        });
    }

    async fn finish_torrent_stop(
        &mut self,
        id: DownloadId,
        snapshot: Option<TorrentProgressSnapshot>,
        result: Result<(), TorrentError>,
    ) {
        self.active_torrents.remove(&id);
        let Some((kind, reply)) = self.pending_torrent_stops.remove(&id) else {
            return;
        };
        let outcome = self
            .apply_torrent_stop_result(id, kind, snapshot, result)
            .await;
        if let Some(reply) = reply {
            let _ = reply.send(outcome);
        } else if let Err(error) = outcome {
            self.storage_error(Some(id), &error);
        }
    }

    async fn apply_torrent_stop_result(
        &mut self,
        id: DownloadId,
        kind: TorrentStopKind,
        snapshot: Option<TorrentProgressSnapshot>,
        result: Result<(), TorrentError>,
    ) -> Result<(), EngineError> {
        let mut entry = self.entry(id)?.clone();
        if let Some(snap) = snapshot {
            entry.download.downloaded_bytes = snap.downloaded_bytes;
            if let Some(total) = snap.total_bytes {
                entry.download.total_bytes = Some(total);
            }
            if let Some(name) = snap.name {
                entry.download.filename = Some(name);
            }
            if let Some(path) = snap.completed_path {
                entry.download.completed_path = Some(path);
            }
            let ts = torrent_bridge::ensure_torrent_state(&mut entry);
            if snap.info_hash.is_some() {
                ts.info_hash = snap.info_hash;
            }
            if snap.output_folder.is_some() {
                ts.output_folder = snap.output_folder;
            }
            if snap.only_files.is_some() {
                ts.only_files = snap.only_files;
            }
            ts.uploaded_bytes = snap.uploaded_bytes;
            ts.seed_elapsed_secs = snap.seed_elapsed_secs;
        }
        match result {
            Ok(()) | Err(TorrentError::NotFound) => match kind {
                TorrentStopKind::Pause => self.apply_stop(entry, Stop::Pause).await,
                TorrentStopKind::Cancel { delete_files } => {
                    torrent_bridge::remove_cached_torrent(&self.torrent_data_dir, id.0).await;
                    if delete_files && let Some(path) = entry.download.completed_path.take() {
                        let _ = tokio::task::spawn_blocking(move || {
                            if path.is_dir() && path.exists() {
                                let _ = std::fs::remove_dir_all(&path);
                            } else {
                                let _ = transfer::discard_output(&path);
                            }
                        })
                        .await;
                    }
                    self.storage.apply(vec![Change::RemoveDownload(id)]).await?;
                    self.queue.remove(&id);
                    self.emit(EngineEvent::Removed(id));
                    Ok(())
                }
                TorrentStopKind::CompleteSeeding => {
                    entry.download.file_type = Some(FileType::Torrents);
                    entry.download.transition(Status::Completed)?;
                    self.save(entry).await
                }
            },
            Err(error) => Err(error.into()),
        }
    }

    async fn tick_torrents(&mut self) {
        let Some(manager) = self.torrent_manager.as_ref().cloned() else {
            return;
        };
        let ids: Vec<DownloadId> = self.active_torrents.keys().copied().collect();
        let mut http_updates = Vec::new();
        let mut torrent_updates = Vec::new();
        let mut seed_completed = Vec::new();
        let mut low_disk = Vec::new();
        let mut failed = Vec::new();

        for id in ids {
            // During recheck the old handle stays registered while its bitfield is being
            // removed. Only the recheck task can report the result of that transition.
            if self.pending_torrent_stops.contains_key(&id)
                || self
                    .active_torrents
                    .get(&id)
                    .is_some_and(|active| active.is_recheck)
            {
                continue;
            }
            let snap = match manager.poll_progress(id) {
                Ok(snap) => snap,
                Err(_) => continue,
            };

            if snap.seed_goal_reached {
                seed_completed.push(id);
            } else if let TorrentPhase::Failed(ref message) = snap.phase {
                tracing::warn!(id = id.0, error = %message, "Torrent entered failed state");
                failed.push(id);
                continue;
            } else if matches!(snap.phase, TorrentPhase::Downloading)
                && let Some(active) = self.active_torrents.get_mut(&id)
                && active.last_disk_check.elapsed() >= self.disk.interval()
            {
                active.last_disk_check = Instant::now();
                if let Some(entry) = self.queue.get(&id) {
                    let remaining = snap
                        .total_bytes
                        .unwrap_or(0)
                        .saturating_sub(snap.downloaded_bytes);
                    if let Err(err) = self.disk.check(&entry.download.destination, remaining)
                        && err.kind() == std::io::ErrorKind::StorageFull
                    {
                        low_disk.push(id);
                    }
                }
            }

            let metrics = (
                ProgressUpdate {
                    id,
                    progress: DownloadProgress {
                        completed: snap.downloaded_bytes,
                        total: snap.total_bytes,
                        resumable: true,
                        restarted: false,
                        waiting_for_network: false,
                        segments: None,
                    },
                },
                TorrentRuntimeProgress {
                    id,
                    download_rate_bps: snap.download_speed_bps,
                    upload_rate_bps: snap.upload_speed_bps,
                    uploaded_bytes: snap.uploaded_bytes,
                    seed_ratio: snap.seed_ratio,
                    seed_elapsed_secs: snap.seed_elapsed_secs,
                    peers_connected: snap.peers_connected,
                    peers_total: snap.peers_total,
                    seeders_connected: snap.seeders_connected,
                    eta_secs: snap.eta_secs,
                    piece_count: snap.piece_count,
                },
            );
            if let Err(error) = self.apply_torrent_snapshot(id, snap).await {
                self.storage_error(Some(id), &error);
            }
            http_updates.push(metrics.0);
            torrent_updates.push(metrics.1);
        }

        if !http_updates.is_empty() {
            self.emit(EngineEvent::Progress(http_updates));
        }
        if !torrent_updates.is_empty() {
            self.emit(EngineEvent::TorrentProgress(torrent_updates));
        }

        for id in seed_completed {
            self.spawn_torrent_stop(id, TorrentStopKind::CompleteSeeding, None)
                .await;
        }
        for id in low_disk {
            self.emit(EngineEvent::Log {
                id: Some(id),
                message: EngineLog::DiskSpaceLow,
            });
            self.spawn_torrent_stop(id, TorrentStopKind::Pause, None)
                .await;
        }
        for id in failed {
            self.active_torrents.remove(&id);
            let _ = manager.remove(id, false).await;
            if let Some(entry) = self.queue.get(&id).cloned()
                && let Err(error) = self.fail(entry, FailureReason::Network).await
            {
                self.storage_error(Some(id), &error);
            }
        }

        self.telemetry_ticks = self.telemetry_ticks.wrapping_add(1) % 4;
        if self.telemetry_ticks == 0 {
            let telemetry = manager.session_telemetry().await;
            if self.last_telemetry != Some(telemetry) {
                self.last_telemetry = Some(telemetry);
                self.emit(EngineEvent::TorrentTelemetry(telemetry));
            }
        }
    }

    async fn apply_torrent_snapshot(
        &mut self,
        id: DownloadId,
        snap: TorrentProgressSnapshot,
    ) -> Result<(), EngineError> {
        let Some(mut entry) = self.queue.get(&id).cloned() else {
            return Ok(());
        };
        if let Some(bytes) = snap.torrent_bytes.as_ref()
            && !torrent_bridge::meta_file_path(&self.torrent_data_dir, id.0).exists()
        {
            let _ = torrent_bridge::write_cached_torrent(&self.torrent_data_dir, id.0, bytes).await;
        }

        let target_status = match snap.phase {
            TorrentPhase::FetchingMetadata | TorrentPhase::Checking { .. } => Status::Resolving,
            TorrentPhase::Downloading => Status::Downloading,
            TorrentPhase::Seeding => Status::Seeding,
            TorrentPhase::Paused => entry.download.status(),
            TorrentPhase::Failed(_) => Status::Failed {
                reason: FailureReason::Network,
            },
        };
        if let Some(active) = self.active_torrents.get_mut(&id) {
            active.is_seeding = matches!(target_status, Status::Seeding);
        }

        let status_changed = target_status != entry.download.status();
        if status_changed {
            if matches!(
                entry.download.status(),
                Status::Paused | Status::Failed { .. }
            ) && target_status != Status::Paused
            {
                entry.download.transition(Status::Queued)?;
            }
            if entry.download.status() != target_status {
                entry.download.transition(target_status)?;
            }
        }

        entry.download.downloaded_bytes = snap.downloaded_bytes;
        if snap.total_bytes.is_some() {
            entry.download.total_bytes = snap.total_bytes;
        }
        if snap.name.is_some() {
            entry.download.filename = snap.name;
        }
        if snap.completed_path.is_some() {
            entry.download.completed_path = snap.completed_path;
        }
        entry.download.file_type = Some(FileType::Torrents);

        let ts = torrent_bridge::ensure_torrent_state(&mut entry);
        if snap.info_hash.is_some() {
            ts.info_hash = snap.info_hash;
        }
        if snap.output_folder.is_some() {
            ts.output_folder = snap.output_folder;
        }
        if snap.only_files.is_some() {
            ts.only_files = snap.only_files;
        }
        ts.uploaded_bytes = snap.uploaded_bytes;
        ts.seed_elapsed_secs = snap.seed_elapsed_secs;

        let should_persist = status_changed
            || self
                .active_torrents
                .get(&id)
                .is_some_and(|a| a.last_saved_at.elapsed() >= TORRENT_PERSIST_INTERVAL);
        if should_persist {
            if let Some(active) = self.active_torrents.get_mut(&id) {
                active.last_saved_at = Instant::now();
            }
            self.save(entry).await?;
        } else {
            self.record(entry);
        }
        Ok(())
    }

    async fn persist_torrents_on_shutdown(&mut self) {
        let Some(manager) = &self.torrent_manager else {
            return;
        };
        let ids: Vec<DownloadId> = self.active_torrents.keys().copied().collect();
        let mut changes = Vec::new();
        for id in ids {
            if let Ok(snap) = manager.poll_progress(id)
                && let Some(mut entry) = self.queue.get(&id).cloned()
            {
                entry.download.downloaded_bytes = snap.downloaded_bytes;
                if snap.total_bytes.is_some() {
                    entry.download.total_bytes = snap.total_bytes;
                }
                let ts = torrent_bridge::ensure_torrent_state(&mut entry);
                ts.uploaded_bytes = snap.uploaded_bytes;
                ts.seed_elapsed_secs = snap.seed_elapsed_secs;
                changes.push(save_entry(&entry));
                self.queue.insert(id, entry);
            }
        }
        if !changes.is_empty()
            && let Err(error) = self.storage.apply(changes).await
        {
            tracing::warn!(%error, "Could not flush torrent metrics during shutdown");
        }
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
