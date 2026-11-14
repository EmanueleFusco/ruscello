use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use ruscello_core::{Source, TorrentSource};
use ruscello_http::{FtpCredentials, sanitize_ftp_url};
use ruscello_resolver::{ContainerFile, DlcKeyService, ImportedLink, import_file, links};
use tracing::warn;

use crate::{
    AddDownload, Url,
    automation::{WatchFolderRule, WatchProcessedBehavior},
};

const MAX_TEXT_IMPORT_BYTES: u64 = 2 * 1024 * 1024;
const STABILITY_AGE: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub(crate) struct WatchImportItem {
    pub source_file: PathBuf,
    pub category_name: Option<String>,
    pub groups: Vec<(Option<String>, Vec<AddDownload>)>,
}

#[derive(Debug, Default)]
pub(crate) struct WatchScanner {
    pending_stability: HashMap<PathBuf, FileStamp>,
    kept_processed: HashSet<(PathBuf, u64, Option<SystemTime>)>,
}

impl WatchScanner {
    pub(crate) async fn scan_rules(
        &mut self,
        rules: &[WatchFolderRule],
        dlc_service: Option<&DlcKeyService>,
        force_immediate: bool,
    ) -> Vec<WatchImportItem> {
        let mut imports = Vec::new();
        let default_dlc = DlcKeyService::production(false).ok();
        let dlc = dlc_service.or(default_dlc.as_ref());

        for rule in rules {
            if !rule.enabled || rule.directory.as_os_str().is_empty() {
                continue;
            }
            let Ok(mut entries) = tokio::fs::read_dir(&rule.directory).await else {
                continue;
            };
            let mut candidates = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name == "processed" || name.starts_with('.'))
                {
                    continue;
                }
                if !is_matching_watch_file(rule, &path) {
                    continue;
                }
                candidates.push(path);
            }
            candidates.sort();

            for path in candidates {
                let Ok(meta) = tokio::fs::metadata(&path).await else {
                    continue;
                };
                if !meta.is_file() || meta.len() == 0 {
                    continue;
                }
                let stamp = FileStamp {
                    size: meta.len(),
                    modified: meta.modified().ok(),
                };
                if rule.processed_behavior == WatchProcessedBehavior::KeepTracked
                    && self
                        .kept_processed
                        .contains(&(path.clone(), stamp.size, stamp.modified))
                {
                    continue;
                }
                if !force_immediate && !self.check_stable(&path, &stamp) {
                    continue;
                }
                self.pending_stability.remove(&path);

                match import_watched_file(&path, rule, dlc).await {
                    Ok(Some(item)) => {
                        match rule.processed_behavior {
                            WatchProcessedBehavior::KeepTracked => {
                                self.kept_processed
                                    .insert((path, stamp.size, stamp.modified));
                            }
                            WatchProcessedBehavior::Delete => {
                                if let Err(err) = tokio::fs::remove_file(&path).await {
                                    warn!(
                                        path = %path.display(),
                                        %err,
                                        "failed to delete processed watch file"
                                    );
                                }
                            }
                            WatchProcessedBehavior::MoveToProcessed => {
                                if let Err(err) = move_to_processed(&rule.directory, &path).await {
                                    warn!(
                                        path = %path.display(),
                                        %err,
                                        "failed to archive processed watch file"
                                    );
                                }
                            }
                        }
                        imports.push(item);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        warn!(
                            path = %path.display(),
                            %err,
                            "watch folder failed to import candidate file"
                        );
                    }
                }
            }
        }

        imports
    }

    fn check_stable(&mut self, path: &Path, stamp: &FileStamp) -> bool {
        if let Some(modified) = stamp.modified
            && let Ok(age) = SystemTime::now().duration_since(modified)
            && age >= STABILITY_AGE
        {
            return true;
        }
        match self.pending_stability.get(path) {
            Some(previous) if previous == stamp => true,
            _ => {
                self.pending_stability
                    .insert(path.to_path_buf(), stamp.clone());
                false
            }
        }
    }
}

