use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use ruscello_core::{Download, DownloadId, PackageId, Source, Status};
use ruscello_engine::{
    EngineEvent, EngineHandle, EngineSnapshot, ProgressUpdate, TorrentRuntimeProgress,
    TorrentSessionTelemetry,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::types::{
    DownloadDto, PackageDto, QueueSnapshotDto, display_name, failure_name, file_type_name,
    source_summary, status_name,
};

#[derive(Debug, Clone, Copy)]
struct RateSample {
    at: Instant,
    bytes: u64,
    speed_bps: u64,
}

#[derive(Debug, Default)]
struct TelemetryState {
    downloads: BTreeMap<DownloadId, Download>,
    packages: BTreeMap<PackageId, ruscello_core::Package>,
    http_rates: HashMap<DownloadId, RateSample>,
    torrent_rates: HashMap<DownloadId, TorrentRuntimeProgress>,
    torrent_session: TorrentSessionTelemetry,
    limits: ruscello_engine::Limits,
    temporary_bandwidth: Option<std::num::NonZeroU64>,
}

impl TelemetryState {
    fn apply_snapshot(&mut self, snapshot: EngineSnapshot) {
        self.downloads = snapshot
            .downloads
            .into_iter()
            .map(|dl| (dl.id, dl))
            .collect();
        self.packages = snapshot
            .packages
            .into_iter()
            .map(|pkg| (pkg.id, pkg))
            .collect();
        self.limits = snapshot.limits;
        self.temporary_bandwidth = snapshot.temporary_bandwidth;
        self.torrent_session = snapshot.telemetry;
        let ids = &self.downloads;
        self.http_rates.retain(|id, _| ids.contains_key(id));
        self.torrent_rates.retain(|id, _| ids.contains_key(id));
    }

    fn apply_progress(&mut self, updates: Vec<ProgressUpdate>, now: Instant) {
        for update in updates {
            let Some(dl) = self.downloads.get_mut(&update.id) else {
                continue;
            };
            let prev = self.http_rates.get(&update.id);
            let speed_bps = prev
                .filter(|_| !update.progress.restarted && !update.progress.waiting_for_network)
                .map_or(0, |sample| {
                    let elapsed = now.saturating_duration_since(sample.at).as_secs_f64();
                    if elapsed > 0.0 {
                        (update.progress.completed.saturating_sub(sample.bytes) as f64 / elapsed)
                            as u64
                    } else {
                        sample.speed_bps
                    }
                });
            dl.downloaded_bytes = update.progress.completed;
            dl.total_bytes = update.progress.total;
            self.http_rates.insert(
                update.id,
                RateSample {
                    at: now,
                    bytes: update.progress.completed,
                    speed_bps,
                },
            );
        }
    }

    fn apply_torrent_progress(&mut self, updates: Vec<TorrentRuntimeProgress>) {
        for update in updates {
            self.torrent_rates.insert(update.id, update);
        }
    }

    fn to_dto(&self) -> QueueSnapshotDto {
        let mut total_down = 0_u64;
        let mut total_up = 0_u64;
        let mut downloads = Vec::with_capacity(self.downloads.len());

        for dl in self.downloads.values() {
            let is_torrent = matches!(dl.source, Source::Torrent(_));
            let active = dl.status().is_active();
            let (down_bps, up_bps, eta_secs) = if is_torrent {
                self.torrent_rates
                    .get(&dl.id)
                    .filter(|_| active)
                    .map_or((0, 0, None), |sample| {
                        (
                            sample.download_rate_bps,
                            sample.upload_rate_bps,
                            sample.eta_secs,
                        )
                    })
            } else {
                let speed = self
                    .http_rates
                    .get(&dl.id)
                    .filter(|_| dl.status() == Status::Downloading)
                    .map_or(0, |sample| sample.speed_bps);
                let eta = dl.total_bytes.and_then(|total| {
                    if speed > 0 && total > dl.downloaded_bytes {
                        Some((total - dl.downloaded_bytes) / speed)
                    } else {
                        None
                    }
                });
                (speed, 0, eta)
            };

            total_down = total_down.saturating_add(down_bps);
            total_up = total_up.saturating_add(up_bps);

            let progress_percent = match dl.total_bytes {
                Some(total) if total > 0 => {
                    ((dl.downloaded_bytes as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
                }
                _ if dl.status() == Status::Completed => 100.0,
                _ => 0.0,
            };

            let failure_reason = match dl.status() {
                Status::Failed { reason } => Some(failure_name(reason).to_owned()),
                _ => None,
            };

            downloads.push(DownloadDto {
                id: dl.id.0,
                package_id: dl.package.map(|p| p.0),
                filename: display_name(dl),
                source_summary: source_summary(&dl.source),
                file_type: file_type_name(dl.file_type.unwrap_or(ruscello_core::FileType::Other))
                    .to_owned(),
                status: status_name(dl.status()).to_owned(),
                downloaded_bytes: dl.downloaded_bytes,
                total_bytes: dl.total_bytes,
                progress_percent,
                download_bps: down_bps,
                upload_bps: up_bps,
                eta_secs,
                failure_reason,
            });
        }

        total_up = total_up.max(self.torrent_session.upload_speed_bps);

        let packages = self
            .packages
            .values()
            .map(|pkg| PackageDto {
                id: pkg.id.0,
                name: pkg.name.clone(),
            })
            .collect();

        QueueSnapshotDto {
            downloads,
            packages,
            total_download_bps: total_down,
            total_upload_bps: total_up,
            alternate_speed_enabled: self.temporary_bandwidth.is_some(),
            alternate_bandwidth_bps: self.temporary_bandwidth.map(std::num::NonZeroU64::get),
            global_bandwidth_bps: self.limits.bandwidth.map(std::num::NonZeroU64::get),
        }
    }
}

/// Maintains a real-time synchronized snapshot of engine queue state and rates.
#[derive(Clone, Debug)]
pub(crate) struct TelemetryHub {
    engine: EngineHandle,
    state: Arc<RwLock<TelemetryState>>,
    updates_tx: broadcast::Sender<QueueSnapshotDto>,
}

impl TelemetryHub {
    pub(crate) async fn start(engine: EngineHandle, shutdown: CancellationToken) -> Self {
        let state = Arc::new(RwLock::new(TelemetryState::default()));
        let (updates_tx, _) = broadcast::channel(64);
        let hub = Self {
            engine,
            state,
            updates_tx,
        };
        let _ = hub.refresh_snapshot().await;
        tokio::spawn(run_telemetry_loop(hub.clone(), shutdown));
        hub
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<QueueSnapshotDto> {
        self.updates_tx.subscribe()
    }

    pub(crate) fn current_dto(&self) -> QueueSnapshotDto {
        self.state
            .read()
            .map(|state| state.to_dto())
            .unwrap_or_default()
    }

    pub(crate) async fn refresh_snapshot(&self) -> Option<QueueSnapshotDto> {
        let snapshot = self.engine.snapshot().await.ok()?;
        let dto = if let Ok(mut state) = self.state.write() {
            state.apply_snapshot(snapshot);
            state.to_dto()
        } else {
            return None;
        };
        let _ = self.updates_tx.send(dto.clone());
        Some(dto)
    }

    fn handle_event(&self, event: EngineEvent, now: Instant) -> bool {
        match event {
            EngineEvent::Added(dl) => {
                if let Ok(mut state) = self.state.write() {
                    state.downloads.insert(dl.id, *dl);
                    let dto = state.to_dto();
                    let _ = self.updates_tx.send(dto);
                }
                false
            }
            EngineEvent::Removed(id) => {
                if let Ok(mut state) = self.state.write() {
                    state.downloads.remove(&id);
                    state.http_rates.remove(&id);
                    state.torrent_rates.remove(&id);
                    let dto = state.to_dto();
                    let _ = self.updates_tx.send(dto);
                }
                false
            }
            EngineEvent::Progress(updates) => {
                if let Ok(mut state) = self.state.write() {
                    state.apply_progress(updates, now);
                    let dto = state.to_dto();
                    let _ = self.updates_tx.send(dto);
                }
                false
            }
            EngineEvent::TorrentProgress(updates) => {
                if let Ok(mut state) = self.state.write() {
                    state.apply_torrent_progress(updates);
                    let dto = state.to_dto();
                    let _ = self.updates_tx.send(dto);
                }
                false
            }
            EngineEvent::TorrentTelemetry(telemetry) => {
                if let Ok(mut state) = self.state.write() {
                    state.torrent_session = telemetry;
                    let dto = state.to_dto();
                    let _ = self.updates_tx.send(dto);
                }
                false
            }
            EngineEvent::StatusChanged { id, status } => {
                let updated = if let Ok(mut state) = self.state.write() {
                    if let Some(dl) = state.downloads.get_mut(&id) {
                        let _ = dl.transition(status);
                        let dto = state.to_dto();
                        let _ = self.updates_tx.send(dto);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                !updated
            }
            EngineEvent::QueueChanged
            | EngineEvent::AutomationUpdated(_)
            | EngineEvent::ScheduleChanged(_) => true,
            EngineEvent::PostProcessing { .. }
            | EngineEvent::QueueCompleted { .. }
            | EngineEvent::CompletionCountdown { .. }
            | EngineEvent::CompletionCountdownCancelled
            | EngineEvent::Log { .. } => false,
        }
    }
}

async fn run_telemetry_loop(hub: TelemetryHub, shutdown: CancellationToken) {
    let mut rx = hub.engine.subscribe();
    let mut last_broadcast = Instant::now() - Duration::from_secs(1);

    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            received = rx.recv() => {
                match received {
                    Ok(event) => {
                        let needs_refresh = hub.handle_event(event, Instant::now());
                        if needs_refresh {
                            let _ = hub.refresh_snapshot().await;
                            last_broadcast = Instant::now();
                        } else if last_broadcast.elapsed() >= Duration::from_millis(250) {
                            last_broadcast = Instant::now();
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = hub.refresh_snapshot().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}
