use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    num::NonZeroU32,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

use bytes::Bytes;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, Api, ByteBufOwned, CloneToOwned,
    CreateTorrentOptions, DhtSessionConfig, ListOnlyResponse, ListenerMode, ListenerOptions,
    Magnet, ManagedTorrent, Session, SessionOptions, SessionPersistenceConfig, TorrentStatsState,
    ValidatedTorrentMetaV1Info, api::TorrentIdOrHash, create_torrent as rqbit_create_torrent,
    dht::DhtPersistenceConfig, limits::LimitsConfig, torrent_from_bytes,
};
use ruscello_core::{DownloadId, TorrentSource};
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::{
    AddTorrentRequest, TorrentDetailsSnapshot, TorrentError, TorrentFilePriority,
    TorrentFileProgress, TorrentInspectFile, TorrentInspection, TorrentPeerInfo, TorrentPeerState,
    TorrentPeerTransport, TorrentPhase, TorrentPieceMap, TorrentProgressSnapshot,
    TorrentSessionTelemetry, TorrentSettings, TorrentTrackerInfo, TorrentTrackerStatus,
    compute_ratio,
};

type ManagedTorrentHandle = Arc<ManagedTorrent>;

struct SessionHolder {
    session: Arc<Session>,
    api: Api,
    settings: TorrentSettings,
}

struct ManagedEntry {
    handle: Option<ManagedTorrentHandle>,
    info_hash: Option<String>,
    name: Option<String>,
    output_folder: Option<PathBuf>,
    only_files: Option<Vec<usize>>,
    torrent_bytes: Option<Vec<u8>>,
    prior_uploaded_bytes: u64,
    last_live_uploaded_bytes: u64,
    prior_seed_elapsed_secs: u64,
    seeding_started_at: Option<Instant>,
    seed_ratio_millis: Option<u32>,
    seed_time_limit_minutes: Option<u64>,
    is_single_file: Option<bool>,
    single_file_rel_path: Option<PathBuf>,
    is_resolving_metadata: bool,
    last_error: Option<String>,
    peer_rates: PeerRateHistory,
}

#[derive(Default)]
struct PeerRateHistory {
    last_sampled_at: Option<Instant>,
    totals: HashMap<String, (u64, u64)>,
}

/// Result returned after starting or resuming a torrent inside the session.
#[derive(Debug, Clone)]
pub struct StartedTorrent {
    pub info_hash: String,
    pub name: String,
    pub total_bytes: u64,
    pub output_folder: PathBuf,
    pub completed_path: PathBuf,
    pub only_files: Option<Vec<usize>>,
    pub torrent_bytes: Vec<u8>,
}

/// Manages the single shared `librqbit` session and tracks Ruscello torrent state.
pub struct TorrentManager {
    data_dir: PathBuf,
    default_destination: PathBuf,
    session_lock: RwLock<SessionHolder>,
    entries: Mutex<BTreeMap<DownloadId, ManagedEntry>>,
    http: reqwest::Client,
}

impl fmt::Debug for TorrentManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorrentManager")
            .field("data_dir", &self.data_dir)
            .field("default_destination", &self.default_destination)
            .finish_non_exhaustive()
    }
}

