use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    num::NonZeroU64,
    path::PathBuf,
};

use ruscello_engine::EngineHandle;
use ruscello_platform::language::Language;
use tokio::{net::TcpListener, sync::broadcast, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{
        LocalApiEndpoint, random_token_hex, remove_local_endpoint, sha256_hex, write_local_endpoint,
    },
    error::ApiError,
    lan::discover_lan_ip_async,
    routes::{lan_router, loopback_router},
    state::{AuthState, SharedServer},
    telemetry::TelemetryHub,
    types::{ApiEvent, LanStatus, PairingOverview},
};

/// Default loopback port for the browser extension and CLI API.
pub const DEFAULT_LOCAL_PORT: u16 = 9667;
/// Default LAN port for the embedded remote control Web UI.
pub const DEFAULT_LAN_PORT: u16 = 9666;

/// Initial configuration for starting [`ApiServer`].
#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub data_dir: Option<PathBuf>,
    pub local_port: u16,
    pub fallback_to_ephemeral_local_port: bool,
    pub cli_token: Option<String>,
    pub extension_token_hashes: Vec<String>,
    pub lan_enabled: bool,
    pub lan_port: u16,
    pub lan_auth_required: bool,
    pub lan_password_hash: Option<String>,
    pub alternate_bandwidth: NonZeroU64,
    pub locale: Language,
    pub allow_dlc_fallback: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            data_dir: None,
            local_port: DEFAULT_LOCAL_PORT,
            fallback_to_ephemeral_local_port: true,
            cli_token: None,
            extension_token_hashes: Vec::new(),
            lan_enabled: false,
            lan_port: DEFAULT_LAN_PORT,
            lan_auth_required: true,
            lan_password_hash: None,
            alternate_bandwidth: NonZeroU64::MIN.saturating_add(512 * 1024 - 1),
            locale: Language::English,
            allow_dlc_fallback: true,
        }
    }
}

/// Live-updatable LAN and telemetry settings applied without restarting the loopback listener.
#[derive(Debug, Clone)]
pub struct ApiRuntimeUpdate {
    pub lan_enabled: bool,
    pub lan_port: u16,
    pub lan_auth_required: bool,
    pub lan_password_hash: Option<String>,
    pub alternate_bandwidth: NonZeroU64,
    pub locale: Language,
}

/// Entrypoint for launching the local and LAN API servers.
#[derive(Debug, Clone, Copy)]
pub struct ApiServer;

impl ApiServer {
    /// Starts the loopback API server (and optional LAN listener) bound to `engine`.
    pub async fn start(engine: EngineHandle, config: ApiConfig) -> Result<ApiHandle, ApiError> {
        let cli_token = match config.cli_token {
            Some(token) if !token.trim().is_empty() => token,
            _ => random_token_hex(32)?,
        };
        let cli_token_hash = sha256_hex(&cli_token);

        let local_listener =
            bind_loopback(config.local_port, config.fallback_to_ephemeral_local_port).await?;
        let local_addr = local_listener.local_addr().map_err(ApiError::Bind)?;

        if let Some(dir) = &config.data_dir {
            write_local_endpoint(
                dir,
                &LocalApiEndpoint {
                    port: local_addr.port(),
                    pid: std::process::id(),
                    token: cli_token.clone(),
                },
            )?;
        }

        let shutdown = CancellationToken::new();
        let telemetry = TelemetryHub::start(engine.clone(), shutdown.child_token()).await;

        let ext_hashes = SharedServer::initial_extension_hashes(
            config.data_dir.as_ref(),
            config.extension_token_hashes,
        );

        let auth = AuthState {
            data_dir: config.data_dir.clone(),
            cli_token_hash,
            extension_token_hashes: ext_hashes,
            lan_auth_required: config.lan_auth_required,
            lan_password_hash: config.lan_password_hash.clone(),
            alternate_bandwidth: config.alternate_bandwidth,
            locale: config.locale,
            allow_dlc_fallback: config.allow_dlc_fallback,
            sessions: crate::auth::SessionStore::default(),
            rate_limiter: crate::auth::RateLimiter::default(),
            pairing_deadline: None,
            pairing_pin: None,
            pending_pairings: std::collections::HashMap::new(),
            lan_status: LanStatus::Disabled,
        };

        let server = SharedServer::new(engine, telemetry, auth, local_addr.port());

        let loopback_app = loopback_router(server.clone(), shutdown.child_token());
        let loopback_cancel = shutdown.child_token();
        let loopback_task = tokio::spawn(async move {
            let service = loopback_app.into_make_service_with_connect_info::<SocketAddr>();
            let _ = axum::serve(local_listener, service)
                .with_graceful_shutdown(loopback_cancel.cancelled_owned())
                .await;
        });

        let mut handle = ApiHandle {
            server,
            local_addr,
            cli_token,
            data_dir: config.data_dir,
            shutdown,
            loopback_task: Some(loopback_task),
            lan_runtime: None,
            active_lan_port: None,
        };

        handle
            .apply_update(ApiRuntimeUpdate {
                lan_enabled: config.lan_enabled,
                lan_port: config.lan_port,
                lan_auth_required: config.lan_auth_required,
                lan_password_hash: config.lan_password_hash,
                alternate_bandwidth: config.alternate_bandwidth,
                locale: config.locale,
            })
            .await;

        Ok(handle)
    }
}

