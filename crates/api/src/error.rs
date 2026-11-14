use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Errors returned by `ruscello-api` startup, configuration, and HTTP handlers.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("failed to bind API listener: {0}")]
    Bind(#[source] std::io::Error),
    #[error("invalid port configuration")]
    InvalidPort,
    #[error("random number generator failed: {0}")]
    Random(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("engine error: {0}")]
    Engine(#[from] ruscello_engine::EngineError),
    #[error("invalid request: {0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("access denied")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("too many attempts")]
    RateLimited(u64),
    #[error("background task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

pub(crate) fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(ErrorBody { error: message })).into_response()
}

pub(crate) fn rate_limited_response(retry_after_secs: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after_secs.to_string())],
        Json(ErrorBody {
            error: "too many failed attempts",
        }),
    )
        .into_response()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => json_error(StatusCode::UNAUTHORIZED, "authentication required"),
            Self::Forbidden => json_error(StatusCode::FORBIDDEN, "access denied"),
            Self::NotFound => json_error(StatusCode::NOT_FOUND, "not found"),
            Self::RateLimited(retry_after) => rate_limited_response(retry_after),
            Self::BadRequest(msg) => json_error(StatusCode::BAD_REQUEST, &msg),
            Self::Engine(ruscello_engine::EngineError::UnknownDownload) => {
                json_error(StatusCode::NOT_FOUND, "download not found")
            }
            Self::Engine(ruscello_engine::EngineError::InvalidUrl) => {
                json_error(StatusCode::BAD_REQUEST, "invalid URL")
            }
            Self::Engine(error) => {
                tracing::debug!(?error, "api engine error");
                json_error(StatusCode::BAD_REQUEST, &error.to_string())
            }
            error => {
                tracing::debug!(?error, "api internal error");
                json_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
            }
        }
    }
}