fn is_matching_watch_file(rule: &WatchFolderRule, path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(OsStr::to_str) else {
        return false;
    };
    let lower = ext.to_ascii_lowercase();
    match lower.as_str() {
        "torrent" => rule.import_torrents,
        "dlc" => rule.import_dlc,
        "txt" => rule.import_txt,
        _ => false,
    }
}

async fn import_watched_file(
    path: &Path,
    rule: &WatchFolderRule,
    dlc: Option<&DlcKeyService>,
) -> io::Result<Option<WatchImportItem>> {
    let ext = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let groups = match ext.as_str() {
        "torrent" => {
            let bytes = tokio::fs::read(path).await?;
            let mut request =
                AddDownload::new(Source::Torrent(TorrentSource::File(path.to_path_buf())));
            request.destination = rule.destination.clone();
            request.cached_torrent_bytes = Some(bytes);
            request.filename_hint = path.file_stem().and_then(OsStr::to_str).map(str::to_owned);
            vec![(None, vec![request])]
        }
        "dlc" => {
            let Some(service) = dlc else {
                return Ok(None);
            };
            let parsed = import_file(path, service)
                .await
                .map_err(|err| io::Error::other(err.to_string()))?;
            match parsed {
                ContainerFile::Packages(packages) => packages
                    .into_iter()
                    .filter_map(|pkg| {
                        let reqs: Vec<AddDownload> = pkg
                            .links
                            .into_iter()
                            .filter_map(|link| add_request_from_imported_link(link, rule))
                            .collect();
                        (!reqs.is_empty()).then_some((Some(pkg.name), reqs))
                    })
                    .collect(),
                ContainerFile::Torrent => Vec::new(),
            }
        }
        "txt" => {
            let meta = tokio::fs::metadata(path).await?;
            if meta.len() > MAX_TEXT_IMPORT_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "text link file exceeds 2 MiB limit",
                ));
            }
            let text = tokio::fs::read_to_string(path).await?;
            let extracted_urls = links(&text);
            if extracted_urls.is_empty() {
                return Ok(None);
            }
            let requests: Vec<AddDownload> = extracted_urls
                .into_iter()
                .filter_map(|url| add_request_from_url(url, rule))
                .collect();
            if requests.is_empty() {
                return Ok(None);
            }
            let package = if requests.len() > 1 {
                path.file_stem().and_then(OsStr::to_str).map(str::to_owned)
            } else {
                None
            };
            vec![(package, requests)]
        }
        _ => return Ok(None),
    };

    if groups.is_empty() {
        return Ok(None);
    }

    Ok(Some(WatchImportItem {
        source_file: path.to_path_buf(),
        category_name: rule.category.clone(),
        groups,
    }))
}

fn add_request_from_imported_link(
    link: ImportedLink,
    rule: &WatchFolderRule,
) -> Option<AddDownload> {
    let mut request = add_request_from_url(link.url, rule)?;
    request.filename_hint = link.filename.clone();
    request.output_filename = link.filename;
    request.size_hint = link.size;
    request.mirrors = link.mirrors;
    request.checksum = link.checksum;
    Some(request)
}

pub(crate) fn add_request_from_url(url: Url, rule: &WatchFolderRule) -> Option<AddDownload> {
    let mut request = if url.scheme() == "magnet" {
        AddDownload::new(Source::Torrent(TorrentSource::Magnet(url.into())))
    } else if matches!(url.scheme(), "ftp" | "ftps") {
        let credentials =
            (!url.username().is_empty() || url.password().is_some()).then(|| FtpCredentials {
                username: if url.username().is_empty() {
                    "anonymous".to_owned()
                } else {
                    url.username().to_owned()
                },
                password: url.password().unwrap_or_default().to_owned(),
            });
        let sanitized = sanitize_ftp_url(&url);
        let mut req = AddDownload::new(Source::Ftp {
            url: sanitized.into(),
        });
        req.ftp_credentials = credentials;
        req
    } else if matches!(url.scheme(), "http" | "https") {
        AddDownload::new(Source::Http { url: url.into() })
    } else {
        return None;
    };

    request.destination = rule.destination.clone();
    Some(request)
}