impl TorrentManager {
    /// Creates and starts the shared `librqbit` BitTorrent session.
    pub async fn new(
        data_dir: PathBuf,
        default_destination: PathBuf,
        settings: TorrentSettings,
    ) -> Result<Self, TorrentError> {
        tokio::fs::create_dir_all(&data_dir).await?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("Ruscello/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| TorrentError::Http(error.to_string()))?;
        let session = build_session(&data_dir, &default_destination, &settings).await?;
        pause_all_restored_torrents(&session).await;
        let api = Api::new(session.clone(), None);
        Ok(Self {
            data_dir,
            default_destination,
            session_lock: RwLock::new(SessionHolder {
                session,
                api,
                settings,
            }),
            entries: Mutex::new(BTreeMap::new()),
            http,
        })
    }

    /// Reconciles torrents loaded from `librqbit` persistence against Ruscello's SQLite queue.
    ///
    /// Known torrents are linked by `info_hash` and kept paused until the scheduler dispatches them;
    /// orphaned torrents no longer present in Ruscello's queue are forgotten without deleting files.
    pub async fn reconcile(&self, known: &[(DownloadId, Option<String>)]) {
        let session = { self.session_lock.read().await.session.clone() };
        let known_by_hash: HashMap<String, DownloadId> = known
            .iter()
            .filter_map(|(id, hash)| hash.as_ref().map(|h| (h.to_ascii_lowercase(), *id)))
            .collect();

        let existing: Vec<(usize, String, ManagedTorrentHandle)> = session.with_torrents(|iter| {
            iter.map(|(tid, handle)| (tid, handle.info_hash().as_string(), handle.clone()))
                .collect()
        });

        let mut to_forget = Vec::new();
        {
            let mut entries = self.lock_entries();
            for (tid, hash, handle) in existing {
                let normalized = hash.to_ascii_lowercase();
                if let Some(&download_id) = known_by_hash.get(&normalized) {
                    let output_folder = handle.output_folder().to_path_buf();
                    let name = handle.name();
                    let only_files = handle.only_files();
                    let (torrent_bytes, is_single_file, single_file_rel_path) = handle
                        .with_metadata(|meta| {
                            let files: Vec<PathBuf> = meta
                                .file_infos
                                .iter()
                                .map(|f| f.relative_filename.clone())
                                .collect();
                            (
                                Some(meta.torrent_bytes.to_vec()),
                                Some(files.len() <= 1),
                                files.into_iter().next(),
                            )
                        })
                        .unwrap_or((None, None, None));

                    if let Some(entry) = entries.get_mut(&download_id) {
                        entry.handle = Some(handle);
                        entry.info_hash = Some(hash);
                        entry.output_folder = Some(output_folder);
                        if entry.name.is_none() {
                            entry.name = name;
                        }
                        if entry.only_files.is_none() {
                            entry.only_files = only_files;
                        }
                        if entry.torrent_bytes.is_none() {
                            entry.torrent_bytes = torrent_bytes;
                        }
                        entry.is_single_file = is_single_file;
                        entry.single_file_rel_path = single_file_rel_path;
                    } else {
                        entries.insert(
                            download_id,
                            ManagedEntry {
                                handle: Some(handle),
                                info_hash: Some(hash),
                                name,
                                output_folder: Some(output_folder),
                                only_files,
                                torrent_bytes,
                                prior_uploaded_bytes: 0,
                                last_live_uploaded_bytes: 0,
                                prior_seed_elapsed_secs: 0,
                                seeding_started_at: None,
                                seed_ratio_millis: None,
                                seed_time_limit_minutes: None,
                                is_single_file,
                                single_file_rel_path,
                                is_resolving_metadata: false,
                                last_error: None,
                                peer_rates: PeerRateHistory::default(),
                            },
                        );
                    }
                } else {
                    to_forget.push(tid);
                }
            }
        }

        for tid in to_forget {
            if let Err(error) = session.delete(TorrentIdOrHash::Id(tid), false).await {
                tracing::debug!(torrent_id = tid, %error, "Failed to prune orphaned persisted torrent");
            }
        }
    }

    /// Returns the actual bound listening port (useful when configured with port `0`).
    pub async fn listen_port(&self) -> Option<u16> {
        let holder = self.session_lock.read().await;
        holder.session.listen_addr().map(|addr| addr.port())
    }

    /// Returns global BitTorrent telemetry (speeds, DHT nodes, listen port).
    pub async fn session_telemetry(&self) -> TorrentSessionTelemetry {
        let holder = self.session_lock.read().await;
        let stats = holder.session.stats_snapshot();
        let dht_nodes = holder.session.get_dht().map(|dht| {
            let dht_stats = dht.stats();
            dht_stats
                .routing_table_size
                .saturating_add(dht_stats.routing_table_size_v6)
        });
        TorrentSessionTelemetry {
            download_speed_bps: stats.download_speed.as_bytes(),
            upload_speed_bps: stats.upload_speed.as_bytes(),
            dht_nodes,
            listen_port: holder.session.listen_addr().map(|addr| addr.port()),
        }
    }

    /// Updates global BitTorrent settings at runtime, restarting the underlying session only if
    /// transport/session options (port, DHT, UPnP, LSD, interface, blocklist, peer limit) changed.
    pub async fn apply_settings(&self, new_settings: TorrentSettings) -> Result<(), TorrentError> {
        let needs_restart = {
            let holder = self.session_lock.read().await;
            if holder.settings == new_settings {
                return Ok(());
            }
            holder.settings.needs_session_restart(&new_settings)
        };

        if !needs_restart {
            let mut holder = self.session_lock.write().await;
            apply_live_ratelimits(&holder.session, &new_settings);
            holder.settings = new_settings;
            return Ok(());
        }

        // Capture active download states before tearing down the session so live torrents can be
        // restored seamlessly under the new listener/DHT/interface/blocklist configuration.
        struct RestoreItem {
            id: DownloadId,
            was_live: bool,
            info_hash: Option<String>,
        }

        let (old_session, old_settings) = {
            let holder = self.session_lock.read().await;
            (holder.session.clone(), holder.settings.clone())
        };
        let restore_items: Vec<RestoreItem> = {
            let mut entries = self.lock_entries();
            entries
                .iter_mut()
                .filter_map(|(&id, entry)| {
                    let handle = entry.handle.take()?;
                    if let Some(started) = entry.seeding_started_at.take() {
                        entry.prior_seed_elapsed_secs = entry
                            .prior_seed_elapsed_secs
                            .saturating_add(started.elapsed().as_secs());
                    }
                    entry.prior_uploaded_bytes = entry
                        .prior_uploaded_bytes
                        .saturating_add(entry.last_live_uploaded_bytes);
                    entry.last_live_uploaded_bytes = 0;

                    let was_live = matches!(
                        handle.stats().state,
                        TorrentStatsState::Live | TorrentStatsState::Initializing { paused: false }
                    );
                    Some(RestoreItem {
                        id,
                        was_live,
                        info_hash: entry
                            .info_hash
                            .clone()
                            .or_else(|| Some(handle.info_hash().as_string())),
                    })
                })
                .collect()
        };

        old_session.stop().await;

        let (new_session, effective_settings, apply_error) =
            match build_session(&self.data_dir, &self.default_destination, &new_settings).await {
                Ok(session) => (session, new_settings, None),
                Err(error) => {
                    // Roll back to previous settings so active downloads keep running if new settings fail.
                    let rollback =
                        build_session(&self.data_dir, &self.default_destination, &old_settings)
                            .await?;
                    (rollback, old_settings, Some(error))
                }
            };
        pause_all_restored_torrents(&new_session).await;
        let new_api = Api::new(new_session.clone(), None);

        {
            let mut holder = self.session_lock.write().await;
            holder.session = new_session.clone();
            holder.api = new_api;
            holder.settings = effective_settings;
        }

        // Re-link restored torrents and unpause those that were active.
        for item in restore_items {
            let handle = item.info_hash.as_deref().and_then(|hash| {
                new_session.with_torrents(|iter| {
                    for (_, h) in iter {
                        if h.info_hash().as_string().eq_ignore_ascii_case(hash) {
                            return Some(h.clone());
                        }
                    }
                    None
                })
            });
            if let Some(handle) = handle {
                if item.was_live
                    && let Err(error) = new_session.unpause(&handle).await
                {
                    tracing::debug!(id = item.id.0, %error, "Failed to unpause torrent after settings reload");
                }
                if let Some(entry) = self.lock_entries().get_mut(&item.id) {
                    entry.handle = Some(handle);
                }
            }
        }

        match apply_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Inspects a `.torrent` file, URL, or magnet URI without permanently adding it to the session.
    pub async fn inspect(
        &self,
        source: &TorrentSource,
        initial_peers: Vec<SocketAddr>,
        cancellation: &CancellationToken,
    ) -> Result<TorrentInspection, TorrentError> {
        match source {
            TorrentSource::File(path) => {
                let bytes = tokio::fs::read(path).await?;
                inspect_torrent_bytes(&bytes)
            }
            TorrentSource::Url(url) => {
                let bytes = fetch_torrent_url(&self.http, url, cancellation).await?;
                inspect_torrent_bytes(&bytes)
            }
            TorrentSource::Magnet(uri) => {
                let magnet = Magnet::parse(uri)
                    .map_err(|error| TorrentError::InvalidSource(error.to_string()))?;
                magnet
                    .as_id20()
                    .ok_or_else(|| TorrentError::InvalidSource("missing v1 infohash".into()))?;
                let session = { self.session_lock.read().await.session.clone() };
                let opts = AddTorrentOptions {
                    list_only: true,
                    overwrite: true,
                    initial_peers: if initial_peers.is_empty() {
                        None
                    } else {
                        Some(initial_peers)
                    },
                    ..Default::default()
                };
                let fut = session.add_torrent(AddTorrent::from_url(uri.as_str()), Some(opts));
                let response = tokio::select! {
                    _ = cancellation.cancelled() => return Err(TorrentError::Cancelled),
                    result = fut => result.map_err(|error| TorrentError::Session(error.to_string()))?,
                };
                match response {
                    AddTorrentResponse::ListOnly(ListOnlyResponse {
                        info_hash,
                        info,
                        only_files,
                        torrent_bytes,
                        ..
                    }) => build_inspection_from_validated(
                        info_hash.as_string(),
                        &info,
                        only_files.as_deref(),
                        magnet.name.as_deref(),
                        magnet.trackers,
                        Some(torrent_bytes.to_vec()),
                    ),
                    AddTorrentResponse::AlreadyManaged(_, handle) => {
                        let hash = handle.info_hash().as_string();
                        let only_files = handle.only_files();
                        let trackers: Vec<String> = handle
                            .shared()
                            .trackers
                            .iter()
                            .map(ToString::to_string)
                            .collect();
                        let name = handle.name();
                        handle
                            .with_metadata(|meta| {
                                build_inspection_from_validated(
                                    hash.clone(),
                                    &meta.info,
                                    only_files.as_deref(),
                                    name.as_deref(),
                                    trackers.clone(),
                                    Some(meta.torrent_bytes.to_vec()),
                                )
                            })
                            .map_err(|_| TorrentError::NotResolved)?
                    }
                    AddTorrentResponse::Added(_, _) => Err(TorrentError::Session(
                        "unexpected non-list-only response during inspect".into(),
                    )),
                }
            }
        }
    }

    /// Starts or resumes a torrent inside the managed `librqbit` session.
    pub async fn start_or_resume(
        &self,
        request: AddTorrentRequest,
        cancellation: &CancellationToken,
    ) -> Result<StartedTorrent, TorrentError> {
        let session = { self.session_lock.read().await.session.clone() };
        let id = request.id;

        if let Some(existing_handle) = self.existing_handle(id) {
            if request.force_recheck {
                return self
                    .recheck_existing(id, existing_handle, request, &session, cancellation)
                    .await;
            }
            if let Some(only_files) = request.only_files.as_ref() {
                let set: HashSet<usize> = only_files.iter().copied().collect();
                session
                    .update_only_files(&existing_handle, &set)
                    .await
                    .map_err(|error| TorrentError::Session(error.to_string()))?;
            }
            {
                let mut entries = self.lock_entries();
                if let Some(entry) = entries.get_mut(&id) {
                    if request.only_files.is_some() {
                        entry.only_files = request.only_files.clone();
                    }
                    entry.prior_uploaded_bytes =
                        entry.prior_uploaded_bytes.max(request.prior_uploaded_bytes);
                    entry.prior_seed_elapsed_secs = entry
                        .prior_seed_elapsed_secs
                        .max(request.prior_seed_elapsed_secs);
                    entry.seed_ratio_millis = request.seed_ratio_millis;
                    entry.seed_time_limit_minutes = request.seed_time_limit_minutes;
                }
            }
            if !request.paused {
                session
                    .unpause(&existing_handle)
                    .await
                    .map_err(|error| TorrentError::Session(error.to_string()))?;
            }
            return self.extract_started_torrent(id, &existing_handle);
        }

        // Seed initial state in entries map so progress callers can report Resolving immediately.
        {
            let mut entries = self.lock_entries();
            entries.insert(
                id,
                ManagedEntry {
                    handle: None,
                    info_hash: None,
                    name: None,
                    output_folder: request.resolved_output_folder.clone(),
                    only_files: request.only_files.clone(),
                    torrent_bytes: request.cached_torrent_bytes.clone(),
                    prior_uploaded_bytes: request.prior_uploaded_bytes,
                    last_live_uploaded_bytes: 0,
                    prior_seed_elapsed_secs: request.prior_seed_elapsed_secs,
                    seeding_started_at: None,
                    seed_ratio_millis: request.seed_ratio_millis,
                    seed_time_limit_minutes: request.seed_time_limit_minutes,
                    is_single_file: None,
                    single_file_rel_path: None,
                    is_resolving_metadata: matches!(request.source, TorrentSource::Magnet(_))
                        && request.cached_torrent_bytes.is_none(),
                    last_error: None,
                    peer_rates: PeerRateHistory::default(),
                },
            );
        }

        // Resolve source to canonical `.torrent` bytes and output folder.
        let mut initial_peers = request.initial_peers.clone();
        let (torrent_bytes, inspection) = if let Some(cached) = request.cached_torrent_bytes {
            let parsed = inspect_torrent_bytes(&cached)?;
            (cached, parsed)
        } else {
            match &request.source {
                TorrentSource::File(path) => {
                    let bytes = tokio::fs::read(path).await?;
                    let parsed = inspect_torrent_bytes(&bytes)?;
                    (bytes, parsed)
                }
                TorrentSource::Url(url) => {
                    let bytes = fetch_torrent_url(&self.http, url, cancellation).await?;
                    let parsed = inspect_torrent_bytes(&bytes)?;
                    (bytes, parsed)
                }
                TorrentSource::Magnet(uri) => {
                    let magnet = Magnet::parse(uri)
                        .map_err(|error| TorrentError::InvalidSource(error.to_string()))?;
                    magnet
                        .as_id20()
                        .ok_or_else(|| TorrentError::InvalidSource("missing v1 infohash".into()))?;

                    let list_opts = AddTorrentOptions {
                        list_only: true,
                        overwrite: true,
                        initial_peers: if initial_peers.is_empty() {
                            None
                        } else {
                            Some(initial_peers.clone())
                        },
                        ..Default::default()
                    };
                    let fut =
                        session.add_torrent(AddTorrent::from_url(uri.as_str()), Some(list_opts));
                    let response = tokio::select! {
                        _ = cancellation.cancelled() => {
                            self.clear_resolving_entry(id);
                            return Err(TorrentError::Cancelled);
                        }
                        res = fut => res.map_err(|error| TorrentError::Session(error.to_string()))?,
                    };

                    match response {
                        AddTorrentResponse::ListOnly(ListOnlyResponse {
                            info_hash,
                            info,
                            only_files,
                            seen_peers,
                            torrent_bytes,
                            ..
                        }) => {
                            for peer in seen_peers {
                                if !initial_peers.contains(&peer) {
                                    initial_peers.push(peer);
                                }
                            }
                            let bytes = torrent_bytes.to_vec();
                            let inspected = build_inspection_from_validated(
                                info_hash.as_string(),
                                &info,
                                only_files.as_deref(),
                                magnet.name.as_deref(),
                                magnet.trackers,
                                Some(bytes.clone()),
                            )?;
                            (bytes, inspected)
                        }
                        AddTorrentResponse::AlreadyManaged(_, handle) => {
                            let bytes = handle
                                .with_metadata(|meta| meta.torrent_bytes.to_vec())
                                .map_err(|_| TorrentError::NotResolved)?;
                            let inspected = inspect_torrent_bytes(&bytes)?;
                            (bytes, inspected)
                        }
                        AddTorrentResponse::Added(_, _) => {
                            return Err(TorrentError::Session(
                                "unexpected add response while resolving magnet".into(),
                            ));
                        }
                    }
                }
            }
        };

        if cancellation.is_cancelled() {
            self.clear_resolving_entry(id);
            return Err(TorrentError::Cancelled);
        }

        let output_folder = resolve_output_folder(
            &request.destination,
            request.resolved_output_folder.as_deref(),
            inspection.suggested_subfolder.as_deref(),
        );
        tokio::fs::create_dir_all(&output_folder).await?;

        if request.force_recheck {
            // Ensure no stale persistence entry or fastresume bitfield remains for this info_hash.
            let already = session.with_torrents(|iter| {
                for (tid, h) in iter {
                    if h.info_hash()
                        .as_string()
                        .eq_ignore_ascii_case(&inspection.info_hash)
                    {
                        return Some(tid);
                    }
                }
                None
            });
            if let Some(tid) = already {
                let _ = session.delete(TorrentIdOrHash::Id(tid), false).await;
            }
        }

        // Prevent duplicate active downloads sharing the same info_hash cleanly.
        if let Some(owner_id) = self.owner_of_info_hash(&inspection.info_hash, Some(id)) {
            self.clear_resolving_entry(id);
            return Err(TorrentError::InvalidSource(format!(
                "torrent {} is already managed by download {}",
                inspection.info_hash, owner_id.0
            )));
        }

        let only_files = request.only_files.or_else(|| {
            if inspection.files.iter().all(|f| f.selected) {
                None
            } else {
                Some(
                    inspection
                        .files
                        .iter()
                        .filter(|f| f.selected)
                        .map(|f| f.index)
                        .collect(),
                )
            }
        });

        let opts = AddTorrentOptions {
            paused: request.paused,
            only_files: only_files.clone(),
            overwrite: true,
            output_folder: Some(output_folder.to_string_lossy().into_owned()),
            ratelimits: LimitsConfig {
                download_bps: to_non_zero_u32(request.download_limit_bps),
                upload_bps: None,
            },
            initial_peers: if initial_peers.is_empty() {
                None
            } else {
                Some(initial_peers)
            },
            ..Default::default()
        };

        let response = session
            .add_torrent(
                AddTorrent::TorrentFileBytes(Bytes::from(torrent_bytes.clone())),
                Some(opts),
            )
            .await
            .map_err(|error| TorrentError::Session(error.to_string()))?;

        let handle = match response {
            AddTorrentResponse::Added(_, handle) => handle,
            AddTorrentResponse::AlreadyManaged(_, handle) => {
                if let Some(ref files) = only_files {
                    let set: HashSet<usize> = files.iter().copied().collect();
                    session
                        .update_only_files(&handle, &set)
                        .await
                        .map_err(|error| TorrentError::Session(error.to_string()))?;
                }
                if !request.paused && handle.is_paused() {
                    session
                        .unpause(&handle)
                        .await
                        .map_err(|error| TorrentError::Session(error.to_string()))?;
                }
                handle
            }
            AddTorrentResponse::ListOnly(_) => {
                return Err(TorrentError::Session(
                    "unexpected list_only response when adding torrent".into(),
                ));
            }
        };

        let single_file = inspection.files.len() <= 1;
        let first_rel_path = inspection.files.first().map(|f| f.path.clone());
        let completed_path =
            compute_completed_path(&output_folder, single_file, first_rel_path.as_deref());
        let selected_bytes = compute_selected_bytes(&inspection.files, only_files.as_deref());

        {
            let mut entries = self.lock_entries();
            if let Some(entry) = entries.get_mut(&id) {
                entry.handle = Some(handle);
                entry.info_hash = Some(inspection.info_hash.clone());
                entry.name = Some(inspection.name.clone());
                entry.output_folder = Some(output_folder.clone());
                entry.only_files = only_files.clone();
                entry.torrent_bytes = Some(torrent_bytes.clone());
                entry.is_single_file = Some(single_file);
                entry.single_file_rel_path = first_rel_path;
                entry.is_resolving_metadata = false;
                entry.last_error = None;
            }
        }

        Ok(StartedTorrent {
            info_hash: inspection.info_hash,
            name: inspection.name,
            total_bytes: selected_bytes,
            output_folder,
            completed_path,
            only_files,
            torrent_bytes,
        })
    }

    async fn recheck_existing(
        &self,
        id: DownloadId,
        handle: ManagedTorrentHandle,
        mut request: AddTorrentRequest,
        session: &Arc<Session>,
        cancellation: &CancellationToken,
    ) -> Result<StartedTorrent, TorrentError> {
        let tid = handle.id();
        let snapshot = self.poll_progress(id).ok();
        let saved_bytes = {
            let entries = self.lock_entries();
            entries
                .get(&id)
                .and_then(|e| e.torrent_bytes.clone())
                .or_else(|| handle.with_metadata(|m| m.torrent_bytes.to_vec()).ok())
        };
        let output_folder = handle.output_folder().to_path_buf();
        let only_files = request.only_files.take().or_else(|| handle.only_files());

        if let Some(snap) = snapshot {
            request.prior_uploaded_bytes = snap.uploaded_bytes;
            request.prior_seed_elapsed_secs = snap.seed_elapsed_secs;
        }
        request.cached_torrent_bytes = request.cached_torrent_bytes.or(saved_bytes);
        request.resolved_output_folder = Some(output_folder);
        request.only_files = only_files;
        request.force_recheck = false;

        session
            .delete(TorrentIdOrHash::Id(tid), false)
            .await
            .map_err(|error| TorrentError::Session(error.to_string()))?;

        {
            let mut entries = self.lock_entries();
            entries.remove(&id);
        }

        Box::pin(self.start_or_resume(request, cancellation)).await
    }

    /// Clears fastresume state and re-adds the torrent so `librqbit` performs a full SHA-1 recheck.
    pub async fn force_recheck(
        &self,
        request: AddTorrentRequest,
        cancellation: &CancellationToken,
    ) -> Result<StartedTorrent, TorrentError> {
        let mut req = request;
        req.force_recheck = true;
        Box::pin(self.start_or_resume(req, cancellation)).await
    }

    /// Pauses an active or initializing torrent while preserving accumulated upload/seeding metrics.
    pub async fn pause(&self, id: DownloadId) -> Result<(), TorrentError> {
        let (handle, session) = {
            let handle = self.lock_entries().get(&id).and_then(|e| e.handle.clone());
            let session = self.session_lock.read().await.session.clone();
            (handle, session)
        };
        if let Some(handle) = handle {
            if !handle.is_paused()
                && let Err(error) = session.pause(&handle).await
            {
                let message = error.to_string();
                if !message.contains("already paused") {
                    return Err(TorrentError::Session(message));
                }
            }
            let mut entries = self.lock_entries();
            if let Some(entry) = entries.get_mut(&id) {
                if let Some(started) = entry.seeding_started_at.take() {
                    entry.prior_seed_elapsed_secs = entry
                        .prior_seed_elapsed_secs
                        .saturating_add(started.elapsed().as_secs());
                }
                entry.prior_uploaded_bytes = entry
                    .prior_uploaded_bytes
                    .saturating_add(entry.last_live_uploaded_bytes);
                entry.last_live_uploaded_bytes = 0;
            }
        }
        Ok(())
    }

    /// Updates selected files (`only_files`) for a torrent while paused or live.
    pub async fn set_only_files(
        &self,
        id: DownloadId,
        only_files: Vec<usize>,
    ) -> Result<(), TorrentError> {
        let session = { self.session_lock.read().await.session.clone() };
        let handle = self.lock_entries().get(&id).and_then(|e| e.handle.clone());
        let set: HashSet<usize> = only_files.iter().copied().collect();

        if let Some(handle) = handle {
            let was_paused = handle.is_paused();
            session
                .update_only_files(&handle, &set)
                .await
                .map_err(|error| TorrentError::Session(error.to_string()))?;

            // Wait: if torrent was already finished seeding (`finished == true`), pause and unpause so
            // `librqbit` opens newly selected files and begins requesting their pieces immediately.
            if !was_paused && !handle.stats().finished {
                let _ = session.pause(&handle).await;
                session
                    .unpause(&handle)
                    .await
                    .map_err(|error| TorrentError::Session(error.to_string()))?;
            }
        }

        if let Some(entry) = self.lock_entries().get_mut(&id) {
            entry.only_files = Some(only_files);
            if let Some(started) = entry.seeding_started_at.take() {
                entry.prior_seed_elapsed_secs = entry
                    .prior_seed_elapsed_secs
                    .saturating_add(started.elapsed().as_secs());
            }
        }

        Ok(())
    }

    /// Updates per-torrent seed limits stored on the managed entry.
    pub fn set_torrent_seed_limits(
        &self,
        id: DownloadId,
        seed_ratio_millis: Option<u32>,
        seed_time_limit_minutes: Option<u64>,
    ) {
        if let Some(entry) = self.lock_entries().get_mut(&id) {
            entry.seed_ratio_millis = seed_ratio_millis;
            entry.seed_time_limit_minutes = seed_time_limit_minutes;
        }
    }

    /// Removes a torrent from the active `librqbit` session, optionally deleting downloaded files.
    pub async fn remove(&self, id: DownloadId, delete_files: bool) -> Result<(), TorrentError> {
        let session = { self.session_lock.read().await.session.clone() };
        let removed_entry = self.lock_entries().remove(&id);

        if let Some(entry) = removed_entry {
            if let Some(handle) = entry.handle {
                session
                    .delete(TorrentIdOrHash::Id(handle.id()), delete_files)
                    .await
                    .map_err(|error| TorrentError::Session(error.to_string()))?;
            } else if delete_files && let Some(out) = entry.output_folder {
                remove_dir_or_file_if_exists(
                    &out,
                    entry.is_single_file == Some(true),
                    entry.single_file_rel_path.as_deref(),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Samples current progress and checks whether seeding limits have been satisfied.
    pub fn poll_progress(&self, id: DownloadId) -> Result<TorrentProgressSnapshot, TorrentError> {
        let session_settings = match self.session_lock.try_read() {
            Ok(guard) => Some(guard.settings.clone()),
            Err(_) => None,
        };
        let mut entries = self.lock_entries();
        let entry = entries.get_mut(&id).ok_or(TorrentError::NotFound)?;

        if entry.is_resolving_metadata {
            return Ok(TorrentProgressSnapshot {
                phase: TorrentPhase::FetchingMetadata,
                name: entry.name.clone(),
                info_hash: entry.info_hash.clone(),
                downloaded_bytes: 0,
                uploaded_bytes: entry.prior_uploaded_bytes,
                total_bytes: None,
                download_speed_bps: 0,
                upload_speed_bps: 0,
                eta_secs: None,
                seed_ratio: 0.0,
                seed_elapsed_secs: entry.prior_seed_elapsed_secs,
                peers_connected: 0,
                peers_total: 0,
                seeders_connected: 0,
                completed_path: None,
                output_folder: entry.output_folder.clone(),
                only_files: entry.only_files.clone(),
                torrent_bytes: entry.torrent_bytes.clone(),
                piece_count: 0,
                pieces_have: 0,
                seed_goal_reached: false,
            });
        }

        let Some(handle) = entry.handle.clone() else {
            let phase = match &entry.last_error {
                Some(message) => TorrentPhase::Failed(message.clone()),
                None => TorrentPhase::Paused,
            };
            return Ok(TorrentProgressSnapshot {
                phase,
                name: entry.name.clone(),
                info_hash: entry.info_hash.clone(),
                downloaded_bytes: 0,
                uploaded_bytes: entry.prior_uploaded_bytes,
                total_bytes: None,
                download_speed_bps: 0,
                upload_speed_bps: 0,
                eta_secs: None,
                seed_ratio: 0.0,
                seed_elapsed_secs: entry.prior_seed_elapsed_secs,
                peers_connected: 0,
                peers_total: 0,
                seeders_connected: 0,
                completed_path: entry.output_folder.clone().map(|out| {
                    compute_completed_path(
                        &out,
                        entry.is_single_file == Some(true),
                        entry.single_file_rel_path.as_deref(),
                    )
                }),
                output_folder: entry.output_folder.clone(),
                only_files: entry.only_files.clone(),
                torrent_bytes: entry.torrent_bytes.clone(),
                piece_count: 0,
                pieces_have: 0,
                seed_goal_reached: false,
            });
        };

        let stats = handle.stats();
        let name = handle.name().or_else(|| entry.name.clone());
        let info_hash = Some(handle.info_hash().as_string());
        let output_folder = Some(handle.output_folder().to_path_buf());
        let total_bytes = stats.total_bytes;

        let (piece_count, pieces_have) = handle
            .with_metadata(|meta| (meta.info.lengths().total_pieces(), 0u32))
            .unwrap_or((0, 0));

        let (dl_bps, ul_bps, peers_connected, peers_total, seeders_connected) =
            if let Some(live) = &stats.live {
                (
                    live.download_speed.as_bytes(),
                    live.upload_speed.as_bytes(),
                    live.snapshot.peer_stats.live,
                    live.snapshot.peer_stats.seen,
                    live.snapshot.peer_stats.live_seeders,
                )
            } else {
                (0, 0, 0, 0, 0)
            };

        if matches!(stats.state, TorrentStatsState::Live) {
            entry.last_live_uploaded_bytes = stats.uploaded_bytes;
        }
        let total_uploaded = entry
            .prior_uploaded_bytes
            .saturating_add(entry.last_live_uploaded_bytes);

        let is_seeding = matches!(stats.state, TorrentStatsState::Live) && stats.finished;
        if is_seeding {
            if entry.seeding_started_at.is_none() {
                entry.seeding_started_at = Some(Instant::now());
            }
        } else if let Some(started) = entry.seeding_started_at.take() {
            entry.prior_seed_elapsed_secs = entry
                .prior_seed_elapsed_secs
                .saturating_add(started.elapsed().as_secs());
        }

        let seed_elapsed_secs = entry.prior_seed_elapsed_secs.saturating_add(
            entry
                .seeding_started_at
                .map_or(0, |t| t.elapsed().as_secs()),
        );

        let phase = match stats.state {
            TorrentStatsState::Initializing { paused: true } | TorrentStatsState::Paused => {
                TorrentPhase::Paused
            }
            TorrentStatsState::Initializing { paused: false } => TorrentPhase::Checking {
                checked_bytes: stats.progress_bytes,
                total_bytes,
            },
            TorrentStatsState::Live if stats.finished => TorrentPhase::Seeding,
            TorrentStatsState::Live => TorrentPhase::Downloading,
            TorrentStatsState::Error => {
                TorrentPhase::Failed(stats.error.unwrap_or_else(|| "torrent error".into()))
            }
        };

        let ratio = compute_ratio(total_uploaded, total_bytes);
        let ratio_limit = entry
            .seed_ratio_millis
            .or_else(|| session_settings.as_ref().and_then(|s| s.seed_ratio_millis))
            .map(|m| f64::from(m) / 1000.0);
        let time_limit = entry.seed_time_limit_minutes.or_else(|| {
            session_settings
                .as_ref()
                .and_then(|s| s.seed_time_limit_minutes)
        });

        let seed_goal_reached = stats.finished
            && matches!(phase, TorrentPhase::Seeding)
            && (ratio_limit.is_some_and(|limit| ratio >= limit)
                || time_limit.is_some_and(|mins| seed_elapsed_secs >= mins.saturating_mul(60)));

        let eta_secs = if matches!(phase, TorrentPhase::Downloading) && dl_bps > 0 {
            Some(total_bytes.saturating_sub(stats.progress_bytes) / dl_bps)
        } else {
            None
        };

        let completed_path = output_folder.as_deref().map(|dir| {
            compute_completed_path(
                dir,
                entry.is_single_file == Some(true),
                entry.single_file_rel_path.as_deref(),
            )
        });

        Ok(TorrentProgressSnapshot {
            phase,
            name,
            info_hash,
            downloaded_bytes: stats.progress_bytes,
            uploaded_bytes: total_uploaded,
            total_bytes: Some(total_bytes),
            download_speed_bps: dl_bps,
            upload_speed_bps: ul_bps,
            eta_secs,
            seed_ratio: ratio,
            seed_elapsed_secs,
            peers_connected,
            peers_total,
            seeders_connected,
            completed_path,
            output_folder,
            only_files: handle.only_files(),
            torrent_bytes: entry.torrent_bytes.clone(),
            piece_count,
            pieces_have,
            seed_goal_reached,
        })
    }

    /// Collects full details (piece bitmap, files, live peers with per-peer speeds, trackers) for the UI.
    pub async fn details(&self, id: DownloadId) -> Result<TorrentDetailsSnapshot, TorrentError> {
        let holder = self.session_lock.read().await;
        let (
            handle,
            prior_uploaded,
            last_live_uploaded,
            prior_seed_secs,
            seeding_started_at,
            cached_bytes,
            saved_only_files,
        ) = {
            let entries = self.lock_entries();
            let entry = entries.get(&id).ok_or(TorrentError::NotFound)?;
            (
                entry.handle.clone(),
                entry.prior_uploaded_bytes,
                entry.last_live_uploaded_bytes,
                entry.prior_seed_elapsed_secs,
                entry.seeding_started_at,
                entry.torrent_bytes.clone(),
                entry.only_files.clone(),
            )
        };

        let Some(handle) = handle else {
            if let Some(bytes) = cached_bytes {
                let inspected = inspect_torrent_bytes(&bytes)?;
                let files = inspected
                    .files
                    .into_iter()
                    .map(|f| {
                        let included = saved_only_files
                            .as_ref()
                            .is_none_or(|list| list.contains(&f.index));
                        TorrentFileProgress {
                            index: f.index,
                            path: f.path,
                            length: f.length,
                            downloaded_bytes: 0,
                            priority: if included {
                                TorrentFilePriority::Download
                            } else {
                                TorrentFilePriority::Skip
                            },
                        }
                    })
                    .collect();
                let trackers = inspected
                    .trackers
                    .into_iter()
                    .map(|url| TorrentTrackerInfo {
                        protocol: tracker_protocol(&url),
                        url,
                        status: if inspected.is_private {
                            TorrentTrackerStatus::DisabledPrivate
                        } else {
                            TorrentTrackerStatus::Standby
                        },
                    })
                    .collect();
                return Ok(TorrentDetailsSnapshot {
                    info_hash: inspected.info_hash,
                    is_private: inspected.is_private,
                    seed_ratio: 0.0,
                    seed_elapsed_secs: prior_seed_secs,
                    uploaded_bytes: prior_uploaded,
                    download_speed_bps: 0,
                    upload_speed_bps: 0,
                    piece_map: None,
                    files,
                    peers: Vec::new(),
                    trackers,
                });
            }
            return Err(TorrentError::NotResolved);
        };

        let tid = TorrentIdOrHash::Id(handle.id());
        let stats = handle.stats();
        let info_hash = handle.info_hash().as_string();
        let only_files = handle.only_files();

        let (piece_length, total_pieces, is_private, files) = handle
            .with_metadata(|meta| {
                let lengths = meta.info.lengths();
                let files = meta
                    .file_infos
                    .iter()
                    .enumerate()
                    .map(|(idx, info)| {
                        let included = only_files
                            .as_ref()
                            .is_none_or(|selected| selected.contains(&idx));
                        let have = stats.file_progress.get(idx).copied().unwrap_or(0);
                        TorrentFileProgress {
                            index: idx,
                            path: info.relative_filename.clone(),
                            length: info.len,
                            downloaded_bytes: have.min(info.len),
                            priority: if included {
                                TorrentFilePriority::Download
                            } else {
                                TorrentFilePriority::Skip
                            },
                        }
                    })
                    .collect::<Vec<_>>();
                (
                    lengths.default_piece_length(),
                    lengths.total_pieces(),
                    meta.info.info().private,
                    files,
                )
            })
            .map_err(|_| TorrentError::NotResolved)?;

        let piece_map = match holder.api.api_dump_haves(tid) {
            Ok((bf, _)) => {
                let slice = bf.as_raw_slice();
                let have_count = (0..total_pieces)
                    .filter(|&piece_idx| bf.get(piece_idx as usize).is_some_and(|b| *b))
                    .count() as u32;
                Some(TorrentPieceMap {
                    piece_count: total_pieces,
                    piece_length,
                    have_count,
                    have_bitmap: slice.to_vec(),
                })
            }
            Err(_) => None,
        };

        let tracker_status = if matches!(stats.state, TorrentStatsState::Live) {
            TorrentTrackerStatus::Announcing
        } else {
            TorrentTrackerStatus::Standby
        };
        let mut tracker_urls: Vec<String> = handle
            .shared()
            .trackers
            .iter()
            .map(ToString::to_string)
            .collect();
        tracker_urls.sort();
        let trackers = tracker_urls
            .into_iter()
            .enumerate()
            .map(|(idx, url)| TorrentTrackerInfo {
                protocol: tracker_protocol(&url),
                status: if is_private && idx > 0 {
                    TorrentTrackerStatus::DisabledPrivate
                } else {
                    tracker_status
                },
                url,
            })
            .collect();

        #[derive(Deserialize)]
        struct RawPeerCounters {
            fetched_bytes: u64,
            uploaded_bytes: u64,
            errors: u32,
        }

        #[derive(Deserialize)]
        struct RawPeerStats {
            counters: RawPeerCounters,
            state: String,
            conn_kind: Option<String>,
            client_name: Option<String>,
        }

        #[derive(Deserialize)]
        struct RawPeerSnapshot {
            peers: HashMap<String, RawPeerStats>,
        }

        let peer_filter = serde_json::from_str(r#"{"state":"all"}"#).unwrap_or_default();
        let peer_snapshot: Option<RawPeerSnapshot> = holder
            .api
            .api_peer_stats(tid, peer_filter)
            .ok()
            .and_then(|snap| serde_json::to_value(snap).ok())
            .and_then(|val| serde_json::from_value(val).ok());

        let peers = if let Some(snapshot) = peer_snapshot {
            let now = Instant::now();
            let mut entries = self.lock_entries();
            let rate_history = entries.get_mut(&id).map(|e| &mut e.peer_rates);

            let (elapsed_secs, prev_totals) = match rate_history {
                Some(ref hist) => (
                    hist.last_sampled_at
                        .map(|t| now.saturating_duration_since(t).as_secs_f64())
                        .unwrap_or(0.0),
                    Some(hist.totals.clone()),
                ),
                None => (0.0, None),
            };

            let mut new_totals = HashMap::with_capacity(snapshot.peers.len());
            let mut list: Vec<TorrentPeerInfo> = snapshot
                .peers
                .into_iter()
                .filter(|(_, p)| {
                    matches!(p.state.as_str(), "live" | "connecting" | "queued")
                        || p.counters.fetched_bytes > 0
                        || p.counters.uploaded_bytes > 0
                })
                .map(|(address, p)| {
                    let down = p.counters.fetched_bytes;
                    let up = p.counters.uploaded_bytes;
                    new_totals.insert(address.clone(), (down, up));
                    let (dl_bps, ul_bps) = if elapsed_secs > 0.05 {
                        if let Some((prev_down, prev_up)) =
                            prev_totals.as_ref().and_then(|m| m.get(&address))
                        {
                            (
                                ((down.saturating_sub(*prev_down)) as f64 / elapsed_secs) as u64,
                                ((up.saturating_sub(*prev_up)) as f64 / elapsed_secs) as u64,
                            )
                        } else {
                            (0, 0)
                        }
                    } else {
                        (0, 0)
                    };
                    let state = match p.state.as_str() {
                        "live" => TorrentPeerState::Live,
                        "connecting" => TorrentPeerState::Connecting,
                        "queued" => TorrentPeerState::Queued,
                        "dead" => TorrentPeerState::Dead,
                        _ => TorrentPeerState::NotNeeded,
                    };
                    let transport = p.conn_kind.as_deref().and_then(|kind| {
                        if kind.eq_ignore_ascii_case("tcp") {
                            Some(TorrentPeerTransport::Tcp)
                        } else if kind.eq_ignore_ascii_case("utp") {
                            Some(TorrentPeerTransport::Utp)
                        } else if kind.eq_ignore_ascii_case("socks") {
                            Some(TorrentPeerTransport::Socks)
                        } else {
                            None
                        }
                    });
                    TorrentPeerInfo {
                        address,
                        client: p.client_name,
                        transport,
                        state,
                        downloaded_bytes: down,
                        uploaded_bytes: up,
                        download_speed_bps: dl_bps,
                        upload_speed_bps: ul_bps,
                        errors: p.counters.errors,
                    }
                })
                .collect();

            if let Some(hist) = rate_history {
                hist.last_sampled_at = Some(now);
                hist.totals = new_totals;
            }
            list.sort_by(|a, b| {
                b.download_speed_bps
                    .cmp(&a.download_speed_bps)
                    .then_with(|| a.address.cmp(&b.address))
            });
            list
        } else {
            Vec::new()
        };

        let live_uploaded = if matches!(stats.state, TorrentStatsState::Live) {
            stats.uploaded_bytes
        } else {
            last_live_uploaded
        };
        let uploaded_bytes = prior_uploaded.saturating_add(live_uploaded);
        let seed_elapsed_secs =
            prior_seed_secs.saturating_add(seeding_started_at.map_or(0, |t| t.elapsed().as_secs()));
        let (dl_bps, ul_bps) = stats.live.as_ref().map_or((0, 0), |live| {
            (live.download_speed.as_bytes(), live.upload_speed.as_bytes())
        });

        Ok(TorrentDetailsSnapshot {
            info_hash,
            is_private,
            seed_ratio: compute_ratio(uploaded_bytes, stats.total_bytes),
            seed_elapsed_secs,
            uploaded_bytes,
            download_speed_bps: dl_bps,
            upload_speed_bps: ul_bps,
            piece_map,
            files,
            peers,
            trackers,
        })
    }

    /// Creates a `.torrent` file and starts serving it from this session (useful in tests and seeding).
    pub async fn create_and_serve_local(
        &self,
        source_path: &Path,
        trackers: Vec<String>,
        piece_length: Option<u32>,
    ) -> Result<(Vec<u8>, String, SocketAddr), TorrentError> {
        let holder = self.session_lock.read().await;
        let opts = CreateTorrentOptions {
            name: None,
            trackers,
            piece_length,
        };
        let (torrent_result, _handle) = holder
            .session
            .create_and_serve_torrent(source_path, opts)
            .await
            .map_err(|error| TorrentError::Session(error.to_string()))?;
        let bytes = torrent_result
            .as_bytes()
            .map_err(|error| TorrentError::Session(error.to_string()))?
            .to_vec();
        let hash = torrent_result.info_hash().as_string();
        let addr = holder.session.listen_addr().ok_or_else(|| {
            TorrentError::Session("seeder session has no listener address".into())
        })?;
        let loopback = if addr.ip().is_unspecified() {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port())
        } else {
            addr
        };
        Ok((bytes, hash, loopback))
    }

    /// Standalone helper to generate `.torrent` bytes from a folder or file without adding to session.
    pub async fn build_torrent_bytes(
        path: &Path,
        trackers: Vec<String>,
        piece_length: Option<u32>,
    ) -> Result<Vec<u8>, TorrentError> {
        let spawner = librqbit::spawn_utils::BlockingSpawner::new(1);
        let result = rqbit_create_torrent(
            path,
            CreateTorrentOptions {
                name: None,
                trackers,
                piece_length,
            },
            &spawner,
        )
        .await
        .map_err(|error| TorrentError::Session(error.to_string()))?;
        result
            .as_bytes()
            .map(|b| b.to_vec())
            .map_err(|error| TorrentError::Session(error.to_string()))
    }

    /// Gracefully stops the BitTorrent session and flushes fastresume metadata.
    pub async fn shutdown(&self) {
        let handles: Vec<ManagedTorrentHandle> = {
            let mut entries = self.lock_entries();
            entries
                .values_mut()
                .filter_map(|entry| {
                    if let Some(started) = entry.seeding_started_at.take() {
                        entry.prior_seed_elapsed_secs = entry
                            .prior_seed_elapsed_secs
                            .saturating_add(started.elapsed().as_secs());
                    }
                    entry.prior_uploaded_bytes = entry
                        .prior_uploaded_bytes
                        .saturating_add(entry.last_live_uploaded_bytes);
                    entry.last_live_uploaded_bytes = 0;
                    entry.handle.clone()
                })
                .collect()
        };
        let session = { self.session_lock.read().await.session.clone() };
        for handle in handles {
            if !handle.is_paused() {
                let _ = session.pause(&handle).await;
            }
        }
        session.stop().await;
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<DownloadId, ManagedEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn existing_handle(&self, id: DownloadId) -> Option<ManagedTorrentHandle> {
        self.lock_entries()
            .get(&id)
            .and_then(|entry| entry.handle.clone())
    }

    fn clear_resolving_entry(&self, id: DownloadId) {
        self.lock_entries().remove(&id);
    }

    fn owner_of_info_hash(
        &self,
        info_hash: &str,
        excluding: Option<DownloadId>,
    ) -> Option<DownloadId> {
        let entries = self.lock_entries();
        entries.iter().find_map(|(&id, entry)| {
            if Some(id) != excluding
                && entry
                    .info_hash
                    .as_deref()
                    .is_some_and(|h| h.eq_ignore_ascii_case(info_hash))
            {
                Some(id)
            } else {
                None
            }
        })
    }

    fn extract_started_torrent(
        &self,
        id: DownloadId,
        handle: &ManagedTorrentHandle,
    ) -> Result<StartedTorrent, TorrentError> {
        let entries = self.lock_entries();
        let entry = entries.get(&id).ok_or(TorrentError::NotFound)?;
        let only_files = handle.only_files().or_else(|| entry.only_files.clone());
        let (name, total_bytes, torrent_bytes, single_file, first_rel) = handle
            .with_metadata(|meta| {
                let stats = handle.stats();
                let name = handle
                    .name()
                    .unwrap_or_else(|| handle.info_hash().as_string());
                let single_file = meta.file_infos.len() <= 1;
                let first = meta.file_infos.first().map(|f| f.relative_filename.clone());
                (
                    name,
                    stats.total_bytes,
                    meta.torrent_bytes.to_vec(),
                    single_file,
                    first,
                )
            })
            .map_err(|_| TorrentError::NotResolved)?;
        let output_folder = handle.output_folder().to_path_buf();
        let completed_path =
            compute_completed_path(&output_folder, single_file, first_rel.as_deref());
        Ok(StartedTorrent {
            info_hash: handle.info_hash().as_string(),
            name,
            total_bytes,
            output_folder,
            completed_path,
            only_files,
            torrent_bytes,
        })
    }
}

async fn build_session(
    data_dir: &Path,
    default_destination: &Path,
    settings: &TorrentSettings,
) -> Result<Arc<Session>, TorrentError> {
    let bind_device_name = settings
        .bind_interface
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let blocklist_url = resolve_blocklist_url(settings.blocklist_source.as_deref())?;

    let dht = if settings.enable_dht {
        Some(DhtSessionConfig {
            bootstrap_addrs: settings.dht_bootstrap_addrs.clone(),
            port: if settings.listen_port == 0 {
                None
            } else {
                Some(settings.listen_port)
            },
            persistence: Some(DhtPersistenceConfig {
                dump_interval: None,
                config_filename: Some(data_dir.join("dht.json")),
            }),
        })
    } else {
        None
    };

    let opts = SessionOptions {
        dht,
        bind_device_name: bind_device_name.clone(),
        fastresume: settings.fastresume,
        persistence: Some(SessionPersistenceConfig::Json {
            folder: Some(data_dir.to_path_buf()),
        }),
        listen: Some(ListenerOptions {
            mode: ListenerMode::TcpOnly,
            listen_addr: (Ipv6Addr::UNSPECIFIED, settings.listen_port).into(),
            enable_upnp_port_forwarding: settings.enable_upnp,
            ..Default::default()
        }),
        ratelimits: LimitsConfig {
            download_bps: to_non_zero_u32(settings.download_limit_bps),
            upload_bps: to_non_zero_u32(settings.upload_limit_bps),
        },
        blocklist_url,
        peer_limit: settings.max_peers_per_torrent.map(|n| n.get()),
        disable_local_service_discovery: !settings.enable_lsd || !settings.enable_pex,
        client_name_and_version: Some(format!("Ruscello {}", env!("CARGO_PKG_VERSION"))),
        ..Default::default()
    };

    Session::new_with_opts(default_destination.to_path_buf(), opts)
        .await
        .map_err(|error| map_session_init_error(&error.to_string(), bind_device_name.as_deref()))
}

fn map_session_init_error(message: &str, bind_interface: Option<&str>) -> TorrentError {
    if let Some(iface) = bind_interface
        && message.contains("bind device")
    {
        return TorrentError::InvalidInterface(iface.to_owned());
    }
    if message.contains("blocklist") {
        return TorrentError::Blocklist(message.to_owned());
    }
    TorrentError::Session(message.to_owned())
}

async fn pause_all_restored_torrents(session: &Arc<Session>) {
    let handles: Vec<ManagedTorrentHandle> =
        session.with_torrents(|iter| iter.map(|(_, h)| h.clone()).collect());
    for handle in handles {
        let _ = session.pause(&handle).await;
    }
}

fn apply_live_ratelimits(session: &Session, settings: &TorrentSettings) {
    session
        .ratelimits
        .set_download_bps(to_non_zero_u32(settings.download_limit_bps));
    session
        .ratelimits
        .set_upload_bps(to_non_zero_u32(settings.upload_limit_bps));
}

fn to_non_zero_u32(value: Option<std::num::NonZeroU64>) -> Option<NonZeroU32> {
    value.and_then(|v| {
        let clamped = u32::try_from(v.get()).unwrap_or(u32::MAX).max(1);
        NonZeroU32::new(clamped)
    })
}

fn resolve_blocklist_url(raw: Option<&str>) -> Result<Option<String>, TorrentError> {
    let Some(trimmed) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("file://")
    {
        return Ok(Some(trimmed.to_owned()));
    }
    let abs = std::path::absolute(trimmed).map_err(|error| {
        TorrentError::Blocklist(format!("invalid blocklist path '{trimmed}': {error}"))
    })?;
    let url = reqwest::Url::from_file_path(&abs).map_err(|()| {
        TorrentError::Blocklist(format!(
            "could not convert blocklist path '{}' to file URL",
            abs.display()
        ))
    })?;
    Ok(Some(url.into()))
}

async fn fetch_torrent_url(
    client: &reqwest::Client,
    url: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, TorrentError> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(TorrentError::InvalidSource(format!(
            "unsupported torrent URL scheme: {url}"
        )));
    }
    let response = tokio::select! {
        _ = cancellation.cancelled() => return Err(TorrentError::Cancelled),
        res = client.get(url).send() => res.map_err(|e| TorrentError::Http(e.to_string()))?,
    };
    let response = response
        .error_for_status()
        .map_err(|e| TorrentError::Http(e.to_string()))?;
    let bytes = tokio::select! {
        _ = cancellation.cancelled() => return Err(TorrentError::Cancelled),
        res = response.bytes() => res.map_err(|e| TorrentError::Http(e.to_string()))?,
    };
    Ok(bytes.to_vec())
}

/// Parses `.torrent` bytes synchronously without touching the network.
pub fn inspect_torrent_bytes(bytes: &[u8]) -> Result<TorrentInspection, TorrentError> {
    let parsed = torrent_from_bytes(bytes)
        .map_err(|error| TorrentError::InvalidSource(format!("{error:#}")))?;
    let info_hash = parsed.info_hash.as_string();
    let trackers = parsed
        .iter_announce()
        .filter_map(|raw| std::str::from_utf8(raw.as_ref()).ok().map(str::to_owned))
        .collect::<Vec<_>>();
    let validated = parsed
        .info
        .data
        .clone_to_owned(None)
        .validate()
        .map_err(|error| TorrentError::InvalidSource(format!("{error:#}")))?;
    build_inspection_from_validated(
        info_hash,
        &validated,
        None,
        None,
        trackers,
        Some(bytes.to_vec()),
    )
}

fn build_inspection_from_validated(
    info_hash: String,
    info: &ValidatedTorrentMetaV1Info<ByteBufOwned>,
    only_files: Option<&[usize]>,
    fallback_name: Option<&str>,
    trackers: Vec<String>,
    torrent_bytes: Option<Vec<u8>>,
) -> Result<TorrentInspection, TorrentError> {
    let mut files = Vec::new();
    let mut total_bytes = 0u64;
    let mut selected_bytes = 0u64;

    for (index, detail) in info.iter_file_details().enumerate() {
        let path = detail.filename.to_pathbuf();
        let length = detail.len;
        let selected = only_files.is_none_or(|list| list.contains(&index));
        total_bytes = total_bytes.saturating_add(length);
        if selected {
            selected_bytes = selected_bytes.saturating_add(length);
        }
        files.push(TorrentInspectFile {
            index,
            path,
            length,
            selected,
        });
    }

    let raw_name = info
        .name()
        .map(|s| s.into_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| fallback_name.map(str::to_owned))
        .or_else(|| {
            files
                .first()
                .and_then(|f| f.path.file_name())
                .and_then(|n| n.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| info_hash.clone());

    let suggested_subfolder = if files.len() > 1 {
        sanitize_single_subfolder(&raw_name)
    } else {
        None
    };

    let lengths = info.lengths();
    Ok(TorrentInspection {
        info_hash,
        name: raw_name,
        total_bytes,
        selected_bytes,
        piece_length: lengths.default_piece_length(),
        piece_count: lengths.total_pieces(),
        is_private: info.info().private,
        files,
        trackers,
        suggested_subfolder,
        torrent_bytes,
    })
}

fn sanitize_single_subfolder(name: &str) -> Option<PathBuf> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let pb = Path::new(trimmed);
    let mut components = pb.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) => Some(PathBuf::from(component)),
        _ => None,
    }
}

fn resolve_output_folder(
    destination: &Path,
    explicit_resolved: Option<&Path>,
    suggested_subfolder: Option<&Path>,
) -> PathBuf {
    if let Some(resolved) = explicit_resolved {
        return resolved.to_path_buf();
    }
    match suggested_subfolder {
        Some(sub) => {
            if destination.file_name() == sub.file_name() {
                destination.to_path_buf()
            } else {
                destination.join(sub)
            }
        }
        None => destination.to_path_buf(),
    }
}

fn compute_completed_path(
    output_folder: &Path,
    is_single_file: bool,
    single_file_rel: Option<&Path>,
) -> PathBuf {
    if is_single_file && let Some(rel) = single_file_rel {
        output_folder.join(rel)
    } else {
        output_folder.to_path_buf()
    }
}

fn compute_selected_bytes(files: &[TorrentInspectFile], only_files: Option<&[usize]>) -> u64 {
    files
        .iter()
        .filter(|file| only_files.is_none_or(|list| list.contains(&file.index)))
        .map(|file| file.length)
        .sum()
}

fn tracker_protocol(url: &str) -> String {
    url.split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_uppercase())
        .unwrap_or_else(|| "UNKNOWN".into())
}

async fn remove_dir_or_file_if_exists(
    output_folder: &Path,
    is_single_file: bool,
    single_file_rel: Option<&Path>,
) -> Result<(), TorrentError> {
    if is_single_file {
        if let Some(rel) = single_file_rel {
            let file_path = output_folder.join(rel);
            match tokio::fs::remove_file(&file_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    } else if output_folder.exists() {
        match tokio::fs::remove_dir_all(output_folder).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
