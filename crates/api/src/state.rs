use std::{
    collections::HashMap,
    net::IpAddr,
    num::NonZeroU64,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ruscello_engine::EngineHandle;
use ruscello_platform::language::Language;
use tokio::sync::broadcast;

use crate::{
    auth::{
        RateLimiter, SessionStore, load_extension_token_hashes, random_pin_6, random_token_hex,
        save_extension_token_hashes, sha256_hex,
    },
    error::ApiError,
    telemetry::TelemetryHub,
    types::{ApiEvent, LanStatus, PairingOverview, PendingPairingRequest},
};

const PAIRING_WINDOW_SECS: u64 = 120;
const MAX_PENDING_PAIRINGS: usize = 8;

#[derive(Debug, Clone)]
pub(crate) struct PendingEntry {
    pub(crate) request_id: String,
    pub(crate) client_name: String,
    pub(crate) origin: String,
    pub(crate) state: PendingStatus,
    pub(crate) expires_at: Instant,
}

#[derive(Debug, Clone)]
pub(crate) enum PendingStatus {
    Pending,
    Approved(String),
    Rejected,
}

#[derive(Debug)]
pub(crate) struct AuthState {
    pub(crate) data_dir: Option<PathBuf>,
    pub(crate) cli_token_hash: String,
    pub(crate) extension_token_hashes: Vec<String>,
    pub(crate) lan_auth_required: bool,
    pub(crate) lan_password_hash: Option<String>,
    pub(crate) alternate_bandwidth: NonZeroU64,
    pub(crate) locale: Language,
    pub(crate) allow_dlc_fallback: bool,
    pub(crate) sessions: SessionStore,
    pub(crate) rate_limiter: RateLimiter,
    pub(crate) pairing_deadline: Option<Instant>,
    pub(crate) pairing_pin: Option<String>,
    pub(crate) pending_pairings: HashMap<String, PendingEntry>,
    pub(crate) lan_status: LanStatus,
}

impl AuthState {
    pub(crate) fn is_token_authorized(&self, bearer: &str, allow_extension: bool) -> bool {
        let digest = sha256_hex(bearer);
        if crate::auth::constant_time_eq(digest.as_bytes(), self.cli_token_hash.as_bytes()) {
            return true;
        }
        allow_extension
            && self
                .extension_token_hashes
                .iter()
                .any(|hash| crate::auth::constant_time_eq(digest.as_bytes(), hash.as_bytes()))
    }

    pub(crate) fn issue_extension_token(&mut self) -> Result<String, ApiError> {
        let token = random_token_hex(32)?;
        self.extension_token_hashes.push(sha256_hex(&token));
        if let Some(dir) = &self.data_dir {
            save_extension_token_hashes(dir, &self.extension_token_hashes)?;
        }
        Ok(token)
    }

    pub(crate) fn pairing_overview(&mut self, now: Instant) -> PairingOverview {
        self.prune_pairing(now);
        let (window_active, remaining_secs, pin) = match self.pairing_deadline {
            Some(deadline) if deadline > now => (
                true,
                deadline.saturating_duration_since(now).as_secs().max(1),
                self.pairing_pin.clone(),
            ),
            _ => (false, 0, None),
        };
        let pending = self
            .pending_pairings
            .values()
            .filter(|entry| matches!(entry.state, PendingStatus::Pending))
            .map(|entry| PendingPairingRequest {
                request_id: entry.request_id.clone(),
                client_name: entry.client_name.clone(),
                origin: entry.origin.clone(),
            })
            .collect();
        PairingOverview {
            window_active,
            remaining_secs,
            pin,
            pending,
            paired_count: self.extension_token_hashes.len(),
        }
    }

    pub(crate) fn prune_pairing(&mut self, now: Instant) {
        if self
            .pairing_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            self.pairing_deadline = None;
            self.pairing_pin = None;
        }
        self.pending_pairings
            .retain(|_, entry| entry.expires_at > now);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListenerKind {
    Loopback,
    Lan,
}

#[derive(Clone, Debug)]
pub(crate) struct SharedServer {
    pub(crate) engine: EngineHandle,
    pub(crate) telemetry: TelemetryHub,
    pub(crate) auth: Arc<Mutex<AuthState>>,
    pub(crate) events_tx: broadcast::Sender<ApiEvent>,
    pub(crate) local_port: u16,
}

impl SharedServer {
    pub(crate) fn new(
        engine: EngineHandle,
        telemetry: TelemetryHub,
        auth: AuthState,
        local_port: u16,
    ) -> Self {
        let (events_tx, _) = broadcast::channel(64);
        Self {
            engine,
            telemetry,
            auth: Arc::new(Mutex::new(auth)),
            events_tx,
            local_port,
        }
    }

    pub(crate) fn emit(&self, event: ApiEvent) {
        let _ = self.events_tx.send(event);
    }

    pub(crate) fn pairing_overview(&self) -> PairingOverview {
        let now = Instant::now();
        self.auth
            .lock()
            .map(|mut guard| guard.pairing_overview(now))
            .unwrap_or(PairingOverview {
                window_active: false,
                remaining_secs: 0,
                pin: None,
                pending: Vec::new(),
                paired_count: 0,
            })
    }

    pub(crate) fn start_pairing_window(&self) -> Result<PairingOverview, ApiError> {
        let pin = random_pin_6()?;
        let now = Instant::now();
        let overview = {
            let mut guard = self
                .auth
                .lock()
                .map_err(|_| ApiError::BadRequest("lock poisoned".to_owned()))?;
            guard.pairing_deadline = Some(now + Duration::from_secs(PAIRING_WINDOW_SECS));
            guard.pairing_pin = Some(pin);
            guard.pairing_overview(now)
        };
        self.emit(ApiEvent::PairingChanged(overview.clone()));
        Ok(overview)
    }

    pub(crate) fn cancel_pairing_window(&self) {
        let now = Instant::now();
        let overview = self.auth.lock().ok().map(|mut guard| {
            guard.pairing_deadline = None;
            guard.pairing_pin = None;
            guard.pairing_overview(now)
        });
        if let Some(overview) = overview {
            self.emit(ApiEvent::PairingChanged(overview));
        }
    }

    pub(crate) fn approve_pairing(&self, request_id: &str) -> Result<PairingOverview, ApiError> {
        let now = Instant::now();
        let overview = {
            let mut guard = self
                .auth
                .lock()
                .map_err(|_| ApiError::BadRequest("lock poisoned".to_owned()))?;
            guard.prune_pairing(now);
            let exists = guard.pending_pairings.contains_key(request_id);
            if !exists {
                return Err(ApiError::NotFound);
            }
            let token = guard.issue_extension_token()?;
            if let Some(entry) = guard.pending_pairings.get_mut(request_id) {
                entry.state = PendingStatus::Approved(token);
            }
            guard.pairing_overview(now)
        };
        self.emit(ApiEvent::PairingChanged(overview.clone()));
        Ok(overview)
    }

    pub(crate) fn reject_pairing(&self, request_id: &str) -> Result<PairingOverview, ApiError> {
        let now = Instant::now();
        let overview = {
            let mut guard = self
                .auth
                .lock()
                .map_err(|_| ApiError::BadRequest("lock poisoned".to_owned()))?;
            guard.prune_pairing(now);
            let entry = guard
                .pending_pairings
                .get_mut(request_id)
                .ok_or(ApiError::NotFound)?;
            entry.state = PendingStatus::Rejected;
            guard.pairing_overview(now)
        };
        self.emit(ApiEvent::PairingChanged(overview.clone()));
        Ok(overview)
    }

    pub(crate) fn revoke_extension_tokens(&self) -> Result<usize, ApiError> {
        let now = Instant::now();
        let (removed, overview) = {
            let mut guard = self
                .auth
                .lock()
                .map_err(|_| ApiError::BadRequest("lock poisoned".to_owned()))?;
            let count = guard.extension_token_hashes.len();
            guard.extension_token_hashes.clear();
            guard.pending_pairings.clear();
            if let Some(dir) = &guard.data_dir {
                save_extension_token_hashes(dir, &guard.extension_token_hashes)?;
            }
            (count, guard.pairing_overview(now))
        };
        self.emit(ApiEvent::PairingChanged(overview));
        Ok(removed)
    }

    pub(crate) fn request_pairing(
        &self,
        ip: IpAddr,
        origin: &str,
        client_name: Option<String>,
        submitted_pin: Option<&str>,
    ) -> Result<PairingRequestOutcome, ApiError> {
        let now = Instant::now();
        let client_name = client_name
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Browser extension".to_owned());

        let (outcome, overview) = {
            let mut guard = self
                .auth
                .lock()
                .map_err(|_| ApiError::BadRequest("lock poisoned".to_owned()))?;
            guard.prune_pairing(now);
            if let Err(retry_after) = guard.rate_limiter.check(ip, now) {
                return Err(ApiError::RateLimited(retry_after));
            }

            let window_active = guard
                .pairing_deadline
                .is_some_and(|deadline| deadline > now);

            let mut paired = None;
            if window_active {
                let is_ext_origin = crate::auth::is_allowed_extension_origin(origin);
                let pin_matches = match (submitted_pin.map(str::trim), &guard.pairing_pin) {
                    // Extension origin can complete 1-click pairing while window is armed;
                    // non-extension local callers must present the active PIN.
                    (None | Some(""), _) => is_ext_origin,
                    (Some(submitted), Some(expected)) => {
                        crate::auth::constant_time_eq(submitted.as_bytes(), expected.as_bytes())
                    }
                    _ => false,
                };
                if !pin_matches {
                    if is_ext_origin || submitted_pin.is_some() {
                        guard.rate_limiter.record_failure(ip, now);
                        return Err(ApiError::Unauthorized);
                    }
                    // Headerless client without PIN falls through to desktop approval queue
                } else {
                    guard.rate_limiter.reset(ip);
                    guard.pairing_deadline = None;
                    guard.pairing_pin = None;
                    let token = guard.issue_extension_token()?;
                    let overview = guard.pairing_overview(now);
                    paired = Some((PairingRequestOutcome::Paired(token), overview));
                }
            }
            if let Some(result) = paired {
                result
            } else {
                // Extension-initiated pairing -> pending approval in desktop Settings
                if guard.pending_pairings.len() >= MAX_PENDING_PAIRINGS {
                    guard.rate_limiter.record_failure(ip, now);
                    return Err(ApiError::RateLimited(15));
                }
                let request_id = random_token_hex(16)?;
                guard.pending_pairings.insert(
                    request_id.clone(),
                    PendingEntry {
                        request_id: request_id.clone(),
                        client_name,
                        origin: origin.to_owned(),
                        state: PendingStatus::Pending,
                        expires_at: now + Duration::from_secs(PAIRING_WINDOW_SECS),
                    },
                );
                let overview = guard.pairing_overview(now);
                (PairingRequestOutcome::Pending(request_id), overview)
            }
        };

        self.emit(ApiEvent::PairingChanged(overview));
        Ok(outcome)
    }

    pub(crate) fn initial_extension_hashes(
        data_dir: Option<&PathBuf>,
        seed: Vec<String>,
    ) -> Vec<String> {
        let mut hashes = data_dir
            .map(|dir| load_extension_token_hashes(dir.as_path()))
            .unwrap_or_default();
        for h in seed {
            if !hashes.contains(&h) {
                hashes.push(h);
            }
        }
        hashes
    }
}

pub(crate) enum PairingRequestOutcome {
    Paired(String),
    Pending(String),
}
