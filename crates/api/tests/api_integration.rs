use std::{error::Error, num::NonZeroU64, time::Duration};

use futures_util::StreamExt as _;
use reqwest::{StatusCode, header};
use ruscello_api::{
    AddDownloadsResponse, AltSpeedResponse, AnalyzeLinkResponse, ApiConfig, ApiRuntimeUpdate,
    ApiServer, BulkActionResponse, HealthResponse, LanStatus, PairingResponse, QueueSnapshotDto,
    StreamEnvelope, discover_local_endpoint, hash_password, verify_password,
};
use ruscello_engine::{DlcKeyService, EngineHandle, EngineOptions, Url};
use ruscello_platform::language::Language;
use ruscello_testkit::{Reply, StatusCode as MockStatus, TestServer};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::connect_async;

type TestResult = Result<(), Box<dyn Error>>;

async fn start_test_engine(
    dir: &TempDir,
    dlc_url: Option<Url>,
) -> Result<EngineHandle, Box<dyn Error>> {
    let mut opts = EngineOptions::new(dir.path().join("ruscello.db"), dir.path().join("downloads"));
    opts.data_dir = Some(dir.path().to_path_buf());
    opts.dlc_service = dlc_url.map(|u| DlcKeyService::new(u, None));
    Ok(EngineHandle::start(opts).await?)
}

fn minimal_valid_torrent_bytes() -> Vec<u8> {
    // Minimal single-file v1 metainfo dict: d4:infod6:lengthi12e4:name8:test.bin12:piece lengthi16384e6:pieces20:<20 bytes>ee
    let mut out = b"d4:infod6:lengthi12e4:name8:test.bin12:piece lengthi16384e6:pieces20:".to_vec();
    out.extend_from_slice(&[0xAB_u8; 20]);
    out.extend_from_slice(b"ee");
    out
}

