use std::{
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{
        ConnectInfo, FromRequest, Multipart, Path, Query, State, WebSocketUpgrade,
        ws::{Message as WsMessage, WebSocket},
    },
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, post},
};
use base64::Engine as _;
use futures_util::stream::{self, Stream};
use ruscello_core::{DownloadId, Source, TorrentSource};
use ruscello_engine::{
    AddDownload, BrowserSession, MediaKind, MediaRequest, PageAnalysis, Url, is_dash_url,
    is_hls_url,
};
use ruscello_platform::language::Language;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{
        SESSION_COOKIE_NAME, is_allowed_extension_origin, random_token_hex, verify_password_async,
    },
    error::{ApiError, json_error, rate_limited_response},
    state::{ListenerKind, PairingRequestOutcome, PendingStatus, SharedServer},
    types::{
        AddDownloadsRequest, AddDownloadsResponse, AltSpeedRequest, AltSpeedResponse,
        AnalyzeLinkRequest, AnalyzeLinkResponse, ApiEvent, BulkActionResponse, DeleteQuery,
        ForwardCookiesRequest, ForwardCookiesResponse, HealthResponse, LoginRequest, LoginResponse,
        PairingRequestBody, PairingResponse, QueueSnapshotDto, StreamEnvelope, can_pause,
        can_resume, map_candidate,
    },
    webui,
};

const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct RouteCtx {
    pub(crate) server: SharedServer,
    pub(crate) listener: ListenerKind,
    pub(crate) shutdown: CancellationToken,
}

pub(crate) fn loopback_router(server: SharedServer, shutdown: CancellationToken) -> Router {
    let ctx = RouteCtx {
        server,
        listener: ListenerKind::Loopback,
        shutdown,
    };

    let protected = Router::new()
        .route("/api/v1/queue", get(get_queue))
        .route("/api/v1/state", get(get_queue))
        .route("/api/v1/events", get(sse_events))
        .route("/api/v1/ws", get(ws_events))
        .route("/api/v1/downloads", get(get_queue).post(add_downloads))
        .route("/api/v1/downloads/pause-all", post(pause_all))
        .route("/api/v1/downloads/resume-all", post(resume_all))
        .route("/api/v1/downloads/{id}/pause", post(pause_download))
        .route("/api/v1/downloads/{id}/resume", post(resume_download))
        .route(
            "/api/v1/downloads/{id}/force-start",
            post(force_start_download),
        )
        .route("/api/v1/downloads/{id}/cancel", post(cancel_download_post))
        .route("/api/v1/downloads/{id}", delete(cancel_download_delete))
        .route("/api/v1/speed/alternate", post(set_alternate_speed))
        .route("/api/v1/linkgrabber/analyze", post(analyze_link))
        .route("/api/v1/cookies", post(forward_cookies))
        .route_layer(middleware::from_fn_with_state(ctx.clone(), require_auth));

    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/pair/request", post(pair_request))
        .route("/api/v1/pair/status/{request_id}", get(pair_status))
        .merge(protected)
        .layer(middleware::from_fn(loopback_cors_middleware))
        .with_state(ctx)
}

pub(crate) fn lan_router(server: SharedServer, shutdown: CancellationToken) -> Router {
    let ctx = RouteCtx {
        server,
        listener: ListenerKind::Lan,
        shutdown,
    };

    let protected = Router::new()
        .route("/api/v1/queue", get(get_queue))
        .route("/api/v1/state", get(get_queue))
        .route("/api/v1/events", get(sse_events))
        .route("/api/v1/ws", get(ws_events))
        .route("/api/v1/downloads", get(get_queue).post(add_downloads))
        .route("/api/v1/downloads/pause-all", post(pause_all))
        .route("/api/v1/downloads/resume-all", post(resume_all))
        .route("/api/v1/downloads/{id}/pause", post(pause_download))
        .route("/api/v1/downloads/{id}/resume", post(resume_download))
        .route(
            "/api/v1/downloads/{id}/force-start",
            post(force_start_download),
        )
        .route("/api/v1/downloads/{id}/cancel", post(cancel_download_post))
        .route("/api/v1/downloads/{id}", delete(cancel_download_delete))
        .route("/api/v1/speed/alternate", post(set_alternate_speed))
        .route("/api/v1/linkgrabber/analyze", post(analyze_link))
        .route("/api/v1/cookies", post(forward_cookies))
        .route_layer(middleware::from_fn_with_state(ctx.clone(), require_auth));

    Router::new()
        .route("/", get(webui::index_html))
        .route("/index.html", get(webui::index_html))
        .route("/style.css", get(webui::style_css))
        .route("/app.js", get(webui::app_js))
        .route("/api/v1/health", get(health))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .merge(protected)
        .with_state(ctx)
}

