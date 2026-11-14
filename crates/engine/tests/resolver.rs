use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use ruscello_core::{Source, Status};
use ruscello_engine::{
    AddDownload, Builtin, EngineEvent, EngineHandle, EngineOptions, Resolver, Url,
};
use ruscello_testkit::{FileSpec, HeaderMap, HeaderValue, Reply, StatusCode, TestServer, header};

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn start_engine(
    temp: &tempfile::TempDir,
    recipes: &std::path::Path,
) -> Result<EngineHandle, Box<dyn std::error::Error>> {
    let mut options = EngineOptions::new(temp.path().join("queue.db"), temp.path().join("files"));
    options.resolver = Some(Resolver::new(Some(recipes.to_owned()))?);
    Ok(EngineHandle::start(options).await?)
}

#[tokio::test]
async fn imports_twenty_hoster_pages_and_downloads_all_seeded_files() -> TestResult {
    let temp = tempfile::tempdir()?;
    let server = TestServer::start().await?;
    let recipes = temp.path().join("recipes");
    std::fs::create_dir(&recipes)?;
    std::fs::write(
        recipes.join("batch.toml"),
        r#"
        [recipe]
        name = "Local fixture"
        match = ['^http://127\.0\.0\.1:[0-9]+/share/']
        max_connections = 1
        max_parallel = 1
        [[step]]
        get = ""
        [[step]]
        extract = { regex = 'window\.open\("(/dl/[0-9]+)"\)' }
    "#,
    )?;

    let mut pages = Vec::new();
    let mut expected = Vec::new();
    for index in 0..20 {
        let name = format!("fixture-{index:02}.bin");
        let file = server.file(
            FileSpec::random(index, 24 * 1024)
                .filename(name.clone())
                .content_disposition(format!("attachment; filename=\"{name}\"")),
        );
        expected.push((name, file.payload()));
        let direct = format!("/dl/{index}");
        server.route(&direct, move |_| Reply::File(file.clone()));
        pages.push(server.route(&format!("/share/{index}"), move |_| {
            Reply::html(format!(
                r#"<script>window.open("https://ads.invalid/pop");window.open("{direct}")</script>"#
            ))
        }));
    }
    let list = temp.path().join("Twenty files.txt");
    std::fs::write(&list, pages.join("\n"))?;
    let ruscello_engine::ContainerFile::Packages(packages) =
        EngineHandle::import_links(&list, false).await?
    else {
        return Err("expected link package".into());
    };
    let engine = start_engine(&temp, &recipes).await?;
    let mut events = engine.subscribe();
    let requests = packages
        .into_iter()
        .map(|package| {
            (
                Some(package.name),
                package
                    .links
                    .into_iter()
                    .map(|link| {
                        AddDownload::new(Source::Http {
                            url: link.url.into(),
                        })
                    })
                    .collect(),
            )
        })
        .collect();
    let ids = engine.add_groups(requests).await?;
    assert_eq!(ids.len(), 20);
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut completed = 0;
        while completed < 20 {
            if let EngineEvent::StatusChanged { status, .. } = events.recv().await? {
                match status {
                    Status::Completed => completed += 1,
                    Status::Failed { reason } => {
                        return Err(format!("download failed: {reason:?}").into());
                    }
                    _ => {}
                }
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await??;
    for (name, payload) in expected {
        assert_eq!(
            std::fs::read(temp.path().join("files").join(name))?,
            payload.bytes(0..payload.len())
        );
    }
    engine.shutdown().await?;
    let restarted = start_engine(&temp, &recipes).await?;
    let saved = restarted.snapshot().await?;
    assert_eq!(saved.packages[0].name, "Twenty files");
    assert!(saved.downloads.iter().all(
        |item| item.status() == Status::Completed && item.package == Some(saved.packages[0].id)
    ));
    restarted.shutdown().await?;
    Ok(())
}

#[derive(Debug)]
struct FixturePlugin;

impl ruscello_resolver::HostPlugin for FixturePlugin {
    fn name(&self) -> &str {
        "Local FuckingFast"
    }
    fn matches(&self, url: &Url) -> bool {
        url.path() == "/share"
    }
    fn resolve<'a>(
        &'a self,
        ctx: &'a ruscello_resolver::Context,
        url: &'a Url,
    ) -> ruscello_resolver::ResolveFuture<'a, ruscello_resolver::Resolution> {
        ruscello_resolver::HostPlugin::resolve(&Builtin::FuckingFast, ctx, url)
    }
}

#[tokio::test]
async fn refreshes_expired_link_and_cookie_without_discarding_written_bytes() -> TestResult {
    let temp = tempfile::tempdir()?;
    let server = TestServer::start().await?;
    let old = server.file(FileSpec::random(221, 512 * 1024).drop_first_responses(2, 64 * 1024));
    let new = server.file(FileSpec::random(221, 512 * 1024));
    let refreshed = Arc::new(AtomicUsize::new(0));
    let prior_ranges = Arc::new(AtomicUsize::new(0));
    let share = server.route("/share", {
        let refreshed = refreshed.clone();
        move |_| {
            let generation = refreshed.fetch_add(1, Ordering::Relaxed);
            let mut headers = HeaderMap::new();
            headers.insert(
                "hx-redirect",
                if generation == 0 {
                    HeaderValue::from_static("/dl/old")
                } else {
                    HeaderValue::from_static("/dl/new")
                },
            );
            headers.insert(
                header::SET_COOKIE,
                if generation == 0 {
                    HeaderValue::from_static("session=old; Path=/dl")
                } else {
                    HeaderValue::from_static("session=new; Path=/dl")
                },
            );
            Reply::Response(StatusCode::OK, headers, String::new())
        }
    });
    server.route("/dl/old", {
        let prior_ranges = prior_ranges.clone();
        move |request| {
            if request
                .headers
                .get(header::COOKIE)
                .is_none_or(|cookie| cookie != "session=old")
            {
                return Reply::status(StatusCode::UNAUTHORIZED);
            }
            if request.headers.contains_key(header::IF_RANGE)
                && prior_ranges.fetch_add(1, Ordering::Relaxed) > 0
            {
                return Reply::status(StatusCode::GONE);
            }
            Reply::File(old.clone())
        }
    });
    server.route("/dl/new", {
        let new = new.clone();
        move |request| {
            if request
                .headers
                .get(header::COOKIE)
                .is_some_and(|cookie| cookie == "session=new")
            {
                Reply::File(new.clone())
            } else {
                Reply::status(StatusCode::UNAUTHORIZED)
            }
        }
    });
    let mut options = ruscello_engine::DownloadOptions {
        directory: temp.path().to_owned(),
        connections: std::num::NonZeroUsize::MIN,
        ..Default::default()
    };
    let url = Url::parse(&share)?;
    let resolver = Resolver::new(None)?.with_plugin(FixturePlugin);
    ruscello_engine::resolve_download(&resolver, &url, &options.cancellation)
        .await?
        .ok_or("fixture was not recognized")?
        .apply(&mut options);
    let path = tokio::time::timeout(
        Duration::from_secs(10),
        ruscello_engine::HttpDownloader::new()?.download(&url, options),
    )
    .await??;
    assert_eq!(
        std::fs::read(path)?,
        new.payload().bytes(0..new.payload().len())
    );
    assert_eq!(refreshed.load(Ordering::Relaxed), 2);
    assert!(new.requests().iter().any(|request| {
        request.if_range.is_some()
            && request
                .range
                .as_deref()
                .is_some_and(|range| !range.starts_with("bytes=0-"))
    }));
    Ok(())
}
