use std::error::Error;

use reqwest::Url;
use ruscello_resolver::{ResolveError, Resolver};
use ruscello_testkit::{
    FileSpec, HeaderMap, HeaderValue, Method, Reply, StatusCode, TestServer, header,
};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;

#[tokio::test]
async fn generic_countdown_redirect_with_cookie_requires_verification_instead_of_404() -> TestResult
{
    let server = TestServer::start().await?;
    server.route("/download", |request| {
        let cookie = request
            .headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok());
        if cookie.is_some_and(|value| value.contains("file_code=abc123")) {
            Reply::html(
                "<div id=\"download-countdown\" data-countdown=\"5\" data-code=\"abc123\" data-has-captcha=\"false\"></div>",
            )
        } else {
            Reply::status(StatusCode::NOT_FOUND)
        }
    });
    let entry = server.route("/abc123/update.rar", |_| {
        let mut headers = HeaderMap::new();
        headers.insert(header::LOCATION, HeaderValue::from_static("/download"));
        headers.insert(
            header::SET_COOKIE,
            HeaderValue::from_static("file_code=abc123; Path=/"),
        );
        Reply::Response(StatusCode::FOUND, headers, String::new())
    });
    let resolver = Resolver::new(None)?;
    let cancel = CancellationToken::new();
    let result = resolver
        .recover_link(&Url::parse(&entry)?, &cancel, false, true)
        .await;
    assert!(matches!(result, Err(ResolveError::VerificationRequired)));
    Ok(())
}

#[tokio::test]
async fn generic_xfs_html_form_resolves_without_explicit_hoster_flag() -> TestResult {
    let server = TestServer::start().await?;
    let payload = server.file(FileSpec::random(41, 128).filename("archive.rar"));
    let target = HeaderValue::from_str(payload.url())?;
    let entry = server.route("/generic/archive.rar", move |request| {
        if request.method == Method::POST {
            let mut headers = HeaderMap::new();
            headers.insert(header::LOCATION, target.clone());
            Reply::Response(StatusCode::FOUND, headers, String::new())
        } else {
            Reply::html(
                "<form method=\"post\" action=\"/generic/archive.rar\">\
                 <input type=\"hidden\" name=\"op\" value=\"download2\">\
                 <input type=\"hidden\" name=\"id\" value=\"generic\">\
                 <button id=\"downloadbtn\" type=\"submit\">Download</button>\
                 </form>",
            )
        }
    });
    let resolver = Resolver::new(None)?;
    let cancel = CancellationToken::new();
    let resolved = resolver
        .recover_link(&Url::parse(&entry)?, &cancel, false, false)
        .await?
        .ok_or("expected direct resolution from generic XFS form")?;
    assert_eq!(resolved.url.as_str(), payload.url());
    Ok(())
}