async fn loopback_cors_middleware(request: Request<Body>, next: Next) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    if let Some(ref origin) = origin
        && !is_allowed_extension_origin(origin)
    {
        return json_error(StatusCode::FORBIDDEN, "cross-origin request blocked");
    }

    let mut response = if request.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };

    if let Some(origin) = origin
        && let Ok(val) = HeaderValue::from_str(&origin)
    {
        let headers = response.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, val);
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Authorization, Content-Type"),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
    }

    response
}

async fn require_auth(State(ctx): State<RouteCtx>, request: Request<Body>, next: Next) -> Response {
    let authorized = check_request_auth(&ctx, request.headers(), request.uri().query());
    if !authorized {
        return json_error(StatusCode::UNAUTHORIZED, "authentication required");
    }
    next.run(request).await
}

fn check_request_auth(ctx: &RouteCtx, headers: &HeaderMap, query: Option<&str>) -> bool {
    let bearer = extract_bearer_or_query_token(headers, query);
    let session_cookie = extract_session_cookie(headers);
    let now = Instant::now();

    let Ok(mut guard) = ctx.server.auth.lock() else {
        return false;
    };

    match ctx.listener {
        ListenerKind::Loopback => bearer
            .as_deref()
            .is_some_and(|token| guard.is_token_authorized(token, true)),
        ListenerKind::Lan => {
            if !guard.lan_auth_required {
                return true;
            }
            if let Some(cookie_token) = session_cookie.as_deref()
                && guard.sessions.is_valid(cookie_token, now)
            {
                return true;
            }
            if let Some(token) = bearer.as_deref()
                && guard.sessions.is_valid(token, now)
            {
                return true;
            }
            false
        }
    }
}

fn extract_bearer_or_query_token(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        for prefix in ["Bearer ", "bearer "] {
            if let Some(token) = auth.strip_prefix(prefix) {
                let trimmed = token.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
        }
    }
    if let Some(query) = query {
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix("token=")
                && !val.is_empty()
            {
                return Some(val.to_owned());
            }
        }
    }
    None
}

fn extract_session_cookie(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(raw) = value.to_str() else {
            continue;
        };
        for parsed in cookie::Cookie::split_parse(raw) {
            let Ok(c) = parsed else {
                continue;
            };
            if c.name() == SESSION_COOKIE_NAME && !c.value().is_empty() {
                return Some(c.value().to_owned());
            }
        }
    }
    None
}

async fn health(
    State(ctx): State<RouteCtx>,
    headers: HeaderMap,
    request_uri: axum::http::Uri,
) -> Json<HealthResponse> {
    let authenticated = check_request_auth(&ctx, &headers, request_uri.query());
    let (auth_required, locale) = ctx
        .server
        .auth
        .lock()
        .map(|guard| {
            let required = match ctx.listener {
                ListenerKind::Loopback => true,
                ListenerKind::Lan => guard.lan_auth_required,
            };
            (
                required,
                match guard.locale {
                    Language::English => "en".to_owned(),
                    Language::Italian => "it".to_owned(),
                },
            )
        })
        .unwrap_or((true, "en".to_owned()));

    Json(HealthResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        locale,
        auth_required,
        authenticated,
    })
}