async fn move_to_processed(watch_dir: &Path, src: &Path) -> io::Result<PathBuf> {
    let processed_dir = watch_dir.join("processed");
    tokio::fs::create_dir_all(&processed_dir).await?;
    let file_name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing filename"))?;
    let mut dest = processed_dir.join(file_name);
    if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
        let stem = src
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or("imported");
        let ext = src.extension().and_then(OsStr::to_str);
        for suffix in 1..=10_000_u32 {
            let candidate_name = match ext {
                Some(ext) => format!("{stem}-{suffix}.{ext}"),
                None => format!("{stem}-{suffix}"),
            };
            let candidate = processed_dir.join(candidate_name);
            if !tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
                dest = candidate;
                break;
            }
        }
    }
    if let Err(_err) = tokio::fs::rename(src, &dest).await {
        tokio::fs::copy(src, &dest).await?;
        tokio::fs::remove_file(src).await?;
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn watch_scanner_imports_txt_links_and_moves_to_processed() -> TestResult {
        let dir = tempfile::tempdir()?;
        let links_file = dir.path().join("batch.txt");
        tokio::fs::write(
            &links_file,
            "https://example.test/one.zip\nftp://alice:pw@files.example.test/two.tar.gz\n",
        )
        .await?;

        let rule = WatchFolderRule {
            id: "r1".to_owned(),
            enabled: true,
            directory: dir.path().to_path_buf(),
            import_torrents: true,
            import_dlc: true,
            import_txt: true,
            category: Some("Software".to_owned()),
            destination: None,
            processed_behavior: WatchProcessedBehavior::MoveToProcessed,
        };

        let mut scanner = WatchScanner::default();
        // First non-forced scan marks fresh file as pending stability
        assert!(
            scanner
                .scan_rules(std::slice::from_ref(&rule), None, false)
                .await
                .is_empty()
        );

        // Second scan sees unchanged stamp -> imports & moves into processed/
        let items = scanner
            .scan_rules(std::slice::from_ref(&rule), None, false)
            .await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].groups[0].0.as_deref(), Some("batch"));
        assert_eq!(items[0].groups[0].1.len(), 2);
        assert!(matches!(
            &items[0].groups[0].1[1].source,
            Source::Ftp { url } if url == "ftp://alice@files.example.test/two.tar.gz"
        ));
        assert_eq!(
            items[0].groups[0].1[1]
                .ftp_credentials
                .as_ref()
                .map(|c| c.password.as_str()),
            Some("pw")
        );
        assert!(!links_file.exists());
        assert!(dir.path().join("processed/batch.txt").exists());
        Ok(())
    }

    #[tokio::test]
    async fn watch_scanner_keeps_tracked_without_reimporting() -> TestResult {
        let dir = tempfile::tempdir()?;
        let torrent_file = dir.path().join("linux.torrent");
        tokio::fs::write(&torrent_file, b"d8:announce19:http://example.test/e").await?;

        let rule = WatchFolderRule {
            id: "r2".to_owned(),
            enabled: true,
            directory: dir.path().to_path_buf(),
            import_torrents: true,
            import_dlc: false,
            import_txt: false,
            category: None,
            destination: None,
            processed_behavior: WatchProcessedBehavior::KeepTracked,
        };

        let mut scanner = WatchScanner::default();
        let first = scanner
            .scan_rules(std::slice::from_ref(&rule), None, true)
            .await;
        assert_eq!(first.len(), 1);
        assert!(first[0].groups[0].1[0].cached_torrent_bytes.is_some());

        let second = scanner.scan_rules(&[rule], None, true).await;
        assert!(second.is_empty());
        assert!(torrent_file.exists());
        Ok(())
    }
}