struct LanRuntime {
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

/// Cloneable handle for subscribing to [`ApiEvent`]s and managing extension pairing.
#[derive(Debug, Clone)]
pub struct ApiController {
    server: SharedServer,
    local_addr: SocketAddr,
}

impl ApiController {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn lan_status(&self) -> LanStatus {
        self.server
            .auth
            .lock()
            .map(|guard| guard.lan_status.clone())
            .unwrap_or(LanStatus::Disabled)
    }

    pub fn pairing_status(&self) -> PairingOverview {
        self.server.pairing_overview()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApiEvent> {
        self.server.events_tx.subscribe()
    }

    pub fn controller(&self) -> ApiController {
        ApiController {
            server: self.server.clone(),
            local_addr: self.local_addr,
        }
    }

    pub fn start_pairing_window(&self) -> Result<PairingOverview, ApiError> {
        self.server.start_pairing_window()
    }

    pub fn cancel_pairing_window(&self) {
        self.server.cancel_pairing_window();
    }

    pub fn approve_pairing(&self, request_id: &str) -> Result<PairingOverview, ApiError> {
        self.server.approve_pairing(request_id)
    }

    pub fn reject_pairing(&self, request_id: &str) -> Result<PairingOverview, ApiError> {
        self.server.reject_pairing(request_id)
    }

    pub fn revoke_extension_tokens(&self) -> Result<usize, ApiError> {
        self.server.revoke_extension_tokens()
    }
}

/// Controlling handle for a running `ruscello-api` instance.
pub struct ApiHandle {
    server: SharedServer,
    local_addr: SocketAddr,
    cli_token: String,
    data_dir: Option<PathBuf>,
    shutdown: CancellationToken,
    loopback_task: Option<JoinHandle<()>>,
    lan_runtime: Option<LanRuntime>,
    active_lan_port: Option<u16>,
}

impl std::fmt::Debug for ApiHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiHandle")
            .field("local_addr", &self.local_addr)
            .field("active_lan_port", &self.active_lan_port)
            .finish_non_exhaustive()
    }
}