async fn auth_login(
    State(ctx): State<RouteCtx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<LoginRequest>,
) -> Response {
    let ip = peer.ip();
    let now = Instant::now();

    let (auth_required, stored_hash) = {
        let Ok(mut guard) = ctx.server.auth.lock() else {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        };
        if let Err(retry_after) = guard.rate_limiter.check(ip, now) {
            return rate_limited_response(retry_after);
        }
        (guard.lan_auth_required, guard.lan_password_hash.clone())
    };

    if !auth_required {
        let token = match random_token_hex(32) {
            Ok(t) => t,
            Err(err) => return err.into_response(),
        };
        if let Ok(mut guard) = ctx.server.auth.lock() {
            guard.sessions.insert(token.clone(), now);
        }
        return session_cookie_response(&token);
    }

    let Some(stored) = stored_hash else {
        if let Ok(mut guard) = ctx.server.auth.lock() {
            guard.rate_limiter.record_failure(ip, now);
        }
        return json_error(StatusCode::UNAUTHORIZED, "password not configured");
    };

    let valid = verify_password_async(body.password, stored).await;
    if !valid {
        if let Ok(mut guard) = ctx.server.auth.lock() {
            guard.rate_limiter.record_failure(ip, now);
        }
        return json_error(StatusCode::UNAUTHORIZED, "invalid password");
    }

    let token = match random_token_hex(32) {
        Ok(t) => t,
        Err(err) => return err.into_response(),
    };
    if let Ok(mut guard) = ctx.server.auth.lock() {
        guard.rate_limiter.reset(ip);
        guard.sessions.insert(token.clone(), now);
    }
    session_cookie_response(&token)
}

fn session_cookie_response(token: &str) -> Response {
    let cookie_val = format!("{SESSION_COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/");
    match HeaderValue::from_str(&cookie_val) {
        Ok(val) => (
            StatusCode::OK,
            [(header::SET_COOKIE, val)],
            Json(LoginResponse {
                ok: true,
                token: token.to_owned(),
            }),
        )
            .into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "session error"),
    }
}

async fn auth_logout(State(ctx): State<RouteCtx>, headers: HeaderMap) -> Response {
    if let Some(token) = extract_session_cookie(&headers)
        && let Ok(mut guard) = ctx.server.auth.lock()
    {
        guard.sessions.remove(&token);
    }
    let cleared =
        HeaderValue::from_static("ruscello_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0");
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cleared)],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

async fn get_queue(State(ctx): State<RouteCtx>) -> Json<QueueSnapshotDto> {
    if let Some(dto) = ctx.server.telemetry.refresh_snapshot().await {
        Json(dto)
    } else {
        Json(ctx.server.telemetry.current_dto())
    }
}

