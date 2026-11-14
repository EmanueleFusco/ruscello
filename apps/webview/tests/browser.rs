use std::{path::Path, time::Duration};

use ruscello_browser::{
    BrowserClient, BrowserOptions,
    protocol::{Request, Response, Url, View},
};
use ruscello_core::{DownloadId, Source, Status};
use ruscello_engine::{
    AddDownload, BrowserLocale, EngineHandle, EngineOptions, PageAnalysis, Resolver,
};
use ruscello_testkit::{BrowserFixture, FileSpec, TestServer};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const DEADLINE: Duration = Duration::from_secs(60);

fn trace() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::DEBUG)
        .with_test_writer()
        .try_init();
}

fn options(directory: &Path) -> BrowserOptions {
    BrowserOptions {
        executable: env!("CARGO_BIN_EXE_ruscello-webview").into(),
        profile: directory.join("webview_profile"),
        idle_timeout: Duration::from_millis(300),
    }
}

async fn wait_for(engine: &EngineHandle, ids: &[DownloadId], status: Status) -> anyhow::Result<()> {
    let mut events = engine.subscribe();
    timeout(DEADLINE, async {
        loop {
            let snapshot = engine.snapshot().await?;
            if ids.iter().all(|id| {
                snapshot
                    .downloads
                    .iter()
                    .any(|download| download.id == *id && download.status() == status)
            }) {
                return Ok::<_, anyhow::Error>(());
            }
            if let Some(failed) = snapshot.downloads.iter().find(|download| {
                ids.contains(&download.id) && matches!(download.status(), Status::Failed { .. })
            }) {
                anyhow::bail!(
                    "Download {:?} failed while waiting for {status:?}: {:?}",
                    failed.id,
                    failed.status()
                );
            }
            events.recv().await?;
        }
    })
    .await??;
    Ok(())
}

// Run under a desktop session (Xvfb in Linux CI). The server and all resources are loopback.
#[tokio::test]
#[ignore = "requires a native desktop session"]
async fn one_manual_verification_resumes_batch_and_survives_helper_restart() -> anyhow::Result<()> {
    trace();
    let temp = tempfile::tempdir()?;
    let server = TestServer::start().await?;
    let fixture = BrowserFixture::register(&server);
    let browser = options(temp.path());
    let mut config = EngineOptions::new(temp.path().join("queue.db"), temp.path().join("files"));
    config.browser = Some(browser.clone());
    let engine = EngineHandle::start(config).await?;
    let ids = engine
        .add_many(
            fixture
                .protected
                .iter()
                .map(|url| AddDownload::new(Source::Http { url: url.clone() }))
                .collect(),
        )
        .await?;
    wait_for(&engine, &ids, Status::WaitingUser).await?;
    let unrelated = server.file(FileSpec::random(2, 4096).filename("unrelated.bin"));
    let unrelated = engine
        .add(AddDownload::new(Source::Http {
            url: unrelated.url().into(),
        }))
        .await?;
    wait_for(&engine, &[unrelated], Status::Completed).await?;
    let verify = engine.open_verification(ids[0], BrowserLocale::It);
    let user = async {
        timeout(DEADLINE, fixture.verification_visible()).await?;
        fixture.click_verification();
        Ok::<_, anyhow::Error>(())
    };
    let (verified, clicked) = timeout(DEADLINE, async { tokio::join!(verify, user) }).await?;
    clicked?;
    verified?;
    wait_for(&engine, &ids, Status::Completed).await?;
    let snapshot = engine.snapshot().await?;
    for item in snapshot
        .downloads
        .iter()
        .filter(|item| ids.contains(&item.id))
    {
        let path = item
            .completed_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing published path"))?;
        assert_eq!(
            std::fs::read(path)?,
            fixture.payload.payload().bytes(0..256 * 1024)
        );
    }
    assert_eq!(fixture.verifications(), 1);
    assert!(fixture.authorized_requests() > ids.len());
    engine.shutdown().await?;
    drop(engine);
    let client = BrowserClient::new(browser);
    let cancel = CancellationToken::new();
    let url = Url::parse(&fixture.protected[0])?;
    let response = client
        .request(
            Request::Open {
                url,
                view: View::Session,
                steps: Vec::new(),
            },
            &cancel,
        )
        .await?;
    let Response::Session { session } = response else {
        anyhow::bail!("Expected persisted native session");
    };
    assert!(
        session
            .cookies
            .iter()
            .any(|cookie| cookie.contains("fixture_verified=1") && cookie.contains("HttpOnly"))
    );
    client.stop().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a native desktop session"]
async fn native_popup_blocking_intercepts_download_and_captures_resources() -> anyhow::Result<()> {
    trace();
    let temp = tempfile::tempdir()?;
    let server = TestServer::start().await?;
    let fixture = BrowserFixture::register(&server);
    let recipes = temp.path().join("recipes");
    std::fs::create_dir(&recipes)?;
    std::fs::write(
        recipes.join("browser.toml"),
        r##"
        [recipe]
        name = "Browser fixture"
        match = ['^http://127\.0\.0\.1:[0-9]+/browser-recipe$']
        browser = true
        [[step]]
        wait_for = { selector = "#download" }
        [[step]]
        click = { selector = "#download" }
        [[step]]
        expect_download = true
    "##,
    )?;
    let resolver = Resolver::new(Some(recipes))?;
    let mut configuration =
        EngineOptions::new(temp.path().join("queue.db"), temp.path().join("files"));
    configuration.browser = Some(options(temp.path()));
    configuration.resolver = Some(resolver);
    let engine = EngineHandle::start(configuration).await?;
    let id = engine
        .add(AddDownload::new(Source::Hoster {
            url: fixture.recipe.clone(),
        }))
        .await?;
    wait_for(&engine, &[id], Status::Completed).await?;
    let path = engine
        .snapshot()
        .await?
        .downloads
        .pop()
        .and_then(|item| item.completed_path)
        .ok_or_else(|| anyhow::anyhow!("Missing published path"))?;
    assert_eq!(
        std::fs::read(path)?,
        fixture.payload.payload().bytes(0..256 * 1024)
    );
    assert_eq!(fixture.popup_requests(), 0);
    let page = engine
        .inspect_link_in_browser(&Url::parse(&fixture.resources)?, &CancellationToken::new())
        .await?;
    let PageAnalysis::Page { candidates, .. } = page else {
        anyhow::bail!("Expected a page of captured resources");
    };
    for path in ["/stream.m3u8", "/xhr.mpd", "/video.mp4"] {
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.url.path() == path),
            "Did not capture {path}"
        );
    }
    engine.shutdown().await?;
    Ok(())
}
