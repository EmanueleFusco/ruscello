use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

const INDEX_HTML: &str = include_str!("../assets/webui/index.html");
const STYLE_CSS: &str = include_str!("../assets/webui/style.css");
const APP_JS: &str = include_str!("../assets/webui/app.js");

const CSP_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self' ws: wss:; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

pub(crate) async fn index_html() -> Response {
    asset_response("text/html; charset=utf-8", INDEX_HTML)
}

pub(crate) async fn style_css() -> Response {
    asset_response("text/css; charset=utf-8", STYLE_CSS)
}

pub(crate) async fn app_js() -> Response {
    asset_response("application/javascript; charset=utf-8", APP_JS)
}

fn asset_response(content_type: &'static str, body: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, must-revalidate"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(CSP_POLICY),
            ),
        ],
        body,
    )
        .into_response()
}