#[tokio::test]
async fn loopback_auth_strict_cors_and_discovery_file() -> TestResult {
    let dir = TempDir::new()?;
    let engine = start_test_engine(&dir, None).await?;
    let api = ApiServer::start(
        engine.clone(),
        ApiConfig {
            data_dir: Some(dir.path().to_path_buf()),
            local_port: 0,
            cli_token: Some("cli-secret-token".to_owned()),
            lan_enabled: false,
            ..ApiConfig::default()
        },
    )
    .await?;

    let base = format!("http://{}", api.local_addr());

    // Discovery file is written with port & token.
    let endpoint = discover_local_endpoint(dir.path()).ok_or("missing discovery file")?;
    assert_eq!(endpoint.port, api.local_addr().port());
    assert_eq!(endpoint.token, "cli-secret-token");

    let client = reqwest::Client::new();

    // Unauthenticated queue access is denied.
    let resp = client.get(format!("{base}/api/v1/queue")).send().await?;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Health works without auth and reports unauthenticated.
    let health: HealthResponse = client
        .get(format!("{base}/api/v1/health"))
        .send()
        .await?
        .json()
        .await?;
    assert!(health.ok);
    assert!(health.auth_required);
    assert!(!health.authenticated);

    // Malicious web Origin on loopback is rejected with 403 Forbidden.
    let evil = client
        .get(format!("{base}/api/v1/queue"))
        .header(header::ORIGIN, "https://evil.example")
        .bearer_auth("cli-secret-token")
        .send()
        .await?;
    assert_eq!(evil.status(), StatusCode::FORBIDDEN);

    let evil_preflight = client
        .request(reqwest::Method::OPTIONS, format!("{base}/api/v1/downloads"))
        .header(header::ORIGIN, "http://127.0.0.1:8080")
        .send()
        .await?;
    assert_eq!(evil_preflight.status(), StatusCode::FORBIDDEN);

    // Browser extension Origin (Chrome & Firefox) receives explicit ACAO header.
    for ext_origin in [
        "chrome-extension://abcdefghijklmnop",
        "moz-extension://12345678-1234-1234-1234-1234567890ab",
    ] {
        let resp = client
            .get(format!("{base}/api/v1/queue"))
            .header(header::ORIGIN, ext_origin)
            .bearer_auth("cli-secret-token")
            .send()
            .await?;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some(ext_origin)
        );
    }

    api.shutdown().await;
    assert!(discover_local_endpoint(dir.path()).is_none());
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn extension_one_click_and_pending_pairing_flows() -> TestResult {
    let dir = TempDir::new()?;
    let engine = start_test_engine(&dir, None).await?;
    let api = ApiServer::start(
        engine.clone(),
        ApiConfig {
            data_dir: Some(dir.path().to_path_buf()),
            local_port: 0,
            ..ApiConfig::default()
        },
    )
    .await?;

    let base = format!("http://{}", api.local_addr());
    let client = reqwest::Client::new();
    let chrome_origin = "chrome-extension://ruscellopairtest";

    // Path A: Desktop-armed one-click pairing window -> immediate token issuance for extension origin.
    let overview = api.start_pairing_window()?;
    assert!(overview.window_active);
    assert!(overview.pin.is_some());

    let pair_resp: PairingResponse = client
        .post(format!("{base}/api/v1/pair/request"))
        .header(header::ORIGIN, chrome_origin)
        .json(&serde_json::json!({ "client_name": "Chrome Test" }))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(pair_resp.status, "paired");
    let ext_token = pair_resp.token.ok_or("missing extension token")?;

    // Extension token authenticates queue requests.
    let q_resp = client
        .get(format!("{base}/api/v1/queue"))
        .header(header::ORIGIN, chrome_origin)
        .bearer_auth(&ext_token)
        .send()
        .await?;
    assert_eq!(q_resp.status(), StatusCode::OK);

    // Path B: Extension-initiated pairing when window is closed -> pending request + desktop Approve.
    let firefox_origin = "moz-extension://firefox-pair-test";
    let pending_resp = client
        .post(format!("{base}/api/v1/pair/request"))
        .header(header::ORIGIN, firefox_origin)
        .json(&serde_json::json!({ "client_name": "Firefox" }))
        .send()
        .await?;
    assert_eq!(pending_resp.status(), StatusCode::ACCEPTED);
    let pending: PairingResponse = pending_resp.json().await?;
    assert_eq!(pending.status, "pending");
    let req_id = pending.request_id.ok_or("missing request id")?;

    let status_pending: PairingResponse = client
        .get(format!("{base}/api/v1/pair/status/{req_id}"))
        .header(header::ORIGIN, firefox_origin)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(status_pending.status, "pending");

    assert_eq!(api.pairing_status().pending.len(), 1);
    api.approve_pairing(&req_id)?;

    let status_done: PairingResponse = client
        .get(format!("{base}/api/v1/pair/status/{req_id}"))
        .header(header::ORIGIN, firefox_origin)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(status_done.status, "paired");
    let ff_token = status_done.token.ok_or("missing firefox token")?;

    let ff_q = client
        .get(format!("{base}/api/v1/queue"))
        .header(header::ORIGIN, firefox_origin)
        .bearer_auth(&ff_token)
        .send()
        .await?;
    assert_eq!(ff_q.status(), StatusCode::OK);

    // Revoking extension tokens invalidates all issued extension tokens immediately.
    let revoked = api.revoke_extension_tokens()?;
    assert_eq!(revoked, 2);

    let rejected = client
        .get(format!("{base}/api/v1/queue"))
        .header(header::ORIGIN, chrome_origin)
        .bearer_auth(&ext_token)
        .send()
        .await?;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    api.shutdown().await;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn lan_webui_optional_auth_cookie_session_and_rate_limiting() -> TestResult {
    let dir = TempDir::new()?;
    let engine = start_test_engine(&dir, None).await?;
    let password_hash = hash_password("ruscello-pin-1234")?;
    assert!(verify_password("ruscello-pin-1234", &password_hash));
    assert!(!verify_password("wrong", &password_hash));

    // Start with LAN enabled, password disabled (trusted LAN open mode).
    let mut api = ApiServer::start(
        engine.clone(),
        ApiConfig {
            local_port: 0,
            lan_enabled: true,
            lan_port: 0,
            lan_auth_required: false,
            lan_password_hash: Some(password_hash.clone()),
            locale: Language::Italian,
            ..ApiConfig::default()
        },
    )
    .await?;

    let lan_bind = match api.lan_status() {
        LanStatus::Active { bind_addr, .. } => bind_addr,
        other => return Err(format!("expected Active LAN status, got {other:?}").into()),
    };
    let lan_base = format!("http://127.0.0.1:{}", lan_bind.port());

    let client = reqwest::Client::builder().cookie_store(true).build()?;

    // Embedded Web UI assets served with localized en + it strings and CSP.
    let index = client.get(format!("{lan_base}/")).send().await?;
    assert_eq!(index.status(), StatusCode::OK);
    let csp = index
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(csp.contains("default-src 'self'"));
    let html = index.text().await?;
    assert!(html.contains("/app.js") && html.contains("/style.css"));

    let js = client
        .get(format!("{lan_base}/app.js"))
        .send()
        .await?
        .text()
        .await?;
    assert!(js.contains("In scaricamento") && js.contains("Downloading"));

    let css = client.get(format!("{lan_base}/style.css")).send().await?;
    assert_eq!(css.status(), StatusCode::OK);

    // Health reflects Italian locale and no password required.
    let health: HealthResponse = client
        .get(format!("{lan_base}/api/v1/health"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(health.locale, "it");
    assert!(!health.auth_required);
    assert!(health.authenticated);

    // Open access to queue without cookie when lan_auth_required = false.
    let open_q = client
        .get(format!("{lan_base}/api/v1/queue"))
        .send()
        .await?;
    assert_eq!(open_q.status(), StatusCode::OK);

    // Switch live to password-protected mode.
    api.apply_update(ApiRuntimeUpdate {
        lan_enabled: true,
        lan_port: lan_bind.port(),
        lan_auth_required: true,
        lan_password_hash: Some(password_hash),
        alternate_bandwidth: NonZeroU64::MIN.saturating_add(128 * 1024 - 1),
        locale: Language::English,
    })
    .await;

    let blocked = client
        .get(format!("{lan_base}/api/v1/queue"))
        .send()
        .await?;
    assert_eq!(blocked.status(), StatusCode::UNAUTHORIZED);

    // Valid login sets HttpOnly cookie and unlocks protected endpoints.
    let login_ok = client
        .post(format!("{lan_base}/api/v1/auth/login"))
        .json(&serde_json::json!({ "password": "ruscello-pin-1234" }))
        .send()
        .await?;
    assert_eq!(login_ok.status(), StatusCode::OK);
    let set_cookie = login_ok
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(set_cookie.contains("ruscello_session="));
    assert!(set_cookie.contains("HttpOnly"));

    let authed_q = client
        .get(format!("{lan_base}/api/v1/queue"))
        .send()
        .await?;
    assert_eq!(authed_q.status(), StatusCode::OK);

    // Logout clears session.
    let logout = client
        .post(format!("{lan_base}/api/v1/auth/logout"))
        .send()
        .await?;
    assert_eq!(logout.status(), StatusCode::OK);
    let after_logout = client
        .get(format!("{lan_base}/api/v1/queue"))
        .send()
        .await?;
    assert_eq!(after_logout.status(), StatusCode::UNAUTHORIZED);

    // Brute-force protection: 5 wrong attempts allowed as 401, 6th returns 429 + Retry-After.
    for _ in 0..5 {
        let bad = client
            .post(format!("{lan_base}/api/v1/auth/login"))
            .json(&serde_json::json!({ "password": "wrong-password" }))
            .send()
            .await?;
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
    }
    let locked = client
        .post(format!("{lan_base}/api/v1/auth/login"))
        .json(&serde_json::json!({ "password": "ruscello-pin-1234" }))
        .send()
        .await?;
    assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(locked.headers().contains_key(header::RETRY_AFTER));

    // Disable LAN live -> LanStatus::Disabled and listener closed.
    let disabled = api
        .apply_update(ApiRuntimeUpdate {
            lan_enabled: false,
            lan_port: lan_bind.port(),
            lan_auth_required: true,
            lan_password_hash: None,
            alternate_bandwidth: NonZeroU64::MIN.saturating_add(128 * 1024 - 1),
            locale: Language::English,
        })
        .await;
    assert_eq!(disabled, LanStatus::Disabled);

    api.shutdown().await;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn downloads_torrent_upload_cookie_forwarding_linkgrabber_and_realtime_streams() -> TestResult
{
    let server = TestServer::start().await?;
    let public_url = server.route("/public.bin", |_req| {
        Reply::bytes("application/octet-stream", b"hello-public-file".to_vec())
    });
    let page_url = server.route("/page.html", |_req| {
        Reply::html(
            r#"<!doctype html><html><head><title>Downloads</title></head><body>
                <a href="/public.bin">Download Archive</a>
                <a href="/protected/secret.bin">Secret Bundle</a>
            </body></html>"#,
        )
    });
    let protected_url = server.route("/protected/secret.bin", |req| {
        let cookie_ok = req
            .headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("auth=valid123"));
        let ua_ok = req
            .headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("ExtBrowser/99"));
        let ref_ok = req
            .headers
            .get("referer")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("http://") && v.contains("/page.html"));
        if cookie_ok && ua_ok && ref_ok {
            Reply::bytes("application/octet-stream", b"protected-payload-ok".to_vec())
        } else {
            Reply::status(MockStatus::FORBIDDEN)
        }
    });

    let dir = TempDir::new()?;
    let engine = start_test_engine(&dir, None).await?;
    let api = ApiServer::start(
        engine.clone(),
        ApiConfig {
            local_port: 0,
            cli_token: Some("test-api-token".to_owned()),
            alternate_bandwidth: NonZeroU64::MIN.saturating_add(64 * 1024 - 1),
            ..ApiConfig::default()
        },
    )
    .await?;

    let base = format!("http://{}", api.local_addr());
    let ws_url = format!("ws://{}/api/v1/ws?token=test-api-token", api.local_addr());
    let client = reqwest::Client::new();

    // Connect WebSocket subscriber and verify initial snapshot message.
    let (mut ws_stream, _) = connect_async(&ws_url).await?;
    let first_ws = timeout(Duration::from_secs(5), ws_stream.next())
        .await?
        .ok_or("ws ended")??;
    let first_env: StreamEnvelope = serde_json::from_str(first_ws.to_text()?)?;
    assert!(matches!(first_env, StreamEnvelope::Snapshot(ref s) if s.downloads.is_empty()));

    // Open SSE stream as well.
    let sse_resp = client
        .get(format!("{base}/api/v1/events"))
        .bearer_auth("test-api-token")
        .send()
        .await?;
    assert_eq!(sse_resp.status(), StatusCode::OK);
    let mut sse_stream = sse_resp.bytes_stream();
    let sse_chunk = timeout(Duration::from_secs(5), sse_stream.next())
        .await?
        .ok_or("sse ended")??;
    let sse_text = String::from_utf8_lossy(&sse_chunk);
    assert!(sse_text.contains("snapshot") && sse_text.contains("downloads"));

    // Toggle alternate speed limit on and verify sync with EngineSnapshot.
    let alt_on: AltSpeedResponse = client
        .post(format!("{base}/api/v1/speed/alternate"))
        .bearer_auth("test-api-token")
        .json(&serde_json::json!({ "enabled": true }))
        .send()
        .await?
        .json()
        .await?;
    assert!(alt_on.enabled);
    assert_eq!(alt_on.bandwidth_bps, Some(64 * 1024));
    assert_eq!(
        engine
            .snapshot()
            .await?
            .temporary_bandwidth
            .map(|b| b.get()),
        Some(64 * 1024)
    );

    // Cookie-protected download added via API with cookies + UA + Referer succeeds.
    let add_resp: AddDownloadsResponse = client
        .post(format!("{base}/api/v1/downloads"))
        .bearer_auth("test-api-token")
        .json(&serde_json::json!({
            "urls": [&protected_url],
            "package_name": "CookieBatch",
            "start_immediately": true,
            "cookies": ["auth=valid123"],
            "user_agent": "ExtBrowser/99",
            "referer": &page_url,
        }))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(add_resp.count, 1);

    // Wait for protected download completion and verify file content on disk.
    let mut completed = false;
    let mut last_downloads = Vec::new();
    for _ in 0..80 {
        let q: QueueSnapshotDto = client
            .get(format!("{base}/api/v1/queue"))
            .bearer_auth("test-api-token")
            .send()
            .await?
            .json()
            .await?;
        last_downloads = q.downloads;
        if last_downloads.iter().any(|d| d.status == "Completed") {
            completed = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(
        completed,
        "cookie-protected download should complete; downloads={last_downloads:?}"
    );
    let saved = std::fs::read(dir.path().join("downloads").join("secret.bin"))?;
    assert_eq!(saved, b"protected-payload-ok");

    // WebSocket received updated snapshot containing the added download.
    let mut saw_ws_download = false;
    for _ in 0..10 {
        let msg = timeout(Duration::from_secs(3), ws_stream.next())
            .await?
            .ok_or("ws closed")??;
        if let Ok(StreamEnvelope::Snapshot(snap)) =
            serde_json::from_str::<StreamEnvelope>(msg.to_text()?)
            && !snap.downloads.is_empty()
        {
            saw_ws_download = true;
            break;
        }
    }
    assert!(saw_ws_download);

    // Upload a .torrent via base64 JSON in paused state, test pause-all / resume-all / cancel.
    let torrent_b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(minimal_valid_torrent_bytes())
    };
    let torrent_add: AddDownloadsResponse = client
        .post(format!("{base}/api/v1/downloads"))
        .bearer_auth("test-api-token")
        .json(&serde_json::json!({
            "torrent_base64": torrent_b64,
            "torrent_filename": "ubuntu.torrent",
            "start_immediately": false
        }))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(torrent_add.count, 1);
    let paused_id = torrent_add.added_ids[0];

    // Resume then pause via individual endpoints.
    let resume_resp = client
        .post(format!("{base}/api/v1/downloads/{paused_id}/resume"))
        .bearer_auth("test-api-token")
        .send()
        .await?;
    assert_eq!(resume_resp.status(), StatusCode::OK);

    let pause_resp = client
        .post(format!("{base}/api/v1/downloads/{paused_id}/pause"))
        .bearer_auth("test-api-token")
        .send()
        .await?;
    assert_eq!(pause_resp.status(), StatusCode::OK);

    // Add a queued URL via multipart/form-data, then pause_all and delete.
    let form = reqwest::multipart::Form::new()
        .text("urls", public_url.clone())
        .text("start_immediately", "false");
    let mp_resp = client
        .post(format!("{base}/api/v1/downloads"))
        .bearer_auth("test-api-token")
        .multipart(form)
        .send()
        .await?;
    assert_eq!(mp_resp.status(), StatusCode::OK);

    let resumed_all: BulkActionResponse = client
        .post(format!("{base}/api/v1/downloads/resume-all"))
        .bearer_auth("test-api-token")
        .send()
        .await?
        .json()
        .await?;
    assert!(resumed_all.affected >= 1);

    let del = client
        .delete(format!(
            "{base}/api/v1/downloads/{paused_id}?delete_files=true"
        ))
        .bearer_auth("test-api-token")
        .send()
        .await?;
    assert_eq!(del.status(), StatusCode::OK);

    // LinkGrabber page analysis extracts candidate links.
    let analyzed: AnalyzeLinkResponse = client
        .post(format!("{base}/api/v1/linkgrabber/analyze"))
        .bearer_auth("test-api-token")
        .json(&serde_json::json!({ "url": page_url }))
        .send()
        .await?
        .json()
        .await?;
    assert!(
        analyzed
            .candidates
            .iter()
            .any(|c| c.url.ends_with("/public.bin"))
    );

    // Queue an FTP URL and verify embedded passwords are stripped in queue DTO.
    let ftp_add: AddDownloadsResponse = client
        .post(format!("{base}/api/v1/downloads"))
        .bearer_auth("test-api-token")
        .json(&serde_json::json!({
            "urls": ["ftp://alice:ftpsecret@127.0.0.1:2121/pub/release.iso"],
            "start_immediately": false
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(ftp_add.count, 1);
    let queue_dto: QueueSnapshotDto = client
        .get(format!("{base}/api/v1/queue"))
        .bearer_auth("test-api-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        queue_dto
            .downloads
            .iter()
            .any(|d| d.source_summary == "ftp://alice@127.0.0.1:2121/pub/release.iso"),
        "expected sanitized FTP summary in {:?}",
        queue_dto.downloads
    );

    api.shutdown().await;
    engine.shutdown().await?;
    Ok(())
}
