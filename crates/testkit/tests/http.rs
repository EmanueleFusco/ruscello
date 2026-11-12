use std::{error::Error, time::Duration};

use reqwest::{Client, StatusCode, header};
use ruscello_testkit::{FileSpec, Representation, TestServer};
use tokio::time::Instant;

type TestResult = Result<(), Box<dyn Error>>;

fn client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
}

#[tokio::test]
async fn serves_exact_ranges_and_rejects_unsatisfiable_ranges() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(42, 1024));
    let client = client()?;
    let response = client
        .get(file.url())
        .header(header::RANGE, "bytes=5-33")
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 5-33/1024");
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "29");
    assert_eq!(&response.bytes().await?[..], file.payload().bytes(5..34));

    let response = client
        .get(file.url())
        .header(header::RANGE, "bytes=-13")
        .send()
        .await?;
    assert_eq!(
        &response.bytes().await?[..],
        file.payload().bytes(1011..1024)
    );

    let response = client
        .get(file.url())
        .header(header::RANGE, "bytes=1024-")
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */1024");
    Ok(())
}

#[tokio::test]
async fn follows_redirects_to_a_chunked_file_with_disposition() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(
        FileSpec::random(61, 32000)
            .filename("local file.txt")
            .ranges(false)
            .chunked()
            .redirects(3)
            .content_disposition("attachment; filename*=UTF-8''caff%C3%A8.txt")
            .content_type("text/plain"),
    );
    let response = client()?
        .get(file.url())
        .header(header::RANGE, "bytes=0-")
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.url().path().ends_with("/local%20file.txt"));
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename*=UTF-8''caff%C3%A8.txt"
    );
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/plain");
    assert_eq!(response.headers()[header::TRANSFER_ENCODING], "chunked");
    assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
    assert_eq!(&response.bytes().await?[..], file.payload().bytes(0..32000));
    Ok(())
}

#[tokio::test]
async fn changes_representation_after_emitting_bytes() -> TestResult {
    let server = TestServer::start().await?;
    let old = Representation::random(17, 256)
        .etag("\"old\"")
        .last_modified("Wed, 21 Oct 2015 07:28:00 GMT");
    let new = Representation::random(29, 256)
        .etag("\"new\"")
        .last_modified("Thu, 22 Oct 2015 07:28:00 GMT");
    let file = server.file(
        FileSpec::random(0, 0)
            .representation(old.clone())
            .change_after_bytes(16, new.clone()),
    );
    let client = client()?;
    let response = client
        .get(file.url())
        .header(header::RANGE, "bytes=0-15")
        .send()
        .await?;
    assert_eq!(response.headers()[header::ETAG], "\"old\"");
    assert_eq!(&response.bytes().await?[..], old.payload().bytes(0..16));

    let response = client
        .get(file.url())
        .header(header::RANGE, "bytes=16-")
        .header(header::IF_RANGE, "\"old\"")
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::ETAG], "\"new\"");
    assert_eq!(
        response.headers()[header::LAST_MODIFIED],
        "Thu, 22 Oct 2015 07:28:00 GMT"
    );
    assert_eq!(&response.bytes().await?[..], new.payload().bytes(0..256));
    let records = file.requests();
    assert_eq!(records[1].if_range.as_deref(), Some("\"old\""));
    assert_eq!(records[1].range.as_deref(), Some("bytes=16-"));
    Ok(())
}

#[tokio::test]
async fn injects_disconnects_and_intermittent_server_errors() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(
        FileSpec::random(19, 100_000)
            .drop_first_responses(1, 24_000)
            .intermittent_error(2, StatusCode::BAD_GATEWAY),
    );
    let client = client()?;
    let mut response = client.get(file.url()).send().await?;
    let mut delivered = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => delivered.extend_from_slice(&chunk),
            Ok(None) => panic!("fixture did not disconnect"),
            Err(error) => {
                assert!(error.is_body() || error.is_decode());
                break;
            }
        }
    }
    assert_eq!(delivered, file.payload().bytes(0..24_000));
    assert_eq!(
        client.get(file.url()).send().await?.status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        client.get(file.url()).send().await?.bytes().await?.len(),
        100_000
    );
    Ok(())
}

#[tokio::test]
async fn limits_concurrent_responses_and_applies_latency_and_throttle() -> TestResult {
    let server = TestServer::start().await?;
    let client = client()?;
    let latency = Duration::from_millis(30);
    for rejection in [
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        let file = server.file(
            FileSpec::random(37, 96_000)
                .latency(latency)
                .throttle_per_conn(160_000)
                .max_conns_per_ip(1, rejection),
        );
        let started = Instant::now();
        let first = client.get(file.url()).send().await?;
        assert!(started.elapsed() >= latency);
        assert_eq!(client.get(file.url()).send().await?.status(), rejection);
        assert_eq!(first.bytes().await?.len(), 96_000);
        assert!(started.elapsed() >= latency + Duration::from_millis(600));
    }
    Ok(())
}
