use std::{
    net::IpAddr,
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    time::Duration,
};

use ruscello_engine::{
    AutomationSettings, Limits, OutsideHoursAction, PostProcessingSettings, QueueCompletionAction,
    ScheduleAction, ScheduleRule, ScheduleSettings, WatchFolderRule, WatchFolderSettings,
    WatchProcessedBehavior, WeekdayMask, format_minutes, parse_hhmm,
};
use serde::Deserialize;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    #[default]
    General,
    Downloads,
    Speed,
    Automation,
    Torrent,
    Categories,
    Integrations,
    Hosters,
    Advanced,
    About,
}

impl Page {
    pub(crate) const ALL: [Self; 10] = [
        Self::General,
        Self::Downloads,
        Self::Speed,
        Self::Automation,
        Self::Torrent,
        Self::Categories,
        Self::Integrations,
        Self::Hosters,
        Self::Advanced,
        Self::About,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Downloads => "downloads",
            Self::Speed => "speed",
            Self::Automation => "automation",
            Self::Torrent => "torrent-input",
            Self::Categories => "categories",
            Self::Integrations => "integrations",
            Self::Hosters => "hoster-accounts",
            Self::Advanced => "advanced",
            Self::About => "about",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Number {
    Concurrent,
    PerHost,
    PerDownload,
    Bandwidth,
    Alternate,
    Port,
}

impl Number {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Concurrent => "limit-concurrent",
            Self::PerHost => "limit-host",
            Self::PerDownload => "limit-download",
            Self::Bandwidth => "limit-bandwidth",
            Self::Alternate => "limit-alternate",
            Self::Port => "lan-port",
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Default)]
pub(crate) struct Draft {
    pub(crate) fields: [String; 6],
}

impl Draft {
    pub(crate) fn new(limits: Limits, alternate: NonZeroU64, port: NonZeroU16) -> Self {
        Self {
            fields: [
                limits.concurrent_downloads.to_string(),
                limits.connections_per_host.to_string(),
                limits.connections_per_download.to_string(),
                limits
                    .bandwidth
                    .map_or_else(String::new, |rate| rate.to_string()),
                alternate.to_string(),
                port.to_string(),
            ],
        }
    }

