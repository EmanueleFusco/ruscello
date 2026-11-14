//! `ruscello-api` — local loopback REST/WS/SSE server, LAN remote Web UI, and browser extension pairing.

mod auth;
mod error;
mod lan;
mod routes;
mod server;
mod state;
mod telemetry;
mod types;
mod webui;

pub use auth::{
    LocalApiEndpoint, discover_local_endpoint, hash_password, hash_password_async, verify_password,
    verify_password_async,
};
pub use error::ApiError;
pub use lan::{discover_lan_ip, discover_lan_ip_async};
pub use server::{
    ApiConfig, ApiController, ApiHandle, ApiRuntimeUpdate, ApiServer, DEFAULT_LAN_PORT,
    DEFAULT_LOCAL_PORT,
};
pub use types::{
    AddDownloadsRequest, AddDownloadsResponse, AltSpeedRequest, AltSpeedResponse,
    AnalyzeLinkRequest, AnalyzeLinkResponse, AnalyzedCandidateDto, ApiEvent, BulkActionResponse,
    DownloadDto, ForwardCookiesRequest, ForwardCookiesResponse, HealthResponse, LanStatus,
    LoginRequest, LoginResponse, PackageDto, PairingOverview, PairingRequestBody, PairingResponse,
    PendingPairingRequest, QueueSnapshotDto, StreamEnvelope,
};
