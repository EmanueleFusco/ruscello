use std::{error::Error, time::Duration};

use reqwest::{StatusCode, Url, header};
use ruscello_http::{HttpError, client, probe};
use ruscello_testkit::{FileSpec, Representation, TestServer};

type TestResult = Result<(), Box<dyn Error>>;

#[tokio::test]
async fn reads_redirect_destination_and_prefers_content_disposition() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(
        FileSpec::random(17, 100_000)
            .filename("final.pdf")
            .redirects(3)
            .content_type("application/pdf")
            .content_disposition(
                "attachment; filename=unused.pdf; filename*=UTF-8''caff%C3%A8.pdf",
            ),
    );
    let data = probe(&client()?, &Url::parse(file.url())?).await?;
    assert!(data.final_url.path().ends_with("/final.pdf"));
    assert_eq!(data.filename, "caffè.pdf");
    assert_eq!(data.size, Some(100_000));
    assert!(data.supports_ranges);
    assert!(data.validators.if_range().is_some());
    let observed = file.requests();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].range.as_deref(), Some("bytes=0-"));
    Ok(())
}

#[tokio::test]
async fn distinguishes_chunked_streams_fixed_sizes_and_empty_files() -> TestResult {
    let server = TestServer::start().await?;
    let client = client()?;
    let file = server.file(
        FileSpec::random(1, 96)
            .filename("")
            .ranges(false)
            .chunked()
            .content_type("application/pdf; custom=fixture"),
    );
    let data = probe(&client, &Url::parse(file.url())?).await?;
    assert!(!data.supports_ranges);
    assert_eq!(data.size, None);
    assert_eq!(data.filename, "download.pdf");

    let file = server.file(
        FileSpec::random(2, 256)
            .filename("name with spaces.bin")
            .ranges(false),
    );
    let data = probe(&client, &Url::parse(file.url())?).await?;
    assert!(!data.supports_ranges);
    assert_eq!(data.size, Some(256));
    assert_eq!(data.filename, "name with spaces.bin");

    let file = server.file(FileSpec::random(3, 0));
    let data = probe(&client, &Url::parse(file.url())?).await?;
    assert!(data.supports_ranges);
    assert_eq!(data.size, Some(0));
    Ok(())
}

#[tokio::test]
async fn extracts_validators_and_ignores_weak_etags_for_if_range() -> TestResult {
    let server = TestServer::start().await?;
    let client = client()?;
    let date = "Wed, 21 Oct 2015 07:28:00 GMT";
    let file = server.file(
        FileSpec::random(0, 0).representation(
            Representation::random(8, 128)
                .etag("W/\"weak\"")
                .last_modified(date),
        ),
    );
    let data = probe(&client, &Url::parse(file.url())?).await?;
    assert_eq!(
        data.validators
            .etag
            .as_ref()
            .map(header::HeaderValue::as_bytes),
        Some(b"W/\"weak\"".as_slice())
    );
    assert!(data.validators.if_range().is_none());

    file.replace(
        Representation::random(9, 128)
            .without_etag()
            .last_modified(date),
    );
    let data = probe(&client, &Url::parse(file.url())?).await?;
    assert_eq!(
        data.validators
            .if_range()
            .map(header::HeaderValue::as_bytes),
        Some(date.as_bytes())
    );
    Ok(())
}

#[tokio::test]
async fn does_not_wait_for_the_body_of_a_delayed_or_dropping_response() -> TestResult {
    let server = TestServer::start().await?;
    let client = client()?;
    let file = server.file(
        FileSpec::random(2, 4 * 1024 * 1024)
            .latency(Duration::from_millis(25))
            .throttle_per_conn(128)
            .drop_after_bytes(1024),
    );
    let url = Url::parse(file.url())?;
    let data = tokio::time::timeout(Duration::from_secs(4), probe(&client, &url)).await??;
    assert_eq!(data.size, Some(4 * 1024 * 1024));
    Ok(())
}

#[tokio::test]
async fn reports_capacity_and_intermittent_server_statuses() -> TestResult {
    let server = TestServer::start().await?;
    let client = client()?;
    let blocked =
        server.file(FileSpec::random(2, 32).max_conns_per_ip(0, StatusCode::TOO_MANY_REQUESTS));
    assert!(matches!(
        probe(&client, &Url::parse(blocked.url())?).await,
        Err(HttpError::Status(StatusCode::TOO_MANY_REQUESTS))
    ));

    let flaky = server.file(FileSpec::random(2, 32).intermittent_error(2, StatusCode::BAD_GATEWAY));
    let url = Url::parse(flaky.url())?;
    probe(&client, &url).await?;
    assert!(matches!(
        probe(&client, &url).await,
        Err(HttpError::Status(StatusCode::BAD_GATEWAY))
    ));
    probe(&client, &url).await?;
    Ok(())
}
