use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use iced::{
    Animation, Element, Size, Subscription, Task, clipboard, event,
    futures::{Stream, stream},
    keyboard,
    widget::{operation, scrollable::AbsoluteOffset, text_editor},
    window,
};
use ruscello_api::{
    ApiConfig, ApiController, ApiEvent, ApiHandle, ApiRuntimeUpdate, ApiServer, DEFAULT_LOCAL_PORT,
    LanStatus, PairingOverview,
};
use ruscello_core::{CategoryId, DownloadId, FileType, PackageId, Source, Status, TorrentSource};
use ruscello_engine::{
    CandidateGroup, EngineEvent, EngineHandle, EngineLog, EngineOptions, EngineSnapshot,
    MediaError, PageAnalysis, SidecarKind, SidecarSettings, SidecarsStatus, TorrentDetailsSnapshot,
    TorrentFilePriority, TorrentInspection, TorrentSessionTelemetry, Url,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    actions::{self, Action, Command, Shortcut},
    add::{self, Dialog, Input},
    desktop,
    i18n::Strings,
    preferences::{Appearance, Locale, Preferences, Renderer},
    queue::{self, Queue, StatusFilter},
    settings::{self, Draft, Folder, Number, Page, UpdateStatus},
    theme, view,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DetailTab {
    #[default]
    General,
    Segments,
    Pieces,
    Files,
    Peers,
    Trackers,
    Log,
}

const MOTION_DURATION: Duration = Duration::from_millis(144);
pub(crate) const DETAILS_COLLAPSED_HEIGHT: f32 = 48.0;
const DETAILS_HEIGHT: f32 = 158.0;

impl DetailTab {
    pub(crate) fn allowed(self, is_torrent: bool) -> bool {
        match self {
            Self::General | Self::Log => true,
            Self::Segments => !is_torrent,
            Self::Pieces | Self::Files | Self::Peers | Self::Trackers => is_torrent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Column {
    Size,
    Progress,
    Speed,
    Eta,
    Status,
}

impl Column {
    pub(crate) const ALL: [Self; 5] = [
        Self::Size,
        Self::Progress,
        Self::Speed,
        Self::Eta,
        Self::Status,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Progress => "progress",
            Self::Speed => "speed",
            Self::Eta => "eta",
            Self::Status => "status",
        }
    }

    pub(crate) fn width(self) -> u32 {
        match self {
            Self::Size => 70,
            Self::Progress => 102,
            Self::Speed => 78,
            Self::Eta => 85,
            Self::Status => 110,
        }
    }

    pub(crate) fn mask(self) -> u8 {
        1 << self as u8
    }
}

#[derive(Clone)]
pub(crate) struct ApiStartup {
    pub(crate) handle: Arc<tokio::sync::Mutex<Option<ApiHandle>>>,
    pub(crate) controller: ApiController,
    pub(crate) lan_status: LanStatus,
    pub(crate) pairing: PairingOverview,
    pub(crate) password_hash: Option<String>,
}

impl std::fmt::Debug for ApiStartup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiStartup")
            .field("local_addr", &self.controller.local_addr())
            .field("lan_status", &self.lan_status)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Noop,
    Started(Option<EngineHandle>),
    ApiStarted(Option<Box<ApiStartup>>),
    Api(ApiEvent),
    Snapshot(Option<EngineSnapshot>),
    Event(EngineEvent),
    DiskSpace(Option<u64>),
    Close,
    Stopped(bool),
    Search(String),
    Status(StatusFilter),
    FileType(Option<FileType>),
    Category(Option<CategoryId>),
    Select(DownloadId),
    Package(PackageId),
    Scrolled {
        offset: f32,
        height: f32,
    },
    Resize(Size),
    SystemTheme(iced::theme::Mode),
    ToggleDetails,
    Motion(bool),
    AnimationFrame(Instant),
    DetailTab(DetailTab),
    Settings(bool),
    Locale(Locale),
    Appearance(Appearance),
    Saved(bool),
    Dismiss,
    ToggleColumns,
    ToggleColumn(Column),
    OpenAdd,
    CloseAdd,
    Clipboard(Option<String>),
    EditAdd(text_editor::Action),
    Files(Vec<PathBuf>),
    FilesLoaded(Vec<Input>),
    PickFiles,
    PickDestination,
    Destination(Option<PathBuf>),
    AddCategory(Option<CategoryId>),
    AddAdvanced,
    AddConnections(String),
    AddBandwidth(String),
    AddChecksum(String),
    StartImmediately(bool),
    Probe(Url, add::Probe),
    Imported(
        PathBuf,
        Result<Vec<ruscello_engine::ImportedPackage>, &'static str>,
    ),
    SelectLink(Url, bool),
    GrabberSearch(String),
    GrabberFilter(Option<ruscello_engine::CandidateGroup>),
    GrabberGroup(ruscello_engine::CandidateGroup),
    DlcFallback(bool),
    BrowserHelper(bool),
    CookieSite(String),
    ImportCookies(ruscello_engine::InstalledBrowser),
    CookiesImported(Option<usize>),
    OpenVerification(DownloadId),
    BrowserResult(Option<&'static str>),
    AnalyzeInBrowser(Url, bool),
    AutoAcceptHosters(bool),
    MonitorClipboard(bool),
    ClipboardTick,
    ClipboardMonitor(Option<String>),
    HosterDomain(String),
    HosterUsername(String),
    HosterPassword(settings::LanPassword),
    SaveHoster,
    RemoveHoster,
    HosterSaved(&'static str),
    TorrentInspected(String, Result<TorrentInspection, String>),
    ToggleTorrentFile(String, usize),
    ToggleAllTorrentFiles(String, bool),
    InspectMedia(Url),
    SelectMediaFormat(Url, String),
    TogglePlaylistItem(Url, usize),
    ToggleAllPlaylistItems(Url, bool),
    SubmitAdd,
    Added(bool),
    Modifiers(keyboard::Modifiers),
    Shortcut(Shortcut),
    Menu(DownloadId),
    Hover(Option<DownloadId>),
    RowAction(Action),
    SingleAction(DownloadId, Action),
    DeleteFiles(bool),
    ConfirmRemove,
    CancelRemove,
    PauseAll,
    AlternateSpeed,
    AlternateApplied(bool, bool),
    ActionResult(bool),
    SettingsPage(Page),
    Renderer(Renderer),
    #[cfg(windows)]
    NvidiaProfile(crate::nvidia::Action),
    #[cfg(windows)]
    NvidiaProfileResult(crate::nvidia::Outcome),
    SettingNumber(Number, String),
    SaveLimits,
    ChooseFolder(Folder),
    SettingsFolder(Folder, Option<PathBuf>),
    ClearTemporary,
    LanEnabled(bool),
    LanAuth(bool),
    LanAddress(Option<std::net::IpAddr>),
    LanStatusUpdated(Option<LanStatus>),
    LanPassword(settings::LanPassword),
    SaveLanPassword,
    LanPasswordSaved(settings::LanPasswordOutcome),
    CopyLanUrl,
    OpenLanUrl,
    StartPairing,
    CancelPairing,
    ApprovePairing(String),
    RejectPairing(String),
    RevokePairing,
    PairingTick,
    CheckAutomatically(bool),
    CheckUpdates,
    UpdateChecked(UpdateStatus),
    OpenRelease,
    CopyDiagnostics,
    OpenLogs,
    TorrentOption(settings::TorrentOption, bool),
    TorrentField(settings::TorrentField, String),
    SaveTorrentSettings,
    PickBlocklist,
    BlocklistPicked(Option<PathBuf>),
    TorrentSettingsApplied(Option<TorrentSessionTelemetry>),
    TorrentDetails(DownloadId, Option<TorrentDetailsSnapshot>),
    SetTorrentFilePriority(DownloadId, usize, bool),
    TorrentFilePriorityUpdated(DownloadId, bool, Option<TorrentDetailsSnapshot>),
    CategoryEdit(settings::CategoryEdit, String),
    CategoryTarget(ruscello_core::CategoryId),
    AddCategoryRule,
    CreateCategory,
    RemoveCategoryRule(usize),
    SetupTray,
    Tray(desktop::Event),
    Incoming(Vec<String>),
    CloseToTray(bool),
    Notifications(bool),
    Autostart(bool),
    AutostartApplied(bool, bool),
    #[cfg(any(windows, target_os = "linux"))]
    RegisterAssociations,
    SidecarConsent(SidecarKind, bool),
    SidecarAutoUpdate(bool),
    SidecarCustomPath(SidecarKind, String),
    PickSidecarPath(SidecarKind),
    SidecarPathPicked(SidecarKind, Option<PathBuf>),
    SaveSidecarSettings,
    SidecarStatusLoaded(Option<SidecarsStatus>),
    InstallSidecar(SidecarKind),
    SidecarInstallResult(SidecarKind, Result<SidecarsStatus, &'static str>),
    SidecarAutoUpdated(Option<SidecarsStatus>),
}

pub(crate) struct App {
    selection_motion: Animation<bool>,
    details_motion: Animation<bool>,
    frame: Instant,
    window: window::Id,
    engine: Option<EngineHandle>,
    data_dir: Option<PathBuf>,
    api: Option<Arc<tokio::sync::Mutex<Option<ApiHandle>>>>,
    pub(crate) api_control: Option<ApiController>,
    pub(crate) queue: Queue,
    pub(crate) strings: Strings,
    pub(crate) preferences: Preferences,
    config: PathBuf,
    destination: PathBuf,
    pub(crate) disk_space: Option<u64>,
    pub(crate) booting: bool,
    closing: bool,
    saving: bool,
    dirty: bool,
    pub(crate) settings: bool,
    pub(crate) detail_tab: DetailTab,
    pub(crate) offset: f32,
    pub(crate) viewport_height: f32,
    pub(crate) banner: Option<&'static str>,
    pub(crate) show_columns: bool,
    system_theme: iced::theme::Mode,
    pub(crate) add: Option<Dialog>,
    pub(crate) editor: text_editor::Content,
    pending_probes: VecDeque<Url>,
    probe_tokens: HashMap<Url, CancellationToken>,
    clipboard_seen: Option<String>,
    in_flight: usize,
    inspect_tokens: HashMap<String, CancellationToken>,
    pub(crate) torrent_details: Option<(DownloadId, TorrentDetailsSnapshot)>,
    details_in_flight: bool,
    pub(crate) selection: BTreeSet<DownloadId>,
    modifiers: keyboard::Modifiers,
    pub(crate) menu: Option<DownloadId>,
    pub(crate) hovered: Option<DownloadId>,
    pub(crate) remove: Option<(Vec<DownloadId>, bool)>,
    deferred_inputs: Vec<String>,
    alternate_speed: bool,
    alternate_pending: bool,
    pub(crate) page: Page,
    pub(crate) settings_draft: Draft,
    pub(crate) renderer: Renderer,
    #[cfg(windows)]
    pub(crate) nvidia_profile: crate::nvidia::State,
    #[cfg(windows)]
    pub(crate) nvidia_restart: bool,
    #[cfg(windows)]
    nvidia_busy: bool,
    pub(crate) lan_address: Option<std::net::IpAddr>,
    pub(crate) lan_status: LanStatus,
    pub(crate) lan_password: settings::LanPassword,
    pub(crate) lan_password_hash: Option<String>,
    pub(crate) pairing: PairingOverview,
    pub(crate) hoster_draft: settings::HosterDraft,
    pub(crate) sidecar_draft: settings::SidecarDraft,
    pub(crate) cookie_site: String,
    pub(crate) cookie_importing: bool,
    pub(crate) cookie_count: Option<usize>,
    pub(crate) update_status: UpdateStatus,
    pub(crate) category_draft: settings::CategoryDraft,
    default_destination: PathBuf,
    tray: Option<tray_icon::TrayIcon>,
    tray_events: desktop::Receiver<desktop::Event>,
    ipc_events: desktop::Receiver<Vec<String>>,
}

impl App {
    pub(crate) fn boot(
        preferences: Preferences,
        config: PathBuf,
        database: PathBuf,
        destination: PathBuf,
        restore_failed: bool,
        strings: Strings,
        desktop: desktop::Startup,
    ) -> (Self, Task<Message>) {
        let (window, open) = window::open(window::Settings {
            size: Size::new(1120.0, 720.0),
            min_size: Some(Size::new(950.0, 490.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        });
        let default_destination = destination;
        let destination = preferences
            .default_folder
            .clone()
            .unwrap_or_else(|| default_destination.clone());
        let mut options = EngineOptions::new(database, destination.clone());
        options.temporary_directory = preferences.temporary_folder.clone();
        options.data_dir = options.database.parent().map(Path::to_path_buf);
        options.browser = options
            .data_dir
            .clone()
            .and_then(|directory| ruscello_engine::BrowserOptions::installed(directory).ok());
        options.browser_enabled = preferences.browser_helper;
        options.eager_torrent_session = true;
        options.resolver =
            ruscello_engine::Resolver::new(config.parent().map(|parent| parent.join("recipes")))
                .ok();
        if let Some(torrent) = preferences.torrent.to_settings() {
            options.torrent = torrent;
        }
        options.sidecars = preferences.sidecars.clone();
        let data_dir = options.data_dir.clone();
        let sidecar_draft = settings::SidecarDraft::from_settings(&preferences.sidecars);
        let app = Self {
            selection_motion: view_animation(true),
            details_motion: view_animation(preferences.details),
            frame: Instant::now(),
            window,
            engine: None,
            data_dir,
            api: None,
            api_control: None,
            queue: Queue::default(),
            strings,
            renderer: preferences.renderer,
            #[cfg(windows)]
            nvidia_profile: crate::nvidia::State::Checking,
            #[cfg(windows)]
            nvidia_restart: false,
            #[cfg(windows)]
            nvidia_busy: false,
            preferences,
            config,
            destination,
            disk_space: None,
            booting: true,
            closing: false,
            saving: false,
            dirty: false,
            settings: false,
            detail_tab: DetailTab::General,
            offset: 0.0,
            viewport_height: 720.0,
            banner: restore_failed.then_some("restore-failed"),
            show_columns: false,
            system_theme: iced::theme::Mode::default(),
            add: None,
            editor: text_editor::Content::new(),
            pending_probes: VecDeque::new(),
            probe_tokens: HashMap::new(),
            clipboard_seen: None,
            in_flight: 0,
            inspect_tokens: HashMap::new(),
            torrent_details: None,
            details_in_flight: false,
            selection: BTreeSet::new(),
            modifiers: keyboard::Modifiers::default(),
            menu: None,
            hovered: None,
            remove: None,
            deferred_inputs: Vec::new(),
            alternate_speed: false,
            alternate_pending: false,
            page: Page::General,
            settings_draft: Draft::default(),
            lan_address: None,
            lan_status: LanStatus::Disabled,
            lan_password: settings::LanPassword(String::new()),
            lan_password_hash: None,
            pairing: PairingOverview::default(),
            hoster_draft: settings::HosterDraft::default(),
            sidecar_draft,
            cookie_site: String::new(),
            cookie_importing: false,
            cookie_count: None,
            update_status: UpdateStatus::Idle,
            category_draft: settings::CategoryDraft::default(),
            default_destination,
            tray: None,
            tray_events: desktop.tray,
            ipc_events: desktop.ipc,
        };
        let start = Task::perform(
            async {
                EngineHandle::start(options)
                    .await
                    .inspect_err(|error| tracing::debug!(%error, "Could not start the queue"))
                    .ok()
            },
            Message::Started,
        );
        (
            app,
            Task::batch([
                open.map(|_| Message::SetupTray),
                start,
                iced::system::theme().map(Message::SystemTheme),
                Task::done(Message::Incoming(desktop.inputs)),
            ]),
        )
    }

    pub(crate) fn title(&self, _: window::Id) -> String {
        self.strings.get("app-name")
    }

    pub(crate) fn theme(&self, _: window::Id) -> Option<iced::Theme> {
        let appearance = match (self.preferences.appearance, self.system_theme) {
            (Appearance::System, iced::theme::Mode::Dark) => Appearance::Dark,
            (Appearance::System, _) => Appearance::Light,
            (appearance, _) => appearance,
        };
        theme::theme(appearance)
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        let engine = self
            .engine
            .as_ref()
            .map_or_else(Subscription::none, |engine| {
                Subscription::run_with(
                    QueueSubscription {
                        window: self.window,
                        engine: engine.clone(),
                    },
                    queue_events,
                )
            });
        let api = self
            .api_control
            .as_ref()
            .map_or_else(Subscription::none, |control| {
                Subscription::run_with(
                    ApiSubscription {
                        window: self.window,
                        control: control.clone(),
                    },
                    api_events,
                )
            });
        Subscription::batch([
            engine,
            api,
            if self.pairing.window_active {
                iced::time::every(Duration::from_secs(1)).map(|_| Message::PairingTick)
            } else {
                Subscription::none()
            },
            if self.preferences.monitor_clipboard {
                iced::time::every(Duration::from_secs(2)).map(|_| Message::ClipboardTick)
            } else {
                Subscription::none()
            },
            if self.preferences.motion
                && (self.selection_motion.is_animating(self.frame)
                    || self.details_motion.is_animating(self.frame))
            {
                iced::time::every(Duration::from_millis(16)).map(Message::AnimationFrame)
            } else {
                Subscription::none()
            },
            window::close_requests().map(|_| Message::Close),
            window::resize_events().map(|(_, size)| Message::Resize(size)),
            iced::system::theme_changes().map(Message::SystemTheme),
            event::listen_with(actions::event),
            Subscription::run_with(
                DesktopSubscription(self.window, self.tray_events.clone()),
                tray_events,
            ),
            Subscription::run_with(
                DesktopSubscription(self.window, self.ipc_events.clone()),
                ipc_events,
            ),
        ])
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        let selection = (self.detail_tab, self.page);
        let details = self.preferences.details;
        let task = self.handle(message);
        if selection != (self.detail_tab, self.page) {
            self.frame = self.frame.max(Instant::now());
            self.selection_motion = view_animation(false).go(true, self.frame);
        }
        if details != self.preferences.details {
            self.frame = self.frame.max(Instant::now());
            self.details_motion
                .go_mut(self.preferences.details, self.frame);
        }
        task
    }

    pub(crate) fn selection_opacity(&self) -> f32 {
        if self.preferences.motion {
            self.selection_motion.interpolate(0.65, 1.0, self.frame)
        } else {
            1.0
        }
    }

    pub(crate) fn details_height(&self) -> f32 {
        DETAILS_COLLAPSED_HEIGHT
            + (DETAILS_HEIGHT - DETAILS_COLLAPSED_HEIGHT) * self.details_progress()
    }

    pub(crate) fn details_tabs_opacity(&self) -> f32 {
        // Finish fading before the shrinking panel starts clipping the tabs.
        ((self.details_progress() - 0.12) / 0.88).clamp(0.0, 1.0)
    }

    fn details_progress(&self) -> f32 {
        if self.preferences.motion {
            self.details_motion.interpolate(0.0, 1.0, self.frame)
        } else if self.preferences.details {
            1.0
        } else {
            0.0
        }
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => {}
            Message::AnimationFrame(now) => self.frame = self.frame.max(now),
            Message::Motion(enabled) => {
                self.preferences.motion = enabled;
                self.selection_motion = view_animation(true);
                self.details_motion = view_animation(self.preferences.details);
                return self.save_preferences();
            }
            Message::Started(engine) => {
                self.engine = engine;
                if self.engine.is_none() {
                    self.booting = false;
                    self.banner = Some("startup-failed");
                }
                return if self.closing && !self.booting {
                    self.finish_close()
                } else {
                    let check = if self.preferences.check_updates && !self.closing {
                        self.update_settings(Message::CheckUpdates)
                    } else {
                        Task::none()
                    };
                    let auto_update = self.maybe_auto_update_ytdlp();
                    let start_api = self.start_api();
                    Task::batch([
                        self.check_disk(),
                        check,
                        self.run_probes(),
                        self.refresh_sidecar_status(),
                        auto_update,
                        start_api,
                    ])
                };
            }
            Message::ApiStarted(Some(startup)) => {
                self.api = Some(startup.handle);
                self.api_control = Some(startup.controller);
                self.lan_status = startup.lan_status;
                self.pairing = startup.pairing;
                self.lan_password_hash = startup.password_hash;
                if let LanStatus::Active {
                    bind_addr: _,
                    display_url: _,
                } = &self.lan_status
                {
                    return Task::perform(settings::lan_address(), Message::LanAddress);
                }
            }
            Message::ApiStarted(None) => {}
            Message::Api(event) => match event {
                ApiEvent::LanStatusChanged(status) => self.lan_status = status,
                ApiEvent::PairingChanged(overview) => self.pairing = overview,
                ApiEvent::AlternateSpeedChanged(rate) => self.alternate_speed = rate.is_some(),
                ApiEvent::QueueRequested { .. } => {}
                ApiEvent::AnalyzePage { url, .. } => {
                    return self.update(Message::Incoming(vec![url]));
                }
            },
            Message::Snapshot(snapshot) => {
                self.booting = false;
                if let Some(snapshot) = snapshot {
                    self.alternate_speed = snapshot.temporary_bandwidth.is_some();
                    if self.preferences.sidecars == SidecarSettings::default()
                        && snapshot.sidecars != SidecarSettings::default()
                    {
                        self.preferences.sidecars = snapshot.sidecars.clone();
                        self.sidecar_draft =
                            settings::SidecarDraft::from_settings(&snapshot.sidecars);
                    }
                    self.queue.replace(snapshot);
                    self.selection
                        .retain(|id| self.queue.downloads.contains_key(id));
                    if self
                        .torrent_details
                        .as_ref()
                        .is_some_and(|(id, _)| self.queue.selected != Some(*id))
                    {
                        self.torrent_details = None;
                    }
                    if self.closing {
                        return self.finish_close();
                    }
                    let details = if self.preferences.details && self.selected_is_torrent() {
                        self.refresh_torrent_details()
                    } else {
                        Task::none()
                    };
                    let scroll = if self.offset >= self.queue.rows.len() as f32 * queue::ROW_HEIGHT
                    {
                        self.reset_scroll()
                    } else {
                        Task::none()
                    };
                    return Task::batch([details, scroll]);
                } else {
                    self.banner = Some("queue-unavailable");
                    if self.closing {
                        return self.finish_close();
                    }
                }
            }
            Message::Event(EngineEvent::Progress(updates)) => {
                self.queue.progress(updates, Instant::now())
            }
            Message::Event(EngineEvent::TorrentProgress(updates)) => {
                self.queue.torrent_progress(updates);
                if self.preferences.details && self.selected_is_torrent() && !self.details_in_flight
                {
                    return self.refresh_torrent_details();
                }
            }
            Message::Event(EngineEvent::TorrentTelemetry(telemetry)) => {
                self.queue.torrent_telemetry(telemetry);
            }
            Message::Event(EngineEvent::Log {
                id: Some(id),
                message: EngineLog::VerificationRequired,
            }) => {
                self.queue.log(id, EngineLog::VerificationRequired);
                if self.preferences.notifications {
                    let title = self.strings.get("waiting-user");
                    let body = self
                        .queue
                        .downloads
                        .get(&id)
                        .map_or_else(|| self.strings.get("app-name"), queue::display_name);
                    return Task::future(desktop::notify(title, body));
                }
            }
            Message::Event(EngineEvent::Log {
                id: Some(id),
                message,
            }) => self.queue.log(id, message),
            Message::Event(EngineEvent::Log { id: None, message }) => {
                self.banner = Some(view::log_key(message))
            }
            Message::Event(EngineEvent::StatusChanged { id, status }) => {
                let notice = if self.preferences.notifications
                    && matches!(status, Status::Completed | Status::Failed { .. })
                {
                    let title = self.strings.get(i18n_status_key(status));
                    let body = self
                        .queue
                        .downloads
                        .get(&id)
                        .map_or_else(|| self.strings.get("app-name"), queue::display_name);
                    Task::future(desktop::notify(title, body))
                } else {
                    Task::none()
                };
                return Task::batch([notice, self.check_disk()]);
            }
            Message::Event(EngineEvent::Removed(_)) => return self.check_disk(),
            Message::Event(_) => {}
            Message::DiskSpace(bytes) => self.disk_space = bytes,
            Message::Close => {
                if self.preferences.close_to_tray && self.tray.is_some() {
                    return window::set_mode(self.window, window::Mode::Hidden);
                }
                return self.quit();
            }
            Message::Tray(desktop::Event::Quit) => return self.quit(),
            Message::Tray(desktop::Event::Show) => return self.show_window(),
            Message::Tray(desktop::Event::PauseAll) => return self.interaction(Message::PauseAll),
            Message::SetupTray => {
                self.tray = desktop::tray(&self.strings)
                    .inspect_err(|_| tracing::debug!("System tray unavailable"))
                    .ok();
            }
            Message::Incoming(inputs) => {
                if self.add.as_ref().is_some_and(|dialog| dialog.submitting) {
                    self.deferred_inputs.extend(inputs);
                    return self.show_window();
                }
                if !inputs.is_empty() {
                    self.settings = false;
                    let dialog = self.add.get_or_insert_with(Dialog::default);
                    if !dialog.text.is_empty() {
                        dialog.text.push('\n');
                    }
                    dialog.text.push_str(&inputs.join("\n"));
                    self.editor = text_editor::Content::with_text(&dialog.text);
                    return Task::batch([self.refresh_add(), self.show_window()]);
                }
                return self.show_window();
            }
            Message::CloseToTray(enabled) => {
                if enabled && self.tray.is_none() {
                    self.banner = Some("tray-unavailable");
                    return Task::none();
                }
                self.preferences.close_to_tray = enabled;
                return self.save_preferences();
            }
            Message::Notifications(enabled) => {
                self.preferences.notifications = enabled;
                return self.save_preferences();
            }
            Message::Autostart(enabled) => {
                return Task::perform(desktop::autostart(enabled), move |success| {
                    Message::AutostartApplied(enabled, success)
                });
            }
            Message::AutostartApplied(enabled, success) => {
                if success {
                    self.preferences.autostart = enabled;
                    return self.save_preferences();
                }
                self.banner = Some("action-failed");
            }
            #[cfg(any(windows, target_os = "linux"))]
            Message::RegisterAssociations => {
                return Task::perform(desktop::associations(), Message::ActionResult);
            }
            Message::Stopped(saved) => {
                self.engine = None;
                if saved {
                    return iced::exit();
                }
                self.closing = false;
                self.banner = Some("shutdown-failed");
            }
            Message::Search(value) => {
                self.queue.search = value;
                return self.filter_changed();
            }
            Message::Status(status) => {
                self.queue.status = status;
                return self.filter_changed();
            }
            Message::FileType(file_type) => {
                self.queue.file_type = file_type;
                return self.filter_changed();
            }
            Message::Category(category) => {
                self.queue.category = category;
                return self.filter_changed();
            }
            Message::Select(id) => {
                self.select(id);
                if self.preferences.details && self.selected_is_torrent() {
                    return self.refresh_torrent_details();
                }
            }
            Message::Package(id) => {
                if !self.queue.collapsed.remove(&id) {
                    self.queue.collapsed.insert(id);
                }
                self.queue.rebuild();
            }
            Message::Scrolled { offset, height } => {
                self.offset = offset;
                self.viewport_height = height;
            }
            Message::Resize(size) => self.viewport_height = size.height,
            Message::SystemTheme(mode) => self.system_theme = mode,
            Message::ToggleDetails => {
                self.preferences.details = !self.preferences.details;
                let refresh = if self.preferences.details && self.selected_is_torrent() {
                    self.refresh_torrent_details()
                } else {
                    Task::none()
                };
                return Task::batch([self.save_preferences(), refresh]);
            }
            Message::DetailTab(tab) => {
                self.detail_tab = tab;
                if self.preferences.details && self.selected_is_torrent() {
                    return self.refresh_torrent_details();
                }
            }
            Message::Settings(open) => {
                self.settings = open;
                if !open {
                    self.hoster_draft.password.0.clear();
                }
                if open {
                    self.settings_draft = Draft::new(
                        self.queue.limits,
                        self.preferences.alternate_bandwidth,
                        self.preferences.lan_port,
                    );
                    #[cfg(windows)]
                    if self.page == Page::General {
                        return Task::batch([self.reset_scroll(), self.read_nvidia_profile()]);
                    }
                }
                return self.reset_scroll();
            }
            Message::Locale(locale) => {
                self.preferences.locale = locale;
                self.strings.set_locale(locale);
                if self.tray.is_some()
                    && let Ok(tray) = desktop::tray(&self.strings)
                {
                    self.tray = Some(tray);
                }
                return Task::batch([self.save_preferences(), self.sync_api()]);
            }
            Message::Appearance(appearance) => {
                self.preferences.appearance = appearance;
                return self.save_preferences();
            }
            Message::Saved(saved) => {
                self.saving = false;
                if !saved {
                    self.banner = Some("save-failed");
                }
                if self.dirty {
                    return self.save_preferences();
                }
                if self.closing {
                    return self.finish_close();
                }
            }
            Message::Dismiss => self.banner = None,
            Message::ToggleColumns => self.show_columns = !self.show_columns,
            Message::ToggleColumn(column) => {
                self.preferences.columns ^= column.mask();
                return self.save_preferences();
            }
            message => return self.interaction(message),
        }
        Task::none()
    }

    fn interaction(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::OpenAdd => {
                self.settings = false;
                self.menu = None;
                self.add.get_or_insert_with(Dialog::default);
                return Task::batch([
                    clipboard::read().map(Message::Clipboard),
                    operation::focus(view::add_id()),
                ]);
            }
            Message::CloseAdd => {
                if self.add.as_ref().is_some_and(|dialog| dialog.submitting) {
                    return Task::none();
                }
                self.add = None;
                self.editor = text_editor::Content::new();
                self.pending_probes.clear();
                for token in self.probe_tokens.values() {
                    token.cancel();
                }
                for token in self.inspect_tokens.drain().map(|(_, token)| token) {
                    token.cancel();
                }
            }
            Message::Clipboard(Some(value)) => {
                if let Some(dialog) = &mut self.add
                    && dialog.text.is_empty()
                    && add::parse(&value)
                        .iter()
                        .any(|input| matches!(input, Input::Http(_) | Input::Torrent(_)))
                {
                    self.editor = text_editor::Content::with_text(&value);
                    dialog.text = value;
                    return self.refresh_add();
                }
            }
            Message::EditAdd(action) => {
                if let Some(dialog) = &mut self.add {
                    let edited = action.is_edit();
                    self.editor.perform(action);
                    if edited {
                        dialog.text = self.editor.text();
                        return self.refresh_add();
                    }
                }
            }
            Message::Files(paths) => {
                if paths.is_empty() {
                    return Task::none();
                }
                if self.add.as_ref().is_some_and(|dialog| dialog.submitting) {
                    self.deferred_inputs.extend(
                        paths
                            .iter()
                            .map(|path| crate::instance::normalize(path.as_os_str())),
                    );
                    return Task::none();
                }
                self.settings = false;
                self.add.get_or_insert_with(Dialog::default);
                return Task::perform(add::load_files(paths), Message::FilesLoaded);
            }
            Message::FilesLoaded(inputs) => {
                if let Some(dialog) = &mut self.add {
                    if dialog.submitting {
                        self.deferred_inputs.extend(inputs.iter().map(Input::label));
                        return Task::none();
                    }
                    dialog.files.extend(inputs);
                    return self.refresh_add();
                }
            }
            Message::PickFiles => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .add_filter(
                                "Torrent / DLC / TXT / URL / Metalink / M3U",
                                &[
                                    "torrent", "dlc", "txt", "url", "metalink", "meta4", "m3u",
                                    "m3u8",
                                ],
                            )
                            .pick_files()
                            .await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|file| file.path().to_path_buf())
                            .collect()
                    },
                    Message::Files,
                );
            }
            Message::PickDestination => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_folder()
                            .await
                            .map(|folder| folder.path().to_path_buf())
                    },
                    Message::Destination,
                );
            }
            Message::Destination(path) => {
                if let (Some(dialog), Some(path)) = (&mut self.add, path) {
                    dialog.destination = Some(path);
                }
            }
            Message::AddCategory(category) => {
                if let Some(dialog) = &mut self.add {
                    dialog.category = category;
                }
            }
            Message::AddAdvanced => {
                if let Some(dialog) = &mut self.add {
                    dialog.advanced = !dialog.advanced;
                }
            }
            Message::AddConnections(value) => {
                if let Some(dialog) = &mut self.add {
                    dialog.connections = value;
                }
            }
            Message::AddBandwidth(value) => {
                if let Some(dialog) = &mut self.add {
                    dialog.bandwidth = value;
                }
            }
            Message::AddChecksum(value) => {
                if let Some(dialog) = &mut self.add {
                    dialog.checksum = value;
                }
            }
            Message::StartImmediately(start) => {
                if let Some(dialog) = &mut self.add {
                    dialog.paused = !start;
                }
            }
            Message::Probe(url, preview) => {
                self.in_flight = self.in_flight.saturating_sub(1);
                let active = self
                    .probe_tokens
                    .remove(&url)
                    .is_some_and(|token| !token.is_cancelled());
                if active && let Some(dialog) = &mut self.add {
                    dialog.set_probe(url, preview);
                }
                let probes = self.run_probes();
                let automatic = self.auto_accept_hosters();
                return Task::batch([probes, automatic]);
            }
            Message::Imported(path, result) => {
                if let Some(dialog) = &mut self.add {
                    dialog.set_import(path, result);
                }
            }
            Message::SelectLink(url, selected) => {
                if let Some(dialog) = &mut self.add {
                    if selected {
                        dialog.selected_links.insert(url);
                    } else {
                        dialog.selected_links.remove(&url);
                    }
                }
            }
            Message::GrabberSearch(value) => {
                if let Some(dialog) = &mut self.add {
                    dialog.grabber_search = value;
                }
            }
            Message::GrabberFilter(filter) => {
                if let Some(dialog) = &mut self.add {
                    dialog.grabber_filter = filter;
                }
            }
            Message::GrabberGroup(group) => {
                if let Some(dialog) = &mut self.add
                    && !dialog.collapsed_groups.remove(&group)
                {
                    dialog.collapsed_groups.insert(group);
                }
            }
            Message::DlcFallback(enabled) => {
                self.preferences.dlc_fallback = enabled;
                return self.save_preferences();
            }
            Message::BrowserHelper(enabled) => {
                self.preferences.browser_helper = enabled;
                if let Some(engine) = &self.engine {
                    engine.enable_browser_helper(enabled);
                }
                return self.save_preferences();
            }
            Message::CookieSite(site) => {
                self.cookie_site = site;
                self.cookie_count = None;
            }
            Message::ImportCookies(browser) => {
                if self.cookie_importing || !self.preferences.browser_helper {
                    return Task::none();
                }
                let site = self.cookie_site.trim();
                let url = if site.contains("://") {
                    Url::parse(site)
                } else {
                    Url::parse(&format!("https://{site}"))
                };
                let Ok(url) = url else {
                    self.banner = Some("cookie-site-invalid");
                    return Task::none();
                };
                let Some(engine) = self.engine.clone() else {
                    return Task::none();
                };
                self.cookie_count = None;
                self.cookie_importing = true;
                return Task::perform(
                    async move { engine.import_browser_cookies(browser, &url).await.ok() },
                    Message::CookiesImported,
                );
            }
            Message::CookiesImported(count) => {
                self.cookie_importing = false;
                self.cookie_count = count;
                if count.is_none() {
                    self.banner = Some("browser-unavailable");
                }
            }
            Message::OpenVerification(id) => {
                self.menu = None;
                if !self.preferences.browser_helper {
                    self.banner = Some("browser-disabled");
                    return Task::none();
                }
                if let Some(engine) = self.engine.clone() {
                    let locale = self.browser_locale();
                    return Task::perform(
                        async move {
                            engine
                                .open_verification(id, locale)
                                .await
                                .err()
                                .and_then(|error| browser_error(&error))
                        },
                        Message::BrowserResult,
                    );
                }
            }
            Message::BrowserResult(Some(key)) => self.banner = Some(key),
            Message::AnalyzeInBrowser(url, verify) => {
                if !self.preferences.browser_helper {
                    self.banner = Some("browser-disabled");
                    return Task::none();
                }
                let Some(engine) = self.engine.clone() else {
                    return Task::none();
                };
                if self.probe_tokens.contains_key(&url) {
                    return Task::none();
                }
                let Some(dialog) = &mut self.add else {
                    return Task::none();
                };
                dialog.previews.insert(url.clone(), add::Probe::Pending);
                let cancellation = CancellationToken::new();
                self.probe_tokens.insert(url.clone(), cancellation.clone());
                self.in_flight += 1;
                let locale = self.browser_locale();
                return Task::perform(
                    async move {
                        let result = if verify {
                            engine.verify_link(&url, locale, &cancellation).await
                        } else {
                            Ok(())
                        };
                        let result = match result {
                            Ok(()) => engine.inspect_link_in_browser(&url, &cancellation).await,
                            Err(error) => Err(error),
                        };
                        if let Ok(PageAnalysis::Direct(ref candidate)) = result
                            && candidate.group == CandidateGroup::Media
                            && ruscello_engine::is_manifest_url(&url)
                        {
                            let probe = add::inspect_media(engine, url.clone(), cancellation).await;
                            return (url, probe);
                        }
                        if result.as_ref().err().is_some_and(|error| {
                            matches!(
                                error,
                                ruscello_engine::EngineError::Resolver(
                                    ruscello_engine::ResolveError::Cancelled
                                )
                            )
                        }) {
                            return (url, add::Probe::Verification);
                        }
                        (url, add::probe_result(result))
                    },
                    |(url, result)| Message::Probe(url, result),
                );
            }
            Message::AutoAcceptHosters(enabled) => {
                self.preferences.auto_accept_hosters = enabled;
                return self.save_preferences();
            }
            Message::MonitorClipboard(enabled) => {
                self.preferences.monitor_clipboard = enabled;
                self.clipboard_seen = None;
                return self.save_preferences();
            }
            Message::ClipboardTick if self.preferences.monitor_clipboard => {
                return clipboard::read().map(Message::ClipboardMonitor);
            }
            Message::ClipboardMonitor(Some(value))
                if self.preferences.monitor_clipboard && value.len() <= 65536 =>
            {
                let previous = self.clipboard_seen.replace(value.clone());
                if previous.as_ref().is_some_and(|seen| seen != &value)
                    && !self.settings
                    && self.remove.is_none()
                    && self.add.is_none()
                {
                    let entries = ruscello_engine::links(&value)
                        .into_iter()
                        .map(|url| url.to_string())
                        .collect::<Vec<_>>();
                    if !entries.is_empty() {
                        return self.update(Message::Incoming(entries));
                    }
                }
            }
            Message::HosterDomain(host) if !self.hoster_draft.busy => {
                self.hoster_draft.host = host;
                self.hoster_draft.password.0.clear();
                self.hoster_draft.username.clear();
                self.hoster_draft.result = None;
            }
            Message::HosterUsername(username) => self.hoster_draft.username = username,
            Message::HosterPassword(password) => self.hoster_draft.password = password,
            Message::SaveHoster | Message::RemoveHoster if !self.hoster_draft.busy => {
                let save = matches!(message, Message::SaveHoster);
                let host = self.hoster_draft.host.clone();
                let credentials = ruscello_engine::HosterCredentials {
                    username: self.hoster_draft.username.clone(),
                    password: std::mem::take(&mut self.hoster_draft.password.0),
                };
                self.hoster_draft.busy = true;
                return Task::perform(
                    async move {
                        if save {
                            ruscello_engine::KeyringAccounts::set(host, credentials)
                                .await
                                .map(|_| "hoster-saved")
                        } else {
                            ruscello_engine::KeyringAccounts::remove(host)
                                .await
                                .map(|()| "hoster-removed")
                        }
                        .unwrap_or("hoster-keyring-error")
                    },
                    Message::HosterSaved,
                );
            }
            Message::HosterSaved(result) => {
                self.hoster_draft.busy = false;
                self.hoster_draft.result = Some(result);
            }
            Message::TorrentInspected(key, result) => {
                self.inspect_tokens.remove(&key);
                if let Some(dialog) = &mut self.add {
                    dialog.apply_torrent_inspection(key, result);
                }
            }
            Message::ToggleTorrentFile(key, index) => {
                if let Some(dialog) = &mut self.add {
                    dialog.toggle_torrent_file(&key, index);
                }
            }
            Message::ToggleAllTorrentFiles(key, select_all) => {
                if let Some(dialog) = &mut self.add {
                    dialog.toggle_all_torrent_files(&key, select_all);
                }
            }
            Message::InspectMedia(url) => {
                let Some(engine) = self.engine.clone() else {
                    return Task::none();
                };
                if self.probe_tokens.contains_key(&url) {
                    return Task::none();
                }
                let Some(dialog) = &mut self.add else {
                    return Task::none();
                };
                dialog.previews.insert(url.clone(), add::Probe::Pending);
                let cancellation = CancellationToken::new();
                self.probe_tokens.insert(url.clone(), cancellation.clone());
                self.in_flight += 1;
                return Task::perform(
                    async move {
                        let preview = add::inspect_media(engine, url.clone(), cancellation).await;
                        (url, preview)
                    },
                    |(url, preview)| Message::Probe(url, preview),
                );
            }
            Message::SelectMediaFormat(url, format) => {
                if let Some(dialog) = &mut self.add {
                    dialog.set_media_format(&url, format);
                }
            }
            Message::TogglePlaylistItem(url, index) => {
                if let Some(dialog) = &mut self.add {
                    dialog.toggle_playlist_item(&url, index);
                }
            }
            Message::ToggleAllPlaylistItems(url, select_all) => {
                if let Some(dialog) = &mut self.add {
                    dialog.toggle_all_playlist_items(&url, select_all);
                }
            }
            Message::SubmitAdd => {
                if let (Some(engine), Some(dialog)) = (self.engine.clone(), &mut self.add) {
                    if !dialog.can_submit() {
                        return Task::none();
                    }
                    let requests = dialog.requests(self.strings.get("imported-links"));
                    if !requests.is_empty() && !dialog.submitting {
                        dialog.submitting = true;
                        return Task::perform(
                            async move {
                                let result = engine.add_groups(requests).await;
                                result
                                    .inspect_err(|_| {
                                        tracing::debug!("Could not queue imported downloads")
                                    })
                                    .is_ok()
                            },
                            Message::Added,
                        );
                    }
                }
            }
            Message::Added(success) => {
                if let Some(dialog) = &mut self.add {
                    dialog.submitting = false;
                }
                let inputs = std::mem::take(&mut self.deferred_inputs);
                if success {
                    let close = self.interaction(Message::CloseAdd);
                    if inputs.is_empty() {
                        return close;
                    }
                    let open = self.update(Message::Incoming(inputs));
                    return Task::batch([close, open]);
                }
                self.banner = Some("add-failed");
                if !inputs.is_empty() {
                    return self.update(Message::Incoming(inputs));
                }
            }
            Message::Modifiers(modifiers) => self.modifiers = modifiers,
            Message::Shortcut(shortcut) => return self.shortcut(shortcut),
            Message::Menu(id) => {
                if !self.selection.contains(&id) {
                    self.selection.clear();
                    self.selection.insert(id);
                }
                self.queue.selected = Some(id);
                self.menu = (self.menu != Some(id)).then_some(id);
            }
            Message::RowAction(action) => return self.row_action(action),
            Message::Hover(id) => self.hovered = id,
            Message::SingleAction(id, action) => {
                self.selection.clear();
                self.selection.insert(id);
                self.queue.selected = Some(id);
                return self.row_action(action);
            }
            Message::DeleteFiles(delete) => {
                if let Some((_, choice)) = &mut self.remove {
                    *choice = delete;
                }
            }
            Message::ConfirmRemove => {
                if let Some((ids, delete)) = self.remove.take() {
                    return self.command(
                        ids.into_iter()
                            .map(|id| Command::Remove(id, delete))
                            .collect(),
                    );
                }
            }
            Message::CancelRemove => self.remove = None,
            Message::PauseAll => {
                let downloads = self.queue.downloads.values();
                let pause = self.can_pause_all();
                return self.command(
                    downloads
                        .filter_map(|download| {
                            if pause && actions::can_pause(download.status()) {
                                Some(Command::Pause(download.id))
                            } else if !pause && download.status() == Status::Paused {
                                Some(Command::Resume(download.id))
                            } else {
                                None
                            }
                        })
                        .collect(),
                );
            }
            Message::AlternateSpeed => {
                if !self.alternate_pending
                    && let Some(engine) = self.engine.clone()
                {
                    self.alternate_pending = true;
                    let enabled = !self.alternate_speed;
                    let rate = enabled.then_some(self.preferences.alternate_bandwidth);
                    return Task::perform(
                        async move { engine.set_temporary_bandwidth(rate).await.is_ok() },
                        move |success| Message::AlternateApplied(enabled, success),
                    );
                }
            }
            Message::AlternateApplied(enabled, success) => {
                self.alternate_pending = false;
                if success {
                    self.alternate_speed = enabled;
                } else {
                    self.banner = Some("action-failed");
                }
            }
            Message::ActionResult(false) => self.banner = Some("action-failed"),
            other => return self.update_settings(other),
        }
        Task::none()
    }

    fn update_settings(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SettingsPage(page) => {
                self.page = page;
                let already_open = self.settings;
                #[cfg(windows)]
                if already_open && page == Page::General {
                    return self.read_nvidia_profile();
                }
                let open = if already_open {
                    Task::none()
                } else {
                    self.update(Message::Settings(true))
                };
                if page == Page::Integrations {
                    return Task::batch([
                        open,
                        Task::perform(settings::lan_address(), Message::LanAddress),
                    ]);
                }
                return open;
            }
            Message::Renderer(renderer) => {
                self.preferences.renderer = renderer;
                return self.save_preferences();
            }
            #[cfg(windows)]
            Message::NvidiaProfile(action) => {
                use crate::nvidia::{Action, State};
                let allowed = match action {
                    Action::Read => true,
                    Action::Enable => {
                        self.nvidia_profile == State::Available
                            && self.preferences.renderer == Renderer::Hardware
                    }
                    Action::Restore => self.nvidia_profile == State::Managed,
                };
                if allowed && !self.nvidia_busy && !self.closing {
                    self.nvidia_busy = true;
                    self.nvidia_profile = State::Checking;
                    return Task::perform(
                        crate::nvidia::run(self.config.clone(), action),
                        Message::NvidiaProfileResult,
                    );
                }
            }
            #[cfg(windows)]
            Message::NvidiaProfileResult(outcome) => {
                self.nvidia_busy = false;
                self.nvidia_profile = outcome.state;
                self.nvidia_restart |= outcome.changed;
                if self.closing {
                    return self.finish_close();
                }
            }
            Message::SettingNumber(field, value) => {
                self.settings_draft.fields[field.index()] = value
            }
            Message::SaveLimits => {
                let Some((limits, alternate, port)) = self.settings_draft.parse(self.queue.limits)
                else {
                    self.banner = Some("invalid-settings");
                    return Task::none();
                };
                self.preferences.alternate_bandwidth = alternate;
                self.preferences.lan_port = port;
                let enabled = self.alternate_speed;
                let change = self.engine.clone().map_or_else(Task::none, |engine| {
                    Task::perform(
                        async move {
                            engine.set_limits(limits).await.is_ok()
                                && (!enabled
                                    || engine
                                        .set_temporary_bandwidth(Some(alternate))
                                        .await
                                        .is_ok())
                        },
                        Message::ActionResult,
                    )
                });
                return Task::batch([change, self.save_preferences(), self.sync_api()]);
            }
            Message::ChooseFolder(folder) => {
                return Task::perform(settings::pick_folder(), move |path| {
                    Message::SettingsFolder(folder, path)
                });
            }
            Message::SettingsFolder(folder, Some(path)) => {
                match folder {
                    Folder::Default => self.preferences.default_folder = Some(path),
                    Folder::Temporary => self.preferences.temporary_folder = Some(path),
                    Folder::Category(id) => {
                        let mut categories = self.queue.categories.clone();
                        if let Some(policy) = categories
                            .categories
                            .iter_mut()
                            .find(|policy| policy.category.id == id)
                        {
                            policy.category.destination = Some(path);
                            return self.save_categories(categories);
                        }
                        return Task::none();
                    }
                }
                return self.save_preferences();
            }
            Message::ClearTemporary => {
                self.preferences.temporary_folder = None;
                return self.save_preferences();
            }
            Message::LanEnabled(value) => {
                self.preferences.lan_enabled = value;
                return Task::batch([
                    self.save_preferences(),
                    self.sync_api(),
                    if value {
                        Task::perform(settings::lan_address(), Message::LanAddress)
                    } else {
                        Task::none()
                    },
                ]);
            }
            Message::LanAuth(value) => {
                self.preferences.lan_auth = value;
                return Task::batch([self.save_preferences(), self.sync_api()]);
            }
            Message::LanAddress(address) => self.lan_address = address,
            Message::LanStatusUpdated(Some(status)) => self.lan_status = status,
            Message::LanStatusUpdated(None) => {}
            Message::LanPassword(password) => self.lan_password = password,
            Message::SaveLanPassword => {
                if self.lan_password.0.is_empty() {
                    return Task::none();
                }
                let password = settings::LanPassword(std::mem::take(&mut self.lan_password.0));
                let data_dir = self.data_dir.clone();
                return Task::perform(
                    settings::save_lan_password(data_dir, password),
                    Message::LanPasswordSaved,
                );
            }
            Message::LanPasswordSaved(outcome) => {
                if let Some(hash) = outcome.hash {
                    self.lan_password_hash = Some(hash);
                }
                self.banner = Some(if outcome.keyring_saved {
                    "lan-password-saved"
                } else {
                    "lan-password-failed"
                });
                return self.sync_api();
            }
            Message::CopyLanUrl => {
                if let Some(url) = self.lan_url() {
                    self.banner = Some("lan-url-copied");
                    return clipboard::write(url);
                }
            }
            Message::OpenLanUrl => {
                if let Some(url) = self.lan_url() {
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || open::that_detached(url).is_ok())
                                .await
                                .unwrap_or(false)
                        },
                        Message::ActionResult,
                    );
                }
            }
            Message::StartPairing => {
                if let Some(control) = self.api_control.as_ref() {
                    match control.start_pairing_window() {
                        Ok(overview) => self.pairing = overview,
                        Err(_) => self.banner = Some("action-failed"),
                    }
                }
            }
            Message::CancelPairing => {
                if let Some(control) = self.api_control.as_ref() {
                    control.cancel_pairing_window();
                    self.pairing = control.pairing_status();
                }
            }
            Message::ApprovePairing(request_id) => {
                if let Some(control) = self.api_control.as_ref() {
                    match control.approve_pairing(&request_id) {
                        Ok(overview) => {
                            self.pairing = overview;
                            self.banner = Some("extension-pair-approved");
                        }
                        Err(_) => self.banner = Some("action-failed"),
                    }
                }
            }
            Message::RejectPairing(request_id) => {
                if let Some(control) = self.api_control.as_ref() {
                    match control.reject_pairing(&request_id) {
                        Ok(overview) => self.pairing = overview,
                        Err(_) => self.banner = Some("action-failed"),
                    }
                }
            }
            Message::RevokePairing => {
                if let Some(control) = self.api_control.as_ref() {
                    match control.revoke_extension_tokens() {
                        Ok(_) => {
                            self.pairing = control.pairing_status();
                            self.banner = Some("extension-pair-revoked");
                        }
                        Err(_) => self.banner = Some("action-failed"),
                    }
                }
            }
            Message::PairingTick => {
                if let Some(control) = self.api_control.as_ref() {
                    self.pairing = control.pairing_status();
                }
            }
            Message::CheckAutomatically(value) => {
                self.preferences.check_updates = value;
                return self.save_preferences();
            }
            Message::CheckUpdates if !matches!(self.update_status, UpdateStatus::Checking) => {
                self.update_status = UpdateStatus::Checking;
                return Task::perform(settings::check_updates(), Message::UpdateChecked);
            }
            Message::UpdateChecked(status) => self.update_status = status,
            Message::OpenRelease => {
                if let UpdateStatus::Available(release) = &self.update_status {
                    let url = release.html_url.clone();
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || open::that_detached(url).is_ok())
                                .await
                                .unwrap_or(false)
                        },
                        Message::ActionResult,
                    );
                }
            }
            Message::CopyDiagnostics => {
                return clipboard::write(settings::diagnostics(self.renderer));
            }
            Message::OpenLogs => {
                return Task::perform(settings::open_latest_log(), Message::ActionResult);
            }
            Message::TorrentOption(option, value) => {
                match option {
                    settings::TorrentOption::Dht => self.preferences.torrent.dht = value,
                    settings::TorrentOption::Pex => self.preferences.torrent.pex = value,
                    settings::TorrentOption::Upnp => self.preferences.torrent.upnp = value,
                    settings::TorrentOption::Lsd => self.preferences.torrent.lsd = value,
                }
                return self.save_preferences();
            }
            Message::TorrentField(field, value) => {
                match field {
                    settings::TorrentField::Port => self.preferences.torrent.port = value,
                    settings::TorrentField::DownloadLimit => {
                        self.preferences.torrent.download_limit = value
                    }
                    settings::TorrentField::UploadLimit => {
                        self.preferences.torrent.upload_limit = value
                    }
                    settings::TorrentField::Ratio => self.preferences.torrent.seed_ratio = value,
                    settings::TorrentField::Minutes => {
                        self.preferences.torrent.seed_minutes = value
                    }
                    settings::TorrentField::MaxPeers => self.preferences.torrent.max_peers = value,
                    settings::TorrentField::Interface => self.preferences.torrent.interface = value,
                    settings::TorrentField::Blocklist => self.preferences.torrent.blocklist = value,
                }
                return self.save_preferences();
            }
            Message::PickBlocklist => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::BlocklistPicked,
                );
            }
            Message::BlocklistPicked(Some(path)) => {
                self.preferences.torrent.blocklist = path.display().to_string();
                return self.save_preferences();
            }
            Message::SaveTorrentSettings => {
                let Some(settings) = self.preferences.torrent.to_settings() else {
                    self.banner = Some("invalid-settings");
                    return Task::none();
                };
                let change = self.engine.clone().map_or_else(Task::none, |engine| {
                    Task::perform(
                        async move {
                            if engine.set_torrent_settings(settings).await.is_ok() {
                                engine.torrent_telemetry().await.ok()
                            } else {
                                None
                            }
                        },
                        Message::TorrentSettingsApplied,
                    )
                });
                return Task::batch([change, self.save_preferences()]);
            }
            Message::TorrentSettingsApplied(Some(telemetry)) => {
                self.queue.torrent_telemetry(telemetry);
            }
            Message::TorrentSettingsApplied(None) => self.banner = Some("action-failed"),
            Message::TorrentDetails(id, details) => {
                self.details_in_flight = false;
                if self.queue.selected == Some(id) {
                    self.torrent_details = details.map(|snapshot| (id, snapshot));
                }
            }
            Message::SetTorrentFilePriority(id, file_index, include) => {
                let Some((details_id, snapshot)) = &self.torrent_details else {
                    return Task::none();
                };
                if *details_id != id {
                    return Task::none();
                }
                let only_files: Vec<usize> = snapshot
                    .files
                    .iter()
                    .filter_map(|file| {
                        let keep = if file.index == file_index {
                            include
                        } else {
                            file.priority == TorrentFilePriority::Download
                        };
                        keep.then_some(file.index)
                    })
                    .collect();
                if let Some(engine) = self.engine.clone() {
                    self.details_in_flight = true;
                    return Task::perform(
                        async move {
                            let ok = engine.set_torrent_files(id, only_files).await.is_ok();
                            let details = engine.torrent_details(id).await.ok().flatten();
                            (id, ok, details)
                        },
                        |(id, ok, details)| Message::TorrentFilePriorityUpdated(id, ok, details),
                    );
                }
            }
            Message::TorrentFilePriorityUpdated(id, ok, details) => {
                self.details_in_flight = false;
                if !ok {
                    self.banner = Some("action-failed");
                }
                if self.queue.selected == Some(id)
                    && let Some(snapshot) = details
                {
                    self.torrent_details = Some((id, snapshot));
                }
            }
            Message::CategoryEdit(field, value) => match field {
                settings::CategoryEdit::Name => self.category_draft.name = value,
                settings::CategoryEdit::Rule => self.category_draft.rule = value,
            },
            Message::CategoryTarget(id) => self.category_draft.category = Some(id),
            Message::CreateCategory => {
                let name = self.category_draft.name.trim();
                if name.is_empty() {
                    return Task::none();
                }
                let mut categories = self.queue.categories.clone();
                let ids = categories
                    .categories
                    .iter()
                    .map(|policy| policy.category.id)
                    .collect::<BTreeSet<_>>();
                let id = (1..).map(CategoryId).find(|id| !ids.contains(id));
                let Some(id) = id else {
                    return Task::none();
                };
                categories.categories.push(ruscello_engine::CategoryPolicy {
                    category: ruscello_core::Category {
                        id,
                        name: name.to_owned(),
                        destination: None,
                    },
                    limits: ruscello_engine::CategoryLimits::default(),
                });
                self.category_draft.name.clear();
                return self.save_categories(categories);
            }
            Message::AddCategoryRule => {
                let mut categories = self.queue.categories.clone();
                let Some(category) = self.category_draft.category else {
                    return Task::none();
                };
                let rule = self.category_draft.rule.trim();
                if rule.is_empty() {
                    return Task::none();
                }
                let matcher = match rule.split_once(':') {
                    Some(("domain", value)) => {
                        ruscello_engine::CategoryMatch::Domain(value.to_owned())
                    }
                    Some(("regex", value)) => {
                        ruscello_engine::CategoryMatch::Regex(value.to_owned())
                    }
                    Some(("extension", value)) => {
                        ruscello_engine::CategoryMatch::Extension(value.to_owned())
                    }
                    _ => ruscello_engine::CategoryMatch::Extension(rule.to_owned()),
                };
                categories
                    .rules
                    .push(ruscello_engine::CategoryRule { matcher, category });
                self.category_draft.rule.clear();
                return self.save_categories(categories);
            }
            Message::RemoveCategoryRule(index) => {
                let mut categories = self.queue.categories.clone();
                if index < categories.rules.len() {
                    categories.rules.remove(index);
                    return self.save_categories(categories);
                }
            }
            Message::SidecarConsent(kind, enabled) => {
                match kind {
                    SidecarKind::YtDlp => self.preferences.sidecars.ytdlp_consent = enabled,
                    SidecarKind::Ffmpeg => self.preferences.sidecars.ffmpeg_consent = enabled,
                }
                self.sidecar_draft.message = None;
                return self.apply_sidecar_settings();
            }
            Message::SidecarAutoUpdate(enabled) => {
                self.preferences.sidecars.ytdlp_auto_update = enabled;
                self.sidecar_draft.message = None;
                return self.apply_sidecar_settings();
            }
            Message::SidecarCustomPath(kind, value) => {
                self.sidecar_draft.set_path(kind, value);
                self.sidecar_draft.message = None;
            }
            Message::PickSidecarPath(kind) => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    move |path| Message::SidecarPathPicked(kind, path),
                );
            }
            Message::SidecarPathPicked(kind, Some(path)) => {
                self.sidecar_draft
                    .set_path(kind, path.display().to_string());
                self.sidecar_draft.message = Some("sidecar-paths-saved");
                return self.commit_sidecar_paths();
            }
            Message::SaveSidecarSettings => {
                self.sidecar_draft.message = Some("sidecar-paths-saved");
                return self.commit_sidecar_paths();
            }
            Message::SidecarStatusLoaded(status) => {
                if status.is_some() {
                    self.sidecar_draft.status = status;
                }
            }
            Message::InstallSidecar(kind) => {
                let consented = match kind {
                    SidecarKind::YtDlp => self.preferences.sidecars.ytdlp_consent,
                    SidecarKind::Ffmpeg => self.preferences.sidecars.ffmpeg_consent,
                };
                if !consented {
                    self.sidecar_draft.message = Some("sidecar-consent-error");
                    return Task::none();
                }
                if self.sidecar_draft.busy.is_some() {
                    return Task::none();
                }
                let Some(engine) = self.engine.clone() else {
                    return Task::none();
                };
                self.preferences.sidecars.ytdlp_custom_path =
                    settings::SidecarDraft::parse_custom_path(
                        &self.sidecar_draft.ytdlp_custom_path,
                    );
                self.preferences.sidecars.ffmpeg_custom_path =
                    settings::SidecarDraft::parse_custom_path(
                        &self.sidecar_draft.ffmpeg_custom_path,
                    );
                let sidecars = self.preferences.sidecars.clone();
                self.sidecar_draft.busy = Some(kind);
                self.sidecar_draft.message = None;
                return Task::perform(
                    async move {
                        if let Err(error) = engine.set_sidecar_settings(sidecars).await {
                            tracing::debug!(%error, "Could not update sidecar settings");
                            return Err("sidecar-install-failed");
                        }
                        if let Err(error) = engine
                            .install_sidecar(kind, &CancellationToken::new())
                            .await
                        {
                            tracing::debug!(%error, ?kind, "Sidecar installation failed");
                            return Err(match error {
                                ruscello_engine::EngineError::Media(
                                    MediaError::ConsentRequired(_),
                                ) => "sidecar-consent-error",
                                _ => "sidecar-install-failed",
                            });
                        }
                        engine
                            .sidecar_status()
                            .await
                            .map_err(|_| "sidecar-install-failed")
                    },
                    move |result| Message::SidecarInstallResult(kind, result),
                );
            }
            Message::SidecarInstallResult(_kind, result) => {
                self.sidecar_draft.busy = None;
                match result {
                    Ok(status) => {
                        self.sidecar_draft.status = Some(status);
                        self.sidecar_draft.message = None;
                    }
                    Err(key) => {
                        self.sidecar_draft.message = Some(key);
                    }
                }
            }
            Message::SidecarAutoUpdated(Some(status)) => {
                self.sidecar_draft.status = Some(status);
            }
            _ => {}
        }
        Task::none()
    }

    fn commit_sidecar_paths(&mut self) -> Task<Message> {
        self.preferences.sidecars.ytdlp_custom_path =
            settings::SidecarDraft::parse_custom_path(&self.sidecar_draft.ytdlp_custom_path);
        self.preferences.sidecars.ffmpeg_custom_path =
            settings::SidecarDraft::parse_custom_path(&self.sidecar_draft.ffmpeg_custom_path);
        self.apply_sidecar_settings()
    }

    fn apply_sidecar_settings(&mut self) -> Task<Message> {
        let settings = self.preferences.sidecars.clone();
        let sync = self.engine.clone().map_or_else(Task::none, |engine| {
            Task::perform(
                async move {
                    if let Err(error) = engine.set_sidecar_settings(settings).await {
                        tracing::debug!(%error, "Could not persist sidecar settings");
                        return None;
                    }
                    engine.sidecar_status().await.ok()
                },
                Message::SidecarStatusLoaded,
            )
        });
        Task::batch([sync, self.save_preferences()])
    }

    fn refresh_sidecar_status(&self) -> Task<Message> {
        self.engine.clone().map_or_else(Task::none, |engine| {
            Task::perform(
                async move { engine.sidecar_status().await.ok() },
                Message::SidecarStatusLoaded,
            )
        })
    }

    fn maybe_auto_update_ytdlp(&self) -> Task<Message> {
        if !self.preferences.sidecars.ytdlp_consent || !self.preferences.sidecars.ytdlp_auto_update
        {
            return Task::none();
        }
        self.engine.clone().map_or_else(Task::none, |engine| {
            Task::perform(
                async move {
                    let updated = engine
                        .maybe_auto_update_ytdlp(&CancellationToken::new())
                        .await
                        .ok()
                        .flatten();
                    if updated.is_some() {
                        engine.sidecar_status().await.ok()
                    } else {
                        None
                    }
                },
                Message::SidecarAutoUpdated,
            )
        })
    }

    fn save_categories(&self, categories: ruscello_engine::CategorySettings) -> Task<Message> {
        self.engine.clone().map_or_else(Task::none, |engine| {
            Task::perform(
                async move { engine.set_categories(categories).await.is_ok() },
                Message::ActionResult,
            )
        })
    }

    pub(crate) fn system_download_folder(&self) -> &std::path::Path {
        &self.default_destination
    }

    pub(crate) fn lan_url(&self) -> Option<String> {
        if let LanStatus::Active { display_url, .. } = &self.lan_status {
            return Some(display_url.clone());
        }
        self.lan_address
            .map(|ip| format!("http://{ip}:{}", self.preferences.lan_port))
    }

    fn start_api(&self) -> Task<Message> {
        let Some(engine) = self.engine.clone() else {
            return Task::none();
        };
        let data_dir = self.data_dir.clone();
        let config_lan_enabled = self.preferences.lan_enabled;
        let lan_port = self.preferences.lan_port.get();
        let lan_auth_required = self.preferences.lan_auth;
        let alternate_bandwidth = self.preferences.alternate_bandwidth;
        let locale = self.preferences.locale.language();
        let allow_dlc_fallback = self.preferences.dlc_fallback;
        Task::perform(
            async move {
                let password_hash = settings::load_lan_password_hash(data_dir.clone()).await;
                let config = ApiConfig {
                    data_dir,
                    local_port: DEFAULT_LOCAL_PORT,
                    fallback_to_ephemeral_local_port: true,
                    cli_token: None,
                    extension_token_hashes: Vec::new(),
                    lan_enabled: config_lan_enabled,
                    lan_port,
                    lan_auth_required,
                    lan_password_hash: password_hash.clone(),
                    alternate_bandwidth,
                    locale,
                    allow_dlc_fallback,
                };
                match ApiServer::start(engine, config).await {
                    Ok(handle) => {
                        let controller = handle.controller();
                        let lan_status = handle.lan_status();
                        let pairing = handle.pairing_status();
                        Some(Box::new(ApiStartup {
                            handle: Arc::new(tokio::sync::Mutex::new(Some(handle))),
                            controller,
                            lan_status,
                            pairing,
                            password_hash,
                        }))
                    }
                    Err(error) => {
                        tracing::debug!(%error, "Could not start the local API server");
                        None
                    }
                }
            },
            Message::ApiStarted,
        )
    }

    fn sync_api(&self) -> Task<Message> {
        let Some(api) = self.api.clone() else {
            return Task::none();
        };
        let update = ApiRuntimeUpdate {
            lan_enabled: self.preferences.lan_enabled,
            lan_port: self.preferences.lan_port.get(),
            lan_auth_required: self.preferences.lan_auth,
            lan_password_hash: self.lan_password_hash.clone(),
            alternate_bandwidth: self.preferences.alternate_bandwidth,
            locale: self.preferences.locale.language(),
        };
        Task::perform(
            async move {
                let mut guard = api.lock().await;
                if let Some(handle) = guard.as_mut() {
                    Some(handle.apply_update(update).await)
                } else {
                    None
                }
            },
            Message::LanStatusUpdated,
        )
    }

    fn refresh_add(&mut self) -> Task<Message> {
        let Some(dialog) = &mut self.add else {
            return Task::none();
        };
        let (urls, torrents) = dialog.refresh();
        let active_torrents: HashSet<String> = dialog.torrent_previews.keys().cloned().collect();
        self.inspect_tokens.retain(|key, token| {
            let keep = active_torrents.contains(key);
            if !keep {
                token.cancel();
            }
            keep
        });
        let paths = dialog.new_containers();
        for (url, token) in &self.probe_tokens {
            if !dialog.previews.contains_key(url) {
                token.cancel();
            }
        }
        self.pending_probes
            .retain(|url| dialog.previews.contains_key(url));
        self.pending_probes.extend(urls);
        let fallback = self.preferences.dlc_fallback;
        let import = Task::batch(paths.into_iter().map(|path| {
            Task::perform(
                add::import(self.engine.clone(), path, fallback),
                |(path, result)| Message::Imported(path, result),
            )
        }));
        Task::batch([
            self.run_probes(),
            self.run_torrent_inspections(torrents),
            import,
        ])
    }

    fn run_torrent_inspections(&mut self, keys: Vec<String>) -> Task<Message> {
        let mut tasks = Vec::new();
        for key in keys {
            if self.inspect_tokens.contains_key(&key) {
                continue;
            }
            let source = add::torrent_source(&key);
            let token = CancellationToken::new();
            self.inspect_tokens.insert(key.clone(), token.clone());
            let engine = self.engine.clone();
            tasks.push(Task::perform(
                async move {
                    let result = if let Some(engine) = engine {
                        engine
                            .inspect_torrent(source, Vec::new(), token)
                            .await
                            .map_err(|error| error.to_string())
                    } else if let TorrentSource::File(path) = source {
                        tokio::task::spawn_blocking(move || {
                            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
                            ruscello_engine::inspect_torrent_bytes(&bytes)
                                .map_err(|error| error.to_string())
                        })
                        .await
                        .unwrap_or_else(|error| Err(error.to_string()))
                    } else {
                        Err("Engine not started".to_owned())
                    };
                    (key, result)
                },
                |(key, result)| Message::TorrentInspected(key, result),
            ));
        }
        Task::batch(tasks)
    }

    fn selected_is_torrent(&self) -> bool {
        self.queue
            .selected
            .and_then(|id| self.queue.downloads.get(&id))
            .is_some_and(|download| matches!(download.source, Source::Torrent(_)))
    }

    fn refresh_torrent_details(&mut self) -> Task<Message> {
        if self.details_in_flight {
            return Task::none();
        }
        let (Some(engine), Some(id)) = (self.engine.clone(), self.queue.selected) else {
            return Task::none();
        };
        if !self
            .queue
            .downloads
            .get(&id)
            .is_some_and(|download| matches!(download.source, Source::Torrent(_)))
        {
            return Task::none();
        }
        self.details_in_flight = true;
        Task::perform(
            async move { (id, engine.torrent_details(id).await.ok().flatten()) },
            |(id, details)| Message::TorrentDetails(id, details),
        )
    }

    fn run_probes(&mut self) -> Task<Message> {
        let Some(engine) = self.engine.clone() else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        while self.in_flight < 4 {
            let Some(url) = self.pending_probes.pop_front() else {
                break;
            };
            if self.probe_tokens.contains_key(&url) {
                self.pending_probes.push_back(url);
                break;
            }
            self.in_flight += 1;
            let cancellation = CancellationToken::new();
            self.probe_tokens.insert(url.clone(), cancellation.clone());
            let engine = engine.clone();
            tasks.push(Task::perform(
                async move {
                    let preview = add::probe(engine, url.clone(), cancellation).await;
                    (url, preview)
                },
                |(url, preview)| Message::Probe(url, preview),
            ));
        }
        Task::batch(tasks)
    }

    fn auto_accept_hosters(&mut self) -> Task<Message> {
        if self.preferences.auto_accept_hosters
            && self.add.as_ref().is_some_and(|dialog| {
                dialog.can_submit()
                    && !dialog.paused
                    && !dialog.entries.is_empty()
                    && dialog.entries.iter().all(|entry| {
                        matches!(entry, Input::Http(url)
                        if matches!(dialog.previews.get(url), Some(add::Probe::Hoster(_))))
                    })
            })
        {
            return self.interaction(Message::SubmitAdd);
        }
        Task::none()
    }

    fn select(&mut self, id: DownloadId) {
        self.menu = None;
        if self.modifiers.shift()
            && let Some(anchor) = self.queue.selected
        {
            let range = self
                .queue
                .rows
                .iter()
                .filter_map(|row| match row {
                    queue::ListRow::Download(id) => Some(*id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if let (Some(first), Some(last)) = (
                range.iter().position(|item| *item == anchor),
                range.iter().position(|item| *item == id),
            ) {
                if !self.modifiers.command() && !self.modifiers.control() {
                    self.selection.clear();
                }
                self.selection
                    .extend(range[first.min(last)..=first.max(last)].iter().copied());
            }
        } else if self.modifiers.command() || self.modifiers.control() {
            if !self.selection.remove(&id) {
                self.selection.insert(id);
            }
        } else {
            self.selection.clear();
            self.selection.insert(id);
        }
        self.queue.selected = self
            .selection
            .contains(&id)
            .then_some(id)
            .or_else(|| self.selection.iter().next().copied());
        if self
            .torrent_details
            .as_ref()
            .is_some_and(|(current, _)| self.queue.selected != Some(*current))
        {
            self.torrent_details = None;
            self.details_in_flight = false;
        }
        if !self.detail_tab.allowed(self.selected_is_torrent()) {
            self.detail_tab = DetailTab::General;
        }
    }

    fn shortcut(&mut self, shortcut: Shortcut) -> Task<Message> {
        match shortcut {
            Shortcut::Escape if self.remove.is_some() => self.interaction(Message::CancelRemove),
            Shortcut::Escape if self.add.is_some() => self.interaction(Message::CloseAdd),
            Shortcut::Escape if self.menu.take().is_some() => Task::none(),
            Shortcut::Escape if self.settings => self.update(Message::Settings(false)),
            Shortcut::Add if self.remove.is_none() => self.interaction(Message::OpenAdd),
            Shortcut::Paste if !self.settings && self.add.is_none() && self.remove.is_none() => {
                self.interaction(Message::OpenAdd)
            }
            Shortcut::Search if !self.settings && self.add.is_none() && self.remove.is_none() => {
                operation::focus(view::search_id())
            }
            Shortcut::Settings if self.add.is_none() && self.remove.is_none() => {
                self.update(Message::Settings(true))
            }
            Shortcut::Action(action)
                if !self.settings && self.add.is_none() && self.remove.is_none() =>
            {
                self.row_action(action)
            }
            Shortcut::All if !self.settings && self.add.is_none() && self.remove.is_none() => {
                self.selection = self
                    .queue
                    .rows
                    .iter()
                    .filter_map(|row| match row {
                        queue::ListRow::Download(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                self.queue.selected = self.selection.iter().next().copied();
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn row_action(&mut self, action: Action) -> Task<Message> {
        self.menu = None;
        if self.selection.is_empty() {
            return Task::none();
        }
        let selected = self
            .selection
            .iter()
            .filter_map(|id| self.queue.downloads.get(id));
        match action {
            Action::Verify => {
                if let Some(id) = selected
                    .filter(|download| download.status() == Status::WaitingUser)
                    .map(|download| download.id)
                    .next()
                {
                    return self.update(Message::OpenVerification(id));
                }
            }
            Action::Cancel => self.remove = Some((self.selection.iter().copied().collect(), false)),
            Action::OpenFile | Action::OpenFolder => {
                let paths: BTreeSet<_> = selected
                    .filter_map(|download| match action {
                        Action::OpenFile => download.completed_path.clone(),
                        _ => Some(download.destination.clone()),
                    })
                    .collect();
                return Task::perform(
                    actions::open(paths.into_iter().collect()),
                    Message::ActionResult,
                );
            }
            Action::CopyLink => {
                let contents = selected
                    .map(|download| queue::source_text(&download.source))
                    .collect::<Vec<_>>()
                    .join("\n");
                return clipboard::write(contents);
            }
            Action::TogglePause => {
                return self.command(
                    selected
                        .filter_map(|download| {
                            if actions::can_pause(download.status()) {
                                Some(Command::Pause(download.id))
                            } else if actions::can_resume(download.status()) {
                                Some(Command::Resume(download.id))
                            } else {
                                None
                            }
                        })
                        .collect(),
                );
            }
            Action::ForceRecheck => {
                return self.command(
                    selected
                        .filter(|download| actions::can_recheck(download))
                        .map(|download| Command::ForceRecheck(download.id))
                        .collect(),
                );
            }
            Action::Priority(priority) => {
                return self.command(
                    selected
                        .map(|download| Command::Priority(download.id, priority))
                        .collect(),
                );
            }
            Action::MoveUp | Action::MoveDown => {
                let mut order = self.queue.order.clone();
                let mut commands = Vec::new();
                for offset in 0..order.len().saturating_sub(1) {
                    let index = if matches!(action, Action::MoveUp) {
                        offset
                    } else {
                        order.len() - 2 - offset
                    };
                    let (left, right) = (order[index], order[index + 1]);
                    match (
                        self.selection.contains(&left),
                        self.selection.contains(&right),
                        action,
                    ) {
                        (false, true, Action::MoveUp) => {
                            commands.push(Command::Move(right, Some(left)))
                        }
                        (true, false, Action::MoveDown) => {
                            commands.push(Command::Move(left, order.get(index + 2).copied()))
                        }
                        _ => continue,
                    }
                    order.swap(index, index + 1);
                }
                return self.command(commands);
            }
        }
        Task::none()
    }

    fn browser_locale(&self) -> ruscello_engine::BrowserLocale {
        match self.preferences.locale.language() {
            ruscello_platform::language::Language::English => ruscello_engine::BrowserLocale::En,
            ruscello_platform::language::Language::Italian => ruscello_engine::BrowserLocale::It,
        }
    }

    fn command(&self, commands: Vec<Command>) -> Task<Message> {
        if let Some(engine) = self.engine.clone()
            && !commands.is_empty()
        {
            Task::perform(actions::run(engine, commands), Message::ActionResult)
        } else {
            Task::none()
        }
    }

    pub(crate) fn alternate_speed(&self) -> bool {
        self.alternate_speed
    }

    pub(crate) fn can_pause_all(&self) -> bool {
        self.queue
            .downloads
            .values()
            .any(|download| actions::can_pause(download.status()))
    }

    pub(crate) fn can_resume_all(&self) -> bool {
        self.queue
            .downloads
            .values()
            .any(|download| download.status() == Status::Paused)
    }

    fn finish_close(&mut self) -> Task<Message> {
        #[cfg(windows)]
        if self.nvidia_busy {
            return Task::none();
        }
        if self.booting || self.saving {
            return Task::none();
        }
        if let Some(engine) = self.engine.take() {
            let api = self.api.take();
            self.api_control = None;
            return Task::perform(
                async move {
                    if let Some(api) = api
                        && let Some(handle) = api.lock().await.take()
                    {
                        handle.shutdown().await;
                    }
                    engine
                        .shutdown()
                        .await
                        .inspect_err(
                            |error| tracing::debug!(%error, "Could not stop the queue cleanly"),
                        )
                        .is_ok()
                },
                Message::Stopped,
            );
        }
        iced::exit()
    }

    fn quit(&mut self) -> Task<Message> {
        if self.closing {
            return Task::none();
        }
        self.closing = true;
        self.finish_close()
    }

    fn show_window(&self) -> Task<Message> {
        window::set_mode(self.window, window::Mode::Windowed)
            .chain(window::minimize(self.window, false))
            .chain(window::gain_focus(self.window))
    }

    fn check_disk(&self) -> Task<Message> {
        Task::perform(
            queue::disk_space(self.destination.clone()),
            Message::DiskSpace,
        )
    }

    fn filter_changed(&mut self) -> Task<Message> {
        self.queue.rebuild();
        self.reset_scroll()
    }

    fn reset_scroll(&mut self) -> Task<Message> {
        self.offset = 0.0;
        operation::scroll_to(view::queue_id(), AbsoluteOffset { x: 0.0, y: 0.0 })
    }

    #[cfg(windows)]
    fn read_nvidia_profile(&mut self) -> Task<Message> {
        self.update_settings(Message::NvidiaProfile(crate::nvidia::Action::Read))
    }

    fn save_preferences(&mut self) -> Task<Message> {
        if self.saving {
            self.dirty = true;
            return Task::none();
        }
        self.saving = true;
        self.dirty = false;
        let preferences = self.preferences.clone();
        let path = self.config.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || preferences.save(&path)
                .inspect_err(|error| tracing::debug!(kind = ?error.kind(), "Could not save desktop preferences"))
                .is_ok()).await.unwrap_or(false)
            },
            Message::Saved,
        )
    }

    pub(crate) fn view(&self, _: window::Id) -> Element<'_, Message> {
        view::main(self)
    }
}

fn view_animation(initial: bool) -> Animation<bool> {
    Animation::new(initial)
        .duration(MOTION_DURATION)
        .easing(iced::animation::Easing::EaseOutCubic)
}

fn i18n_status_key(status: Status) -> &'static str {
    crate::i18n::status(status)
}

fn browser_error(error: &ruscello_engine::EngineError) -> Option<&'static str> {
    use ruscello_engine::{BrowserFailure, EngineError, ResolveError};
    match error {
        EngineError::Resolver(ResolveError::Cancelled) | EngineError::VerificationNotPending => {
            None
        }
        EngineError::Resolver(ResolveError::Browser(BrowserFailure::ProfileInUse)) => {
            Some("browser-profile-busy")
        }
        EngineError::Resolver(ResolveError::Browser(BrowserFailure::Timeout)) => {
            Some("browser-timeout")
        }
        _ => Some("browser-unavailable"),
    }
}

struct DesktopSubscription<T>(window::Id, desktop::Receiver<T>);

impl<T> Hash for DesktopSubscription<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

fn tray_events(
    source: &DesktopSubscription<desktop::Event>,
) -> impl Stream<Item = Message> + use<> {
    use iced::futures::StreamExt;
    desktop::events(&source.1).map(Message::Tray)
}

fn ipc_events(source: &DesktopSubscription<Vec<String>>) -> impl Stream<Item = Message> + use<> {
    use iced::futures::StreamExt;
    desktop::events(&source.1).map(Message::Incoming)
}

struct ApiSubscription {
    window: window::Id,
    control: ApiController,
}

impl Hash for ApiSubscription {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.window.hash(state);
    }
}

fn api_events(source: &ApiSubscription) -> impl Stream<Item = Message> + use<> {
    let receiver = source.control.subscribe();
    stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(event) => return Some((Message::Api(event), receiver)),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
}

struct QueueSubscription {
    window: window::Id,
    engine: EngineHandle,
}

impl Hash for QueueSubscription {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.window.hash(state);
    }
}

fn queue_events(source: &QueueSubscription) -> impl Stream<Item = Message> + use<> {
    let engine = source.engine.clone();
    let receiver = engine.subscribe();
    stream::unfold(
        (engine, receiver, true),
        |(engine, mut receiver, refresh)| async move {
            if refresh {
                let snapshot = engine
                    .snapshot()
                    .await
                    .inspect_err(|error| tracing::debug!(%error, "Could not refresh the queue"))
                    .ok();
                return Some((Message::Snapshot(snapshot), (engine, receiver, false)));
            }
            match receiver.recv().await {
                Ok(event) => {
                    let refresh = matches!(
                        event,
                        EngineEvent::Added(_)
                            | EngineEvent::Removed(_)
                            | EngineEvent::StatusChanged { .. }
                            | EngineEvent::QueueChanged
                    );
                    Some((Message::Event(event), (engine, receiver, refresh)))
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    Some((Message::Noop, (engine, receiver, true)))
                }
                Err(broadcast::error::RecvError::Closed) => None,
            }
        },
    )
}
