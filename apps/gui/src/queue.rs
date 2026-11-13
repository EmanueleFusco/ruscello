use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::Range,
    path::{Path, PathBuf},
    time::Instant,
};

use ruscello_core::{
    CategoryId, Download, DownloadId, FileType, PackageId, Source, Status, TorrentSource,
};
use ruscello_engine::{CategorySettings, EngineLog, EngineSnapshot, Limits, ProgressUpdate, Url};

pub(crate) const ROW_HEIGHT: f32 = 34.0;

pub(crate) const FILE_TYPES: [FileType; 8] = [
    FileType::Video,
    FileType::Audio,
    FileType::Archives,
    FileType::Programs,
    FileType::Documents,
    FileType::Images,
    FileType::Torrents,
    FileType::Other,
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum StatusFilter {
    #[default]
    All,
    Active,
    Queued,
    Paused,
    Completed,
    Failed,
    Seeding,
}

impl StatusFilter {
    pub(crate) const ALL: [Self; 7] = [
        Self::All,
        Self::Active,
        Self::Queued,
        Self::Paused,
        Self::Completed,
        Self::Failed,
        Self::Seeding,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Active => "active",
            Self::Queued => "queued",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Seeding => "seeding",
        }
    }

    fn includes(self, status: Status) -> bool {
        match self {
            Self::All => true,
            Self::Active => status.is_active() || status == Status::WaitingUser,
            Self::Queued => status == Status::Queued,
            Self::Paused => status == Status::Paused,
            Self::Completed => status == Status::Completed,
            Self::Failed => matches!(status, Status::Failed { .. }),
            Self::Seeding => status == Status::Seeding,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListRow {
    Download(DownloadId),
    Package(PackageId, usize),
}

#[derive(Debug, Default)]
pub(crate) struct Queue {
    pub(crate) downloads: BTreeMap<DownloadId, Download>,
    pub(crate) order: Vec<DownloadId>,
    pub(crate) categories: CategorySettings,
    pub(crate) packages: BTreeMap<PackageId, String>,
    pub(crate) limits: Limits,
    pub(crate) rows: Vec<ListRow>,
    pub(crate) status: StatusFilter,
    pub(crate) file_type: Option<FileType>,
    pub(crate) category: Option<CategoryId>,
    pub(crate) search: String,
    pub(crate) selected: Option<DownloadId>,
    pub(crate) collapsed: BTreeSet<PackageId>,
    pub(crate) logs: HashMap<DownloadId, Vec<EngineLog>>,
    type_counts: [usize; FILE_TYPES.len()],
    category_counts: BTreeMap<CategoryId, usize>,
    status_counts: [usize; StatusFilter::ALL.len()],
    pub(crate) active_count: usize,
    telemetry: HashMap<DownloadId, Telemetry>,
}

#[derive(Debug)]
struct Telemetry {
    at: Instant,
    bytes: u64,
    speed: u64,
    resumable: bool,
    offline: bool,
    segments: Option<ruscello_engine::SegmentProgress>,
}

impl Queue {
    pub(crate) fn replace(&mut self, snapshot: EngineSnapshot) {
        self.order = snapshot
            .downloads
            .iter()
            .map(|download| download.id)
            .collect();
        self.downloads = snapshot
            .downloads
            .into_iter()
            .map(|download| (download.id, download))
            .collect();
        self.limits = snapshot.limits;
        self.categories = snapshot.categories;
        self.packages = snapshot
            .packages
            .into_iter()
            .map(|package| (package.id, package.name))
            .collect();
        self.type_counts = [0; FILE_TYPES.len()];
        self.status_counts = [0; StatusFilter::ALL.len()];
        self.category_counts.clear();
        self.active_count = 0;
        for download in self.downloads.values() {
            self.type_counts[file_type(download) as usize] += 1;
            self.active_count += usize::from(download.status().is_active());
            for status in StatusFilter::ALL {
                self.status_counts[status as usize] +=
                    usize::from(status.includes(download.status()));
            }
            if let Some(category) = download.category {
                *self.category_counts.entry(category).or_default() += 1;
            }
        }
        self.telemetry.retain(|id, _| {
            self.downloads
                .get(id)
                .is_some_and(|download| download.status().is_active())
        });
        self.logs.retain(|id, _| self.downloads.contains_key(id));
        if self
            .selected
            .is_some_and(|id| !self.downloads.contains_key(&id))
        {
            self.selected = None;
        }
        if self.category.is_some_and(|id| {
            !self
                .categories
                .categories
                .iter()
                .any(|entry| entry.category.id == id)
        }) {
            self.category = None;
        }
        self.rebuild();
    }

    pub(crate) fn rebuild(&mut self) {
        let query = self.search.trim().to_lowercase();
        let mut packages = BTreeMap::<PackageId, Vec<DownloadId>>::new();
        for id in &self.order {
            let download = &self.downloads[id];
            if self.matches(download, &query)
                && let Some(package) = download.package
            {
                packages.entry(package).or_default().push(*id);
            }
        }
        self.rows.clear();
        for id in &self.order {
            let download = &self.downloads[id];
            if !self.matches(download, &query) {
                continue;
            }
            if let Some(package) = download.package {
                if let Some(members) = packages.remove(&package) {
                    self.rows.push(ListRow::Package(package, members.len()));
                    if !self.collapsed.contains(&package) {
                        self.rows.extend(members.into_iter().map(ListRow::Download));
                    }
                }
            } else {
                self.rows.push(ListRow::Download(*id));
            }
        }
    }

    fn matches(&self, download: &Download, query: &str) -> bool {
        self.status.includes(download.status())
            && self
                .file_type
                .is_none_or(|kind| file_type(download) == kind)
            && self.category.is_none_or(|id| download.category == Some(id))
            && (query.is_empty() || display_name(download).to_lowercase().contains(query))
    }

    pub(crate) fn status_count(&self, status: StatusFilter) -> usize {
        self.status_counts[status as usize]
    }

    pub(crate) fn type_count(&self, kind: FileType) -> usize {
        self.type_counts[kind as usize]
    }

    pub(crate) fn category_count(&self, category: CategoryId) -> usize {
        self.category_counts
            .get(&category)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn progress(&mut self, updates: Vec<ProgressUpdate>, now: Instant) {
        for update in updates {
            let Some(download) = self.downloads.get_mut(&update.id) else {
                continue;
            };
            let previous = self.telemetry.get(&update.id);
            let speed = previous
                .filter(|_| !update.progress.restarted && !update.progress.waiting_for_network)
                .map_or(0, |sample| {
                    let elapsed = now.saturating_duration_since(sample.at).as_secs_f64();
                    if elapsed > 0.0 {
                        (update.progress.completed.saturating_sub(sample.bytes) as f64 / elapsed)
                            as u64
                    } else {
                        sample.speed
                    }
                });
            download.downloaded_bytes = update.progress.completed;
            download.total_bytes = update.progress.total;
            self.telemetry.insert(
                update.id,
                Telemetry {
                    at: now,
                    bytes: update.progress.completed,
                    speed,
                    resumable: update.progress.resumable,
                    offline: update.progress.waiting_for_network,
                    segments: update.progress.segments,
                },
            );
        }
    }

    pub(crate) fn speed(&self, id: DownloadId) -> u64 {
        self.telemetry.get(&id).map_or(0, |sample| sample.speed)
    }

    pub(crate) fn total_speed(&self) -> u64 {
        self.telemetry
            .values()
            .map(|sample| sample.speed)
            .fold(0, u64::saturating_add)
    }

    pub(crate) fn resumable(&self, id: DownloadId) -> Option<bool> {
        self.telemetry.get(&id).map(|sample| sample.resumable)
    }

    pub(crate) fn offline(&self, id: DownloadId) -> bool {
        self.telemetry.get(&id).is_some_and(|sample| sample.offline)
    }

    pub(crate) fn segments(&self, id: DownloadId) -> Option<ruscello_engine::SegmentProgress> {
        self.telemetry.get(&id).and_then(|sample| sample.segments)
    }

    pub(crate) fn log(&mut self, id: DownloadId, message: EngineLog) {
        let logs = self.logs.entry(id).or_default();
        if logs.len() == 32 {
            logs.remove(0);
        }
        logs.push(message);
    }
}

pub(crate) fn visible_rows(offset: f32, height: f32, len: usize) -> Range<usize> {
    let start = ((offset / ROW_HEIGHT) as usize).saturating_sub(4).min(len);
    let end = ((((offset + height) / ROW_HEIGHT).ceil() as usize).saturating_add(4)).min(len);
    start..end
}

pub(crate) fn source_text(source: &Source) -> &str {
    match source {
        Source::Http { url }
        | Source::Hoster { url }
        | Source::Media { url }
        | Source::Torrent(TorrentSource::Magnet(url) | TorrentSource::Url(url)) => url,
        Source::Torrent(TorrentSource::File(path)) => path.to_str().unwrap_or_default(),
    }
}

fn source_name(source: &Source) -> Option<String> {
    if let Source::Torrent(TorrentSource::File(path)) = source {
        return path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
    }
    let url = Url::parse(source_text(source)).ok()?;
    if url.scheme() == "magnet" {
        return url
            .query_pairs()
            .find(|(key, _)| key == "dn")
            .map(|(_, name)| name.into_owned());
    }
    url.path_segments()
        .and_then(|mut parts| parts.rfind(|part| !part.is_empty()))
        .or(url.host_str())
        .map(str::to_owned)
}

pub(crate) fn display_name(download: &Download) -> String {
    download
        .filename
        .clone()
        .or_else(|| source_name(&download.source))
        .unwrap_or_else(|| format!("#{}", download.id.0))
}

pub(crate) fn file_type(download: &Download) -> FileType {
    if matches!(download.source, Source::Torrent(_)) {
        return FileType::Torrents;
    }
    download.file_type.unwrap_or_else(|| {
        let name = display_name(download);
        ruscello_engine::file_type_hint(Path::new(&name))
    })
}

pub(crate) async fn disk_space(path: PathBuf) -> Option<u64> {
    tokio::task::spawn_blocking(move || {
        path.ancestors()
            .find_map(|directory| fs4::available_space(directory).ok())
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_viewport_and_overscan_are_rendered() {
        assert_eq!(visible_rows(34_000.0, 340.0, 20_000), 996..1014);
        assert_eq!(visible_rows(0.0, 340.0, 2), 0..2);
        assert_eq!(visible_rows(34_000.0, 340.0, 5), 5..5);
    }

    #[test]
    fn filters_combine_and_packages_expand() {
        let mut queue = Queue::default();
        for (id, filename, category) in [
            (1, "Match.mp4", 1),
            (2, "Match.mp4", 2),
            (3, "Other.mp4", 1),
        ] {
            let id = DownloadId(id);
            let mut download =
                Download::new(id, Source::Http { url: String::new() }, PathBuf::new());
            download.package = Some(PackageId(1));
            download.filename = Some(filename.into());
            download.category = Some(CategoryId(category));
            queue.order.push(id);
            queue.downloads.insert(id, download);
        }
        queue.search = "MATCH".into();
        queue.category = Some(CategoryId(1));
        queue.file_type = Some(FileType::Video);
        queue.status = StatusFilter::Queued;
        queue.rebuild();
        assert_eq!(
            queue.rows,
            [
                ListRow::Package(PackageId(1), 1),
                ListRow::Download(DownloadId(1))
            ]
        );
        queue.collapsed.insert(PackageId(1));
        queue.rebuild();
        assert_eq!(queue.rows, [ListRow::Package(PackageId(1), 1)]);
    }
}