async fn sse_events(
    State(ctx): State<RouteCtx>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send> {
    let initial = ctx
        .server
        .telemetry
        .refresh_snapshot()
        .await
        .unwrap_or_else(|| ctx.server.telemetry.current_dto());
    let rx = ctx.server.telemetry.subscribe();
    let shutdown = ctx.shutdown;

    struct SseState {
        initial: Option<QueueSnapshotDto>,
        rx: broadcast::Receiver<QueueSnapshotDto>,
        shutdown: CancellationToken,
    }

    let state = SseState {
        initial: Some(initial),
        rx,
        shutdown,
    };

    let event_stream = stream::unfold(state, |mut st| async move {
        if let Some(dto) = st.initial.take() {
            let ev = match Event::default().event("snapshot").json_data(&dto) {
                Ok(ev) => ev,
                Err(_) => Event::default().event("error").data("serialize"),
            };
            return Some((Ok(ev), st));
        }

        loop {
            tokio::select! {
                () = st.shutdown.cancelled() => return None,
                msg = st.rx.recv() => {
                    match msg {
                        Ok(dto) => {
                            let ev = match Event::default().event("snapshot").json_data(&dto) {
                                Ok(ev) => ev,
                                Err(_) => continue,
                            };
                            return Some((Ok(ev), st));
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            }
        }
    });

    Sse::new(event_stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn ws_events(State(ctx): State<RouteCtx>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_websocket(socket, ctx))
}

async fn handle_websocket(mut socket: WebSocket, ctx: RouteCtx) {
    let initial = ctx
        .server
        .telemetry
        .refresh_snapshot()
        .await
        .unwrap_or_else(|| ctx.server.telemetry.current_dto());

    if let Ok(text) = serde_json::to_string(&StreamEnvelope::Snapshot(initial))
        && socket.send(WsMessage::Text(text.into())).await.is_err()
    {
        return;
    }

    let mut rx = ctx.server.telemetry.subscribe();
    loop {
        tokio::select! {
            () = ctx.shutdown.cancelled() => {
                let _ = socket.send(WsMessage::Close(None)).await;
                break;
            }
            msg = rx.recv() => {
                match msg {
                    Ok(dto) => {
                        if let Ok(text) = serde_json::to_string(&StreamEnvelope::Snapshot(dto))
                            && socket.send(WsMessage::Text(text.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(WsMessage::Ping(payload))) => {
                        if socket.send(WsMessage::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

async fn add_downloads(
    State(ctx): State<RouteCtx>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Json<AddDownloadsResponse>, ApiError> {
    let is_multipart = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("multipart/form-data"));

    let req = if is_multipart {
        parse_multipart_add(headers, request).await?
    } else {
        let bytes = axum::body::to_bytes(request.into_body(), MAX_IMPORT_BYTES)
            .await
            .map_err(|err| ApiError::BadRequest(err.to_string()))?;
        serde_json::from_slice::<AddDownloadsRequest>(&bytes)?
    };

    process_add_downloads(&ctx, req).await.map(Json)
}

async fn parse_multipart_add(
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<AddDownloadsRequest, ApiError> {
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|err| ApiError::BadRequest(err.to_string()))?;
    let _ = headers;
    let mut req = AddDownloadsRequest::default();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::BadRequest(err.to_string()))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        let filename = field.file_name().map(str::to_owned);
        let data = field
            .bytes()
            .await
            .map_err(|err| ApiError::BadRequest(err.to_string()))?;
        if data.len() > MAX_IMPORT_BYTES {
            return Err(ApiError::BadRequest("uploaded file too large".to_owned()));
        }
        match name.as_str() {
            "urls" => {
                if let Ok(text) = std::str::from_utf8(&data) {
                    req.urls.extend(
                        text.lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
            "package_name" => {
                if let Ok(text) = std::str::from_utf8(&data) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        req.package_name = Some(trimmed.to_owned());
                    }
                }
            }
            "start_immediately" => {
                if let Ok(text) = std::str::from_utf8(&data) {
                    req.start_immediately = Some(text.trim() != "false" && text.trim() != "0");
                }
            }
            "file" => {
                let fname = filename.unwrap_or_else(|| "upload.bin".to_owned());
                let lower = fname.to_ascii_lowercase();
                if lower.ends_with(".torrent") {
                    req.torrent_base64 =
                        Some(base64::engine::general_purpose::STANDARD.encode(&data));
                    req.torrent_filename = Some(fname);
                } else if lower.ends_with(".dlc") {
                    let text = String::from_utf8(data.to_vec())
                        .map_err(|_| ApiError::BadRequest("invalid DLC encoding".to_owned()))?;
                    req.dlc_content = Some(text);
                    req.dlc_filename = Some(fname);
                } else if let Ok(text) = std::str::from_utf8(&data) {
                    for line in ruscello_engine::links(text) {
                        req.urls.push(line.into());
                    }
                }
            }
            _ => {}
        }
    }

    Ok(req)
}

async fn process_add_downloads(
    ctx: &RouteCtx,
    req: AddDownloadsRequest,
) -> Result<AddDownloadsResponse, ApiError> {
    if req.open_in_ui.unwrap_or(false) && !req.urls.is_empty() {
        import_request_cookies(ctx, &req).await;
        ctx.server.emit(ApiEvent::QueueRequested {
            urls: req.urls.clone(),
        });
    }

    let start_immediately = req.start_immediately.unwrap_or(true);
    let allow_dlc_fallback = ctx
        .server
        .auth
        .lock()
        .map(|guard| guard.allow_dlc_fallback)
        .unwrap_or(true);

    import_request_cookies(ctx, &req).await;

    let mut groups: Vec<(Option<String>, Vec<AddDownload>)> = Vec::new();
    let mut direct_items = Vec::new();

    for raw in &req.urls {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(mut dl) = add_request_from_str(trimmed) else {
            return Err(ApiError::BadRequest(format!(
                "invalid download URL: {trimmed}"
            )));
        };
        dl.destination.clone_from(&req.destination);
        dl.start_immediately = start_immediately;
        if req.urls.len() == 1 {
            dl.filename_hint.clone_from(&req.filename_hint);
        }
        direct_items.push(dl);
    }

    if let Some(torrent_b64) = req.torrent_base64.as_deref() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(torrent_b64.trim())
            .map_err(|_| ApiError::BadRequest("invalid torrent_base64 payload".to_owned()))?;
        let fname = req
            .torrent_filename
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("upload.torrent");
        let mut dl = AddDownload::new(Source::Torrent(TorrentSource::File(PathBuf::from(fname))));
        dl.cached_torrent_bytes = Some(bytes);
        dl.destination.clone_from(&req.destination);
        dl.start_immediately = start_immediately;
        direct_items.push(dl);
    }

    if !direct_items.is_empty() {
        groups.push((
            req.package_name
                .clone()
                .filter(|name| !name.trim().is_empty()),
            direct_items,
        ));
    }

    if let Some(dlc_raw) = req.dlc_content.as_deref() {
        let fallback_name = req
            .package_name
            .as_deref()
            .or(req.dlc_filename.as_deref())
            .map(|name| name.strip_suffix(".dlc").unwrap_or(name));
        let packages = ctx
            .server
            .engine
            .import_dlc_str(dlc_raw, fallback_name, allow_dlc_fallback)
            .await?;
        for pkg in packages {
            let mut items = Vec::with_capacity(pkg.links.len());
            for link in pkg.links {
                let mut dl = add_request_from_url(&link.url);
                dl.destination.clone_from(&req.destination);
                dl.start_immediately = start_immediately;
                dl.filename_hint = link.filename.clone();
                dl.output_filename = link.filename;
                dl.size_hint = link.size;
                dl.checksum = link.checksum;
                if dl.checksum.is_some() {
                    dl.mirrors = link.mirrors;
                }
                items.push(dl);
            }
            if !items.is_empty() {
                groups.push(((!pkg.name.is_empty()).then_some(pkg.name), items));
            }
        }
    }

    if groups.is_empty() {
        return Err(ApiError::BadRequest(
            "provide at least one valid URL, .torrent, or .dlc".to_owned(),
        ));
    }

    let ids = ctx.server.engine.add_groups(groups).await?;
    let _ = ctx.server.telemetry.refresh_snapshot().await;
    Ok(AddDownloadsResponse {
        count: ids.len(),
        added_ids: ids.into_iter().map(|id| id.0).collect(),
    })
}

async fn import_request_cookies(ctx: &RouteCtx, req: &AddDownloadsRequest) {
    let has_session_info = req
        .cookies
        .as_ref()
        .is_some_and(|cookies| !cookies.is_empty())
        || req.user_agent.is_some()
        || req.referer.is_some();
    if !has_session_info {
        return;
    }
    let session = BrowserSession {
        user_agent: req.user_agent.clone().unwrap_or_default(),
        cookies: req.cookies.clone().unwrap_or_default(),
    };
    for raw in &req.urls {
        if let Ok(url) = Url::parse(raw.trim())
            && matches!(url.scheme(), "http" | "https")
        {
            let _ = ctx
                .server
                .engine
                .import_session_cookies(&url, session.clone(), req.referer.as_deref())
                .await;
        }
    }
}

fn add_request_from_str(raw: &str) -> Option<AddDownload> {
    if raw.len() >= 7 && raw[..7].eq_ignore_ascii_case("magnet:") {
        return Some(AddDownload::new(Source::Torrent(TorrentSource::Magnet(
            raw.to_owned(),
        ))));
    }
    let url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https" | "ftp" | "ftps") || url.host_str().is_none() {
        return None;
    }
    Some(add_request_from_url(&url))
}

fn add_request_from_url(url: &Url) -> AddDownload {
    if matches!(url.scheme(), "ftp" | "ftps") {
        return AddDownload::new(Source::Ftp {
            url: url.to_string(),
        });
    }
    if url.path().to_ascii_lowercase().ends_with(".torrent") {
        return AddDownload::new(Source::Torrent(TorrentSource::Url(url.to_string())));
    }
    let kind = if is_hls_url(url) {
        Some(MediaKind::Hls)
    } else if is_dash_url(url) {
        Some(MediaKind::Dash)
    } else {
        None
    };
    if let Some(kind) = kind {
        let mut dl = AddDownload::new(Source::Media {
            url: url.to_string(),
        });
        dl.media = Some(MediaRequest {
            kind,
            ..MediaRequest::default()
        });
        dl
    } else {
        AddDownload::new(Source::Http {
            url: url.to_string(),
        })
    }
}

async fn pause_download(
    State(ctx): State<RouteCtx>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    ctx.server.engine.pause(DownloadId(id)).await?;
    let _ = ctx.server.telemetry.refresh_snapshot().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn resume_download(
    State(ctx): State<RouteCtx>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    ctx.server.engine.resume(DownloadId(id)).await?;
    let _ = ctx.server.telemetry.refresh_snapshot().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn force_start_download(
    State(ctx): State<RouteCtx>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    ctx.server.engine.force_start(DownloadId(id)).await?;
    let _ = ctx.server.telemetry.refresh_snapshot().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn cancel_download_post(
    State(ctx): State<RouteCtx>,
    Path(id): Path<u64>,
    Query(query): Query<DeleteQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    cancel_download_inner(&ctx, id, query.delete_files).await
}

async fn cancel_download_delete(
    State(ctx): State<RouteCtx>,
    Path(id): Path<u64>,
    Query(query): Query<DeleteQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    cancel_download_inner(&ctx, id, query.delete_files).await
}

async fn cancel_download_inner(
    ctx: &RouteCtx,
    id: u64,
    delete_files: bool,
) -> Result<Json<serde_json::Value>, ApiError> {
    ctx.server
        .engine
        .cancel(DownloadId(id), delete_files)
        .await?;
    let _ = ctx.server.telemetry.refresh_snapshot().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn pause_all(State(ctx): State<RouteCtx>) -> Result<Json<BulkActionResponse>, ApiError> {
    let snapshot = ctx.server.engine.snapshot().await?;
    let mut affected = 0_usize;
    for dl in snapshot.downloads {
        if can_pause(dl.status()) && ctx.server.engine.pause(dl.id).await.is_ok() {
            affected += 1;
        }
    }
    let _ = ctx.server.telemetry.refresh_snapshot().await;
    Ok(Json(BulkActionResponse { affected }))
}

async fn resume_all(State(ctx): State<RouteCtx>) -> Result<Json<BulkActionResponse>, ApiError> {
    let snapshot = ctx.server.engine.snapshot().await?;
    let mut affected = 0_usize;
    for dl in snapshot.downloads {
        if can_resume(dl.status()) && ctx.server.engine.resume(dl.id).await.is_ok() {
            affected += 1;
        }
    }
    let _ = ctx.server.telemetry.refresh_snapshot().await;
    Ok(Json(BulkActionResponse { affected }))
}

async fn set_alternate_speed(
    State(ctx): State<RouteCtx>,
    body: Option<Json<AltSpeedRequest>>,
) -> Result<Json<AltSpeedResponse>, ApiError> {
    let snapshot = ctx.server.engine.snapshot().await?;
    let currently_enabled = snapshot.temporary_bandwidth.is_some();
    let target_enabled = body
        .and_then(|Json(b)| b.enabled)
        .unwrap_or(!currently_enabled);

    let alt_rate = ctx
        .server
        .auth
        .lock()
        .map(|guard| guard.alternate_bandwidth)
        .map_err(|_| ApiError::BadRequest("lock poisoned".to_owned()))?;

    let new_rate = target_enabled.then_some(alt_rate);
    ctx.server.engine.set_temporary_bandwidth(new_rate).await?;
    ctx.server.emit(ApiEvent::AlternateSpeedChanged(new_rate));
    let _ = ctx.server.telemetry.refresh_snapshot().await;

    Ok(Json(AltSpeedResponse {
        enabled: target_enabled,
        bandwidth_bps: new_rate.map(std::num::NonZeroU64::get),
    }))
}

async fn forward_cookies(
    State(ctx): State<RouteCtx>,
    Json(body): Json<ForwardCookiesRequest>,
) -> Result<Json<ForwardCookiesResponse>, ApiError> {
    let url = Url::parse(body.url.trim())
        .map_err(|_| ApiError::BadRequest("invalid target URL".to_owned()))?;
    let session = BrowserSession {
        user_agent: body.user_agent.unwrap_or_default(),
        cookies: body.cookies,
    };
    let stored = ctx
        .server
        .engine
        .import_session_cookies(&url, session, body.referer.as_deref())
        .await?;
    Ok(Json(ForwardCookiesResponse { stored }))
}

async fn analyze_link(
    State(ctx): State<RouteCtx>,
    Json(body): Json<AnalyzeLinkRequest>,
) -> Result<Json<AnalyzeLinkResponse>, ApiError> {
    let trimmed = body.url.trim();
    let url = Url::parse(trimmed).map_err(|_| ApiError::BadRequest("invalid URL".to_owned()))?;

    if body.cookies.is_some() || body.user_agent.is_some() || body.referer.is_some() {
        let session = BrowserSession {
            user_agent: body.user_agent.clone().unwrap_or_default(),
            cookies: body.cookies.clone().unwrap_or_default(),
        };
        let _ = ctx
            .server
            .engine
            .import_session_cookies(&url, session, body.referer.as_deref())
            .await;
    }

    if body.open_in_ui.unwrap_or(false) {
        ctx.server.emit(ApiEvent::AnalyzePage {
            url: url.to_string(),
            cookies: body.cookies.unwrap_or_default(),
            user_agent: body.user_agent,
            referer: body.referer,
        });
    }

    let cancel = CancellationToken::new();
    let analysis = ctx.server.engine.inspect_link(&url, &cancel).await?;
    let (title, candidates) = match analysis {
        PageAnalysis::Direct(candidate) => (None, vec![map_candidate(candidate)]),
        PageAnalysis::Hoster { candidate, .. } => (None, vec![map_candidate(candidate)]),
        PageAnalysis::Page { title, candidates } => (
            (!title.is_empty()).then_some(title),
            candidates.into_iter().map(map_candidate).collect(),
        ),
    };
    Ok(Json(AnalyzeLinkResponse {
        title,
        source_url: url.to_string(),
        candidates,
    }))
}

async fn pair_request(
    State(ctx): State<RouteCtx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Option<Json<PairingRequestBody>>,
) -> Result<Response, ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("local-client");
    let body = body.map(|Json(b)| b).unwrap_or_default();

    let outcome =
        ctx.server
            .request_pairing(peer.ip(), origin, body.client_name, body.pin.as_deref())?;

    let local_port = ctx.server.local_port;
    let resp = match outcome {
        PairingRequestOutcome::Paired(token) => (
            StatusCode::OK,
            Json(PairingResponse {
                status: "paired".to_owned(),
                request_id: None,
                token: Some(token),
                local_port,
            }),
        )
            .into_response(),
        PairingRequestOutcome::Pending(request_id) => (
            StatusCode::ACCEPTED,
            Json(PairingResponse {
                status: "pending".to_owned(),
                request_id: Some(request_id),
                token: None,
                local_port,
            }),
        )
            .into_response(),
    };
    Ok(resp)
}

async fn pair_status(
    State(ctx): State<RouteCtx>,
    Path(request_id): Path<String>,
) -> Result<Json<PairingResponse>, ApiError> {
    let now = Instant::now();
    let mut guard = ctx
        .server
        .auth
        .lock()
        .map_err(|_| ApiError::BadRequest("lock poisoned".to_owned()))?;
    guard.prune_pairing(now);

    let entry = guard
        .pending_pairings
        .get(&request_id)
        .ok_or(ApiError::NotFound)?
        .clone();

    let (status, token) = match &entry.state {
        PendingStatus::Pending => ("pending".to_owned(), None),
        PendingStatus::Approved(tok) => {
            guard.pending_pairings.remove(&request_id);
            ("paired".to_owned(), Some(tok.clone()))
        }
        PendingStatus::Rejected => {
            guard.pending_pairings.remove(&request_id);
            ("rejected".to_owned(), None)
        }
    };

    Ok(Json(PairingResponse {
        status,
        request_id: Some(request_id),
        token,
        local_port: ctx.server.local_port,
    }))
}

#[allow(dead_code)] // Helper available if future extension headers are normalized
fn _has_header_name(headers: &HeaderMap, name: HeaderName) -> bool {
    headers.contains_key(name)
}