    pub(crate) fn parse(&self, mut limits: Limits) -> Option<(Limits, NonZeroU64, NonZeroU16)> {
        let value = |field: Number| self.fields[field.index()].trim();
        limits.concurrent_downloads = value(Number::Concurrent).parse::<NonZeroUsize>().ok()?;
        limits.connections_per_host = value(Number::PerHost).parse::<NonZeroUsize>().ok()?;
        limits.connections_per_download =
            value(Number::PerDownload).parse::<NonZeroUsize>().ok()?;
        let bandwidth = value(Number::Bandwidth);
        limits.bandwidth = if bandwidth.is_empty() {
            None
        } else {
            Some(bandwidth.parse().ok()?)
        };
        Some((
            limits,
            value(Number::Alternate).parse().ok()?,
            value(Number::Port).parse().ok()?,
        ))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Folder {
    Default,
    Temporary,
    Category(ruscello_core::CategoryId),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TorrentOption {
    Dht,
    Pex,
    Upnp,
    Lsd,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TorrentField {
    Port,
    DownloadLimit,
    UploadLimit,
    Ratio,
    Minutes,
    MaxPeers,
    Interface,
    Blocklist,
}

#[derive(Debug, Default)]
pub(crate) struct CategoryDraft {
    pub(crate) name: String,
    pub(crate) rule: String,
    pub(crate) category: Option<ruscello_core::CategoryId>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CategoryEdit {
    Name,
    Rule,
}

pub(crate) async fn pick_folder() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .pick_folder()
        .await
        .map(|folder| folder.path().to_path_buf())
}

pub(crate) async fn pick_file() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .pick_file()
        .await
        .map(|file| file.path().to_path_buf())
}

pub(crate) const COMPLETION_ACTIONS: [QueueCompletionAction; 4] = [
    QueueCompletionAction::Nothing,
    QueueCompletionAction::Notify,
    QueueCompletionAction::Sleep,
    QueueCompletionAction::Shutdown,
];

pub(crate) const WATCH_BEHAVIORS: [WatchProcessedBehavior; 3] = [
    WatchProcessedBehavior::MoveToProcessed,
    WatchProcessedBehavior::Delete,
    WatchProcessedBehavior::KeepTracked,
];

pub(crate) fn completion_action_key(action: QueueCompletionAction) -> &'static str {
    match action {
        QueueCompletionAction::Nothing => "completion-nothing",
        QueueCompletionAction::Notify => "completion-notify",
        QueueCompletionAction::Sleep => "completion-sleep",
        QueueCompletionAction::Shutdown => "completion-shutdown",
    }
}

pub(crate) fn watch_behavior_key(behavior: WatchProcessedBehavior) -> &'static str {
    match behavior {
        WatchProcessedBehavior::MoveToProcessed => "watch-behavior-move",
        WatchProcessedBehavior::Delete => "watch-behavior-delete",
        WatchProcessedBehavior::KeepTracked => "watch-behavior-keep",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutsideHoursMode {
    UseGlobalDefault,
    LimitSpeed,
    PauseQueue,
}

impl OutsideHoursMode {
    pub(crate) const ALL: [Self; 3] = [Self::UseGlobalDefault, Self::LimitSpeed, Self::PauseQueue];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::UseGlobalDefault => "schedule-outside-global",
            Self::LimitSpeed => "schedule-outside-limit",
            Self::PauseQueue => "schedule-outside-pause",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScheduleRuleMode {
    FullSpeed,
    SpeedLimit,
    PauseQueue,
}

impl ScheduleRuleMode {
    pub(crate) const ALL: [Self; 3] = [Self::FullSpeed, Self::SpeedLimit, Self::PauseQueue];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::FullSpeed => "schedule-action-full",
            Self::SpeedLimit => "schedule-action-limit",
            Self::PauseQueue => "schedule-action-pause",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ScheduleRuleDraft {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) days: WeekdayMask,
    pub(crate) start_hhmm: String,
    pub(crate) end_hhmm: String,
    pub(crate) mode: ScheduleRuleMode,
    pub(crate) speed_bps: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WatchFolderDraft {
    pub(crate) id: String,
    pub(crate) enabled: bool,
    pub(crate) directory: String,
    pub(crate) import_torrents: bool,
    pub(crate) import_dlc: bool,
    pub(crate) import_txt: bool,
    pub(crate) category: String,
    pub(crate) processed_behavior: WatchProcessedBehavior,
}

#[derive(Clone)]
pub(crate) struct AutomationDraft {
    pub(crate) prevent_sleep_when_active: bool,
    pub(crate) completion_action: QueueCompletionAction,
    pub(crate) completion_countdown_secs: String,
    pub(crate) schedule_enabled: bool,
    pub(crate) outside_mode: OutsideHoursMode,
    pub(crate) outside_speed_bps: String,
    pub(crate) schedule_rules: Vec<ScheduleRuleDraft>,
    pub(crate) auto_verify_checksums: bool,
    pub(crate) auto_extract: bool,
    pub(crate) extract_into_subfolder: bool,
    pub(crate) delete_archives_after_extraction: bool,
    pub(crate) archive_passwords: Vec<String>,
    pub(crate) new_archive_password: String,
    pub(crate) external_extractor_path: String,
    pub(crate) post_download_command: String,
    pub(crate) post_download_timeout_secs: String,
    pub(crate) watch_enabled: bool,
    pub(crate) watch_poll_interval_secs: String,
    pub(crate) watch_rules: Vec<WatchFolderDraft>,
}

impl std::fmt::Debug for AutomationDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AutomationDraft")
            .field("prevent_sleep_when_active", &self.prevent_sleep_when_active)
            .field("completion_action", &self.completion_action)
            .field("schedule_enabled", &self.schedule_enabled)
            .field("schedule_rules", &self.schedule_rules.len())
            .field("auto_verify_checksums", &self.auto_verify_checksums)
            .field("auto_extract", &self.auto_extract)
            .field("archive_passwords", &self.archive_passwords.len())
            .field("watch_enabled", &self.watch_enabled)
            .field("watch_rules", &self.watch_rules.len())
            .finish()
    }
}

impl Default for AutomationDraft {
    fn default() -> Self {
        Self::from_settings(&AutomationSettings::default())
    }
}

impl AutomationDraft {
    pub(crate) fn from_settings(settings: &AutomationSettings) -> Self {
        let (outside_mode, outside_speed_bps) = match settings.schedule.outside_hours {
            OutsideHoursAction::UseGlobalDefault => {
                (OutsideHoursMode::UseGlobalDefault, "262144".to_owned())
            }
            OutsideHoursAction::LimitSpeed { max_bytes_per_sec } => {
                (OutsideHoursMode::LimitSpeed, max_bytes_per_sec.to_string())
            }
            OutsideHoursAction::PauseQueue => (OutsideHoursMode::PauseQueue, "262144".to_owned()),
        };
        let schedule_rules = settings
            .schedule
            .rules
            .iter()
            .map(|rule| {
                let (mode, speed_bps) = match rule.action {
                    ScheduleAction::FullSpeed => (ScheduleRuleMode::FullSpeed, "524288".to_owned()),
                    ScheduleAction::SpeedLimit { max_bytes_per_sec } => {
                        (ScheduleRuleMode::SpeedLimit, max_bytes_per_sec.to_string())
                    }
                    ScheduleAction::PauseQueue => {
                        (ScheduleRuleMode::PauseQueue, "524288".to_owned())
                    }
                };
                ScheduleRuleDraft {
                    id: rule.id.clone(),
                    name: rule.name.clone(),
                    enabled: rule.enabled,
                    days: rule.days,
                    start_hhmm: format_minutes(rule.start_minute),
                    end_hhmm: format_minutes(rule.end_minute),
                    mode,
                    speed_bps,
                }
            })
            .collect();
        let watch_rules = settings
            .watch_folders
            .rules
            .iter()
            .map(|rule| WatchFolderDraft {
                id: rule.id.clone(),
                enabled: rule.enabled,
                directory: rule.directory.to_string_lossy().into_owned(),
                import_torrents: rule.import_torrents,
                import_dlc: rule.import_dlc,
                import_txt: rule.import_txt,
                category: rule.category.clone().unwrap_or_default(),
                processed_behavior: rule.processed_behavior,
            })
            .collect();
        Self {
            prevent_sleep_when_active: settings.prevent_sleep_when_active,
            completion_action: settings.completion_action,
            completion_countdown_secs: settings.completion_countdown_secs.to_string(),
            schedule_enabled: settings.schedule.enabled,
            outside_mode,
            outside_speed_bps,
            schedule_rules,
            auto_verify_checksums: settings.post_processing.auto_verify_checksums,
            auto_extract: settings.post_processing.auto_extract,
            extract_into_subfolder: settings.post_processing.extract_into_subfolder,
            delete_archives_after_extraction: settings
                .post_processing
                .delete_archives_after_extraction,
            archive_passwords: settings.post_processing.archive_passwords.clone(),
            new_archive_password: String::new(),
            external_extractor_path: settings
                .post_processing
                .external_extractor_path
                .as_ref()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
            post_download_command: settings
                .post_processing
                .post_download_command
                .clone()
                .unwrap_or_default(),
            post_download_timeout_secs: settings
                .post_processing
                .post_download_timeout_secs
                .to_string(),
            watch_enabled: settings.watch_folders.enabled,
            watch_poll_interval_secs: settings.watch_folders.poll_interval_secs.to_string(),
            watch_rules,
        }
    }

    pub(crate) fn add_schedule_rule(&mut self) {
        let mut index = self.schedule_rules.len() + 1;
        while self
            .schedule_rules
            .iter()
            .any(|rule| rule.id == format!("rule-{index}"))
        {
            index += 1;
        }
        self.schedule_rules.push(ScheduleRuleDraft {
            id: format!("rule-{index}"),
            name: format!("Rule {index}"),
            enabled: true,
            days: WeekdayMask::EVERY_DAY,
            start_hhmm: "23:00".to_owned(),
            end_hhmm: "07:00".to_owned(),
            mode: ScheduleRuleMode::FullSpeed,
            speed_bps: "1048576".to_owned(),
        });
    }

    pub(crate) fn add_watch_rule(&mut self, directory: PathBuf) {
        let mut index = self.watch_rules.len() + 1;
        while self
            .watch_rules
            .iter()
            .any(|rule| rule.id == format!("watch-{index}"))
        {
            index += 1;
        }
        self.watch_rules.push(WatchFolderDraft {
            id: format!("watch-{index}"),
            enabled: true,
            directory: directory.to_string_lossy().into_owned(),
            import_torrents: true,
            import_dlc: true,
            import_txt: true,
            category: String::new(),
            processed_behavior: WatchProcessedBehavior::MoveToProcessed,
        });
    }

    pub(crate) fn to_settings(&self) -> Option<AutomationSettings> {
        let completion_countdown_secs =
            self.completion_countdown_secs.trim().parse::<u64>().ok()?;
        let outside_hours = match self.outside_mode {
            OutsideHoursMode::UseGlobalDefault => OutsideHoursAction::UseGlobalDefault,
            OutsideHoursMode::LimitSpeed => OutsideHoursAction::LimitSpeed {
                max_bytes_per_sec: self.outside_speed_bps.trim().parse::<u64>().ok()?,
            },
            OutsideHoursMode::PauseQueue => OutsideHoursAction::PauseQueue,
        };
        let mut rules = Vec::with_capacity(self.schedule_rules.len());
        for draft in &self.schedule_rules {
            let start_minute = parse_hhmm(draft.start_hhmm.trim())?;
            let end_minute = parse_hhmm(draft.end_hhmm.trim())?;
            let action = match draft.mode {
                ScheduleRuleMode::FullSpeed => ScheduleAction::FullSpeed,
                ScheduleRuleMode::SpeedLimit => ScheduleAction::SpeedLimit {
                    max_bytes_per_sec: draft.speed_bps.trim().parse::<u64>().ok()?,
                },
                ScheduleRuleMode::PauseQueue => ScheduleAction::PauseQueue,
            };
            rules.push(ScheduleRule {
                id: draft.id.clone(),
                name: draft.name.trim().to_owned(),
                enabled: draft.enabled,
                days: draft.days,
                start_minute,
                end_minute,
                action,
            });
        }
        let post_download_timeout_secs =
            self.post_download_timeout_secs.trim().parse::<u64>().ok()?;
        let watch_poll_interval_secs = self.watch_poll_interval_secs.trim().parse::<u64>().ok()?;
        let mut watch_rules = Vec::with_capacity(self.watch_rules.len());
        for draft in &self.watch_rules {
            let dir = draft.directory.trim();
            if dir.is_empty() {
                return None;
            }
            let category = draft.category.trim();
            watch_rules.push(WatchFolderRule {
                id: draft.id.clone(),
                enabled: draft.enabled,
                directory: PathBuf::from(dir),
                import_torrents: draft.import_torrents,
                import_dlc: draft.import_dlc,
                import_txt: draft.import_txt,
                category: (!category.is_empty()).then(|| category.to_owned()),
                destination: None,
                processed_behavior: draft.processed_behavior,
            });
        }
        let external_extractor = self.external_extractor_path.trim();
        let command = self.post_download_command.trim();
        Some(
            AutomationSettings {
                schedule: ScheduleSettings {
                    enabled: self.schedule_enabled,
                    rules,
                    outside_hours,
                },
                prevent_sleep_when_active: self.prevent_sleep_when_active,
                completion_action: self.completion_action,
                completion_countdown_secs,
                post_processing: PostProcessingSettings {
                    auto_verify_checksums: self.auto_verify_checksums,
                    auto_extract: self.auto_extract,
                    extract_into_subfolder: self.extract_into_subfolder,
                    delete_archives_after_extraction: self.delete_archives_after_extraction,
                    archive_passwords: self.archive_passwords.clone(),
                    external_extractor_path: (!external_extractor.is_empty())
                        .then(|| PathBuf::from(external_extractor)),
                    post_download_command: (!command.is_empty()).then(|| command.to_owned()),
                    post_download_timeout_secs,
                },
                watch_folders: WatchFolderSettings {
                    enabled: self.watch_enabled,
                    poll_interval_secs: watch_poll_interval_secs.max(1),
                    rules: watch_rules,
                },
            }
            .normalized(),
        )
    }
}

pub(crate) async fn lan_address() -> Option<IpAddr> {
    ruscello_api::discover_lan_ip_async().await
}

#[derive(Clone)]
pub(crate) struct LanPassword(pub(crate) String);

#[derive(Debug)]
pub(crate) struct HosterDraft {
    pub(crate) host: String,
    pub(crate) username: String,
    pub(crate) password: LanPassword,
    pub(crate) busy: bool,
    pub(crate) result: Option<&'static str>,
}

impl Default for HosterDraft {
    fn default() -> Self {
        Self {
            host: "pixeldrain.com".into(),
            username: String::new(),
            password: LanPassword(String::new()),
            busy: false,
            result: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SidecarDraft {
    pub(crate) ytdlp_custom_path: String,
    pub(crate) ffmpeg_custom_path: String,
    pub(crate) status: Option<ruscello_engine::SidecarsStatus>,
    pub(crate) busy: Option<ruscello_engine::SidecarKind>,
    pub(crate) message: Option<&'static str>,
}

impl SidecarDraft {
    pub(crate) fn from_settings(settings: &ruscello_engine::SidecarSettings) -> Self {
        Self {
            ytdlp_custom_path: settings
                .ytdlp_custom_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            ffmpeg_custom_path: settings
                .ffmpeg_custom_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            ..Self::default()
        }
    }

    pub(crate) fn path_for(&self, kind: ruscello_engine::SidecarKind) -> &str {
        match kind {
            ruscello_engine::SidecarKind::YtDlp => &self.ytdlp_custom_path,
            ruscello_engine::SidecarKind::Ffmpeg => &self.ffmpeg_custom_path,
        }
    }

    pub(crate) fn set_path(&mut self, kind: ruscello_engine::SidecarKind, value: String) {
        match kind {
            ruscello_engine::SidecarKind::YtDlp => self.ytdlp_custom_path = value,
            ruscello_engine::SidecarKind::Ffmpeg => self.ffmpeg_custom_path = value,
        }
    }

    pub(crate) fn parse_custom_path(raw: &str) -> Option<PathBuf> {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    }
}

impl std::fmt::Debug for LanPassword {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LanPassword([redacted])")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LanPasswordOutcome {
    pub(crate) keyring_saved: bool,
    pub(crate) hash: Option<String>,
}

const LAN_HASH_FILE: &str = "lan-auth.hash";

pub(crate) async fn load_lan_password_hash(data_dir: Option<PathBuf>) -> Option<String> {
    if let Some(dir) = data_dir.as_ref() {
        let path = dir.join(LAN_HASH_FILE);
        if let Ok(raw) = tokio::fs::read_to_string(&path).await {
            let trimmed = raw.trim().to_owned();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    let from_keyring = tokio::task::spawn_blocking(|| {
        keyring::Entry::new("Ruscello LAN Web UI", "remote-control")
            .and_then(|entry| entry.get_password())
            .ok()
            .filter(|password| !password.is_empty())
    })
    .await
    .ok()
    .flatten()?;
    let hash = ruscello_api::hash_password_async(from_keyring).await.ok()?;
    if let Some(dir) = data_dir.as_ref() {
        let _ = write_lan_hash_file(dir, &hash);
    }
    Some(hash)
}

pub(crate) async fn save_lan_password(
    data_dir: Option<PathBuf>,
    password: LanPassword,
) -> LanPasswordOutcome {
    let hash = ruscello_api::hash_password_async(password.0.clone())
        .await
        .ok();
    if let (Some(dir), Some(digest)) = (data_dir.as_ref(), hash.as_ref())
        && let Err(error) = write_lan_hash_file(dir, digest)
    {
        tracing::debug!(%error, "Could not persist LAN verifier hash");
    }
    let keyring_saved = tokio::task::spawn_blocking(move || {
        keyring::Entry::new("Ruscello LAN Web UI", "remote-control")
            .and_then(|entry| entry.set_password(&password.0))
            .is_ok()
    })
    .await
    .unwrap_or(false);
    LanPasswordOutcome {
        keyring_saved,
        hash,
    }
}

fn write_lan_hash_file(dir: &Path, hash: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let target = dir.join(LAN_HASH_FILE);
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(hash.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(target)?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Release {
    pub(crate) tag_name: String,
    pub(crate) body: Option<String>,
    pub(crate) html_url: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Current,
    Available(Release),
    Failed,
}

pub(crate) async fn check_updates() -> UpdateStatus {
    match latest_release().await {
        Ok(Some(release)) => UpdateStatus::Available(release),
        Ok(None) => UpdateStatus::Current,
        Err(error) => {
            tracing::debug!(%error, "Could not check for a release");
            UpdateStatus::Failed
        }
    }
}

async fn latest_release() -> anyhow::Result<Option<Release>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("Ruscello/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(12))
        .build()?;
    let response = client
        .get("https://api.github.com/repos/EmanueleFusco/ruscello/releases/latest")
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let release = response.error_for_status()?.json::<Release>().await?;
    let available = semver::Version::parse(release.tag_name.trim_start_matches('v'))?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    let url = ruscello_engine::Url::parse(&release.html_url)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.path().starts_with("/EmanueleFusco/ruscello/releases/"),
        "Unexpected release URL"
    );
    Ok((available > current).then_some(release))
}

pub(crate) fn diagnostics(renderer: crate::preferences::Renderer) -> String {
    format!(
        "Ruscello {}\nPlatform: {} ({})\nRenderer: {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        renderer.backend()
    )
}

pub(crate) async fn open_latest_log() -> bool {
    tokio::task::spawn_blocking(|| {
        let result = (|| -> anyhow::Result<()> {
            let paths = ruscello_platform::paths::AppPaths::discover()?;
            let latest = std::fs::read_dir(paths.logs().join("ruscello"))?
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_ok_and(|kind| kind.is_file())
                        && entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "log")
                })
                .max_by_key(std::fs::DirEntry::file_name)
                .ok_or_else(|| anyhow::anyhow!("No desktop log file found"))?;
            open::that_detached(latest.path())?;
            Ok(())
        })();
        result.is_ok()
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_limits_and_lan_port_without_changing_unedited_settings() -> anyhow::Result<()> {
        let limits = Limits {
            retries: 4,
            ..Limits::default()
        };
        let mut draft = Draft::new(limits, NonZeroU64::MIN, NonZeroU16::MIN);
        assert_eq!(
            draft.parse(limits),
            Some((limits, NonZeroU64::MIN, NonZeroU16::MIN))
        );
        draft.fields[Number::Port.index()] = "65536".into();
        assert!(draft.parse(limits).is_none());
        draft.fields[Number::Port.index()] = "9666".into();
        draft.fields[Number::Bandwidth.index()] = "1048576".into();
        let (edited, _, _) = draft
            .parse(limits)
            .ok_or_else(|| anyhow::anyhow!("valid draft rejected"))?;
        assert_eq!(edited.retries, 4);
        assert_eq!(edited.bandwidth.map(NonZeroU64::get), Some(1_048_576));
        Ok(())
    }

    #[test]
    fn automation_draft_validates_schedule_times_and_watch_directories() -> anyhow::Result<()> {
        let mut draft = AutomationDraft::default();
        draft.add_schedule_rule();
        draft.add_watch_rule(PathBuf::from("/tmp/watch-folder"));
        draft.completion_action = QueueCompletionAction::Sleep;
        draft.completion_countdown_secs = "45".to_owned();
        draft.auto_extract = true;
        draft.archive_passwords.push("secret".to_owned());

        let settings = draft
            .to_settings()
            .ok_or_else(|| anyhow::anyhow!("expected valid automation draft"))?;
        assert_eq!(settings.completion_action, QueueCompletionAction::Sleep);
        assert_eq!(settings.completion_countdown_secs, 45);
        assert_eq!(settings.schedule.rules.len(), 1);
        assert_eq!(settings.watch_folders.rules.len(), 1);
        assert!(settings.post_processing.auto_extract);

        draft.schedule_rules[0].start_hhmm = "25:99".to_owned();
        assert!(draft.to_settings().is_none());
        Ok(())
    }
}