impl ApiHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn cli_token(&self) -> &str {
        &self.cli_token
    }

    pub fn lan_status(&self) -> LanStatus {
        self.server
            .auth
            .lock()
            .map(|guard| guard.lan_status.clone())
            .unwrap_or(LanStatus::Disabled)
    }

    pub fn pairing_status(&self) -> PairingOverview {
        self.server.pairing_overview()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApiEvent> {
        self.server.events_tx.subscribe()
    }

    pub fn controller(&self) -> ApiController {
        ApiController {
            server: self.server.clone(),
            local_addr: self.local_addr,
        }
    }

    /// Reconfigures LAN binding, authentication mode, alternate speed, and locale live.
    pub async fn apply_update(&mut self, update: ApiRuntimeUpdate) -> LanStatus {
        let should_rebind_lan = match (update.lan_enabled, self.active_lan_port) {
            (false, Some(_)) => {
                self.stop_lan().await;
                false
            }
            (false, None) => false,
            (true, Some(port)) if port == update.lan_port && self.lan_runtime.is_some() => false,
            (true, _) => {
                self.stop_lan().await;
                true
            }
        };

        let clear_sessions = if let Ok(mut guard) = self.server.auth.lock() {
            let changed_auth = guard.lan_auth_required != update.lan_auth_required
                || guard.lan_password_hash != update.lan_password_hash;
            guard.lan_auth_required = update.lan_auth_required;
            guard.lan_password_hash = update.lan_password_hash;
            guard.alternate_bandwidth = update.alternate_bandwidth;
            guard.locale = update.locale;
            if changed_auth {
                guard.sessions.clear();
            }
            if !update.lan_enabled {
                guard.lan_status = LanStatus::Disabled;
            }
            changed_auth
        } else {
            false
        };
        let _ = clear_sessions;

        if should_rebind_lan {
            let next_status = self.start_lan(update.lan_port).await;
            if let Ok(mut guard) = self.server.auth.lock() {
                guard.lan_status = next_status.clone();
            }
            self.server
                .emit(ApiEvent::LanStatusChanged(next_status.clone()));
            next_status
        } else {
            let current = self.lan_status();
            self.server
                .emit(ApiEvent::LanStatusChanged(current.clone()));
            current
        }
    }

    /// Opens a 120-second one-click pairing window and returns the generated PIN and state.
    pub fn start_pairing_window(&self) -> Result<PairingOverview, ApiError> {
        self.server.start_pairing_window()
    }

    pub fn cancel_pairing_window(&self) {
        self.server.cancel_pairing_window();
    }

    /// Approves a pending extension pairing request from the browser.
    pub fn approve_pairing(&self, request_id: &str) -> Result<PairingOverview, ApiError> {
        self.server.approve_pairing(request_id)
    }

    /// Rejects a pending extension pairing request.
    pub fn reject_pairing(&self, request_id: &str) -> Result<PairingOverview, ApiError> {
        self.server.reject_pairing(request_id)
    }

    /// Revokes all previously paired browser extension tokens.
    pub fn revoke_extension_tokens(&self) -> Result<usize, ApiError> {
        self.server.revoke_extension_tokens()
    }

    /// Gracefully shuts down both listeners and removes the local discovery file.
    pub async fn shutdown(mut self) {
        self.stop_lan().await;
        self.shutdown.cancel();
        if let Some(task) = self.loopback_task.take() {
            let _ = task.await;
        }
        if let Some(dir) = self.data_dir.take() {
            remove_local_endpoint(&dir);
        }
    }

    async fn start_lan(&mut self, port: u16) -> LanStatus {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port));
        let listener = match TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, port, "failed to bind LAN API listener");
                return LanStatus::Error(error.to_string());
            }
        };
        let bound_addr = match listener.local_addr() {
            Ok(addr) => addr,
            Err(error) => return LanStatus::Error(error.to_string()),
        };

        let lan_cancel = self.shutdown.child_token();
        let app = lan_router(self.server.clone(), lan_cancel.clone());
        let task_cancel = lan_cancel.clone();
        let task = tokio::spawn(async move {
            let service = app.into_make_service_with_connect_info::<SocketAddr>();
            let _ = axum::serve(listener, service)
                .with_graceful_shutdown(task_cancel.cancelled_owned())
                .await;
        });

        self.lan_runtime = Some(LanRuntime {
            shutdown: lan_cancel,
            task,
        });
        self.active_lan_port = Some(bound_addr.port());

        let host = discover_lan_ip_async()
            .await
            .map_or_else(|| "127.0.0.1".to_owned(), |ip| ip.to_string());
        let display_url = format!("http://{host}:{}", bound_addr.port());

        LanStatus::Active {
            bind_addr: bound_addr,
            display_url,
        }
    }

    async fn stop_lan(&mut self) {
        if let Some(runtime) = self.lan_runtime.take() {
            runtime.shutdown.cancel();
            let _ = runtime.task.await;
        }
        self.active_lan_port = None;
    }
}

impl Drop for ApiHandle {
    fn drop(&mut self) {
        if let Some(runtime) = self.lan_runtime.take() {
            runtime.shutdown.cancel();
        }
        self.shutdown.cancel();
    }
}

async fn bind_loopback(port: u16, fallback_ephemeral: bool) -> Result<TcpListener, ApiError> {
    let primary = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    match TcpListener::bind(primary).await {
        Ok(listener) => Ok(listener),
        Err(error) if fallback_ephemeral && port != 0 => {
            tracing::warn!(%error, port, "loopback port busy, falling back to ephemeral port");
            let ephemeral = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
            TcpListener::bind(ephemeral).await.map_err(ApiError::Bind)
        }
        Err(error) => Err(ApiError::Bind(error)),
    }
}
