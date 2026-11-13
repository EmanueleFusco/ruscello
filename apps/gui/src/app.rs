use std::{
    collections::{BTreeSet, VecDeque},
    hash::{Hash, Hasher},
    path::PathBuf,
    time::Instant,
};

use iced::{
    Element, Size, Subscription, Task, clipboard, event,
    futures::{Stream, stream},
    keyboard,
    widget::{operation, scrollable::AbsoluteOffset, text_editor},
    window,
};
use ruscello_core::{CategoryId, DownloadId, FileType, PackageId, Status};
use ruscello_engine::{EngineEvent, EngineHandle, EngineOptions, EngineSnapshot, Url};
use tokio::sync::broadcast;

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
    Log,
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
            Self::Size => 80,
            Self::Progress => 122,
            Self::Speed => 95,
            Self::Eta => 100,
            Self::Status => 130,
        }
    }

    pub(crate) fn mask(self) -> u8 {
        1 << self as u8
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Noop,
    Started(Option<EngineHandle>),
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
    SettingNumber(Number, String),
    SaveLimits,
    ChooseFolder(Folder),
    SettingsFolder(Folder, Option<PathBuf>),
    ClearTemporary,
    LanEnabled(bool),
    LanAuth(bool),
    LanAddress(Option<std::net::IpAddr>),
    LanPassword(settings::LanPassword),
    SaveLanPassword,
    LanPasswordSaved(bool),
    CopyLanUrl,
    CheckAutomatically(bool),
    CheckUpdates,
    UpdateChecked(UpdateStatus),
    OpenRelease,
    CopyDiagnostics,
    OpenLogs,
    TorrentOption(settings::TorrentOption, bool),
    TorrentField(settings::TorrentField, String),
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
}

pub(crate) struct App {
    window: window::Id,
    engine: Option<EngineHandle>,
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
    in_flight: usize,
    pub(crate) selection: BTreeSet<DownloadId>,
    modifiers: keyboard::Modifiers,
    pub(crate) menu: Option<DownloadId>,
    pub(crate) hovered: Option<DownloadId>,
    pub(crate) remove: Option<(Vec<DownloadId>, bool)>,
    alternate_speed: bool,
    alternate_pending: bool,
    pub(crate) page: Page,
    pub(crate) settings_draft: Draft,
    pub(crate) renderer: Renderer,
    pub(crate) lan_address: Option<std::net::IpAddr>,
    pub(crate) lan_password: settings::LanPassword,
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
        let app = Self {
            window,
            engine: None,
            queue: Queue::default(),
            strings,
            renderer: preferences.renderer,
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
            in_flight: 0,
            selection: BTreeSet::new(),
            modifiers: keyboard::Modifiers::default(),
            menu: None,
            hovered: None,
            remove: None,
            alternate_speed: false,
            alternate_pending: false,
            page: Page::General,
            settings_draft: Draft::default(),
            lan_address: None,
            lan_password: settings::LanPassword(String::new()),
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
        Subscription::batch([
            engine,
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
        match message {
            Message::Noop => {}
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
                    Task::batch([self.check_disk(), check])
                };
            }
            Message::Snapshot(snapshot) => {
                self.booting = false;
                if let Some(snapshot) = snapshot {
                    self.queue.replace(snapshot);
                    self.selection
                        .retain(|id| self.queue.downloads.contains_key(id));
                    if self.closing {
                        return self.finish_close();
                    }
                    if self.offset >= self.queue.rows.len() as f32 * queue::ROW_HEIGHT {
                        return self.reset_scroll();
                    }
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
            Message::Event(EngineEvent::Log {
                id: Some(id),
                message,
            }) => self.queue.log(id, message),
            Message::Event(EngineEvent::Log { id: None, message }) => {
                self.banner = Some(view::log_key(message))
            }
            Message::Event(EngineEvent::StatusChanged { id, status }) => {
                let notice = if self.preferences.notifications
                    && matches!(
                        status,
                        Status::Completed | Status::Failed { .. } | Status::WaitingUser
                    ) {
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
            Message::Select(id) => self.select(id),
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
                return self.save_preferences();
            }
            Message::DetailTab(tab) => self.detail_tab = tab,
            Message::Settings(open) => {
                self.settings = open;
                if open {
                    self.settings_draft = Draft::new(
                        self.queue.limits,
                        self.preferences.alternate_bandwidth,
                        self.preferences.lan_port,
                    );
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
                return self.save_preferences();
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
                self.add = None;
                self.editor = text_editor::Content::new();
                self.pending_probes.clear();
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
                self.settings = false;
                self.add.get_or_insert_with(Dialog::default);
                return Task::perform(add::load_files(paths), Message::FilesLoaded);
            }
            Message::FilesLoaded(inputs) => {
                if let Some(dialog) = &mut self.add {
                    dialog.files.extend(inputs);
                    return self.refresh_add();
                }
            }
            Message::PickFiles => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .add_filter(
                                "Torrent / DLC / TXT / URL",
                                &["torrent", "dlc", "txt", "url"],
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
                if let Some(dialog) = &mut self.add
                    && dialog.previews.contains_key(&url)
                {
                    dialog.previews.insert(url, preview);
                }
                return self.run_probes();
            }
            Message::SubmitAdd => {
                if let (Some(engine), Some(dialog)) = (self.engine.clone(), &mut self.add) {
                    if !dialog.can_submit() {
                        return Task::none();
                    }
                    let requests = dialog.requests();
                    if !requests.is_empty() && !dialog.submitting {
                        dialog.submitting = true;
                        let package = self.strings.get("imported-links");
                        return Task::perform(
                            async move {
                                let result = if requests.len() > 1 {
                                    engine.add_package(package, requests).await
                                } else {
                                    engine.add_many(requests).await
                                };
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
                if success {
                    return self.interaction(Message::CloseAdd);
                }
                if let Some(dialog) = &mut self.add {
                    dialog.submitting = false;
                }
                self.banner = Some("add-failed");
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
                if !self.settings {
                    let _ = self.update(Message::Settings(true));
                }
                self.page = page;
                if page == Page::Integrations {
                    return Task::perform(settings::lan_address(), Message::LanAddress);
                }
            }
            Message::Renderer(renderer) => {
                self.preferences.renderer = renderer;
                return self.save_preferences();
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
                return Task::batch([change, self.save_preferences()]);
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
                return self.save_preferences();
            }
            Message::LanAuth(value) => {
                self.preferences.lan_auth = value;
                return self.save_preferences();
            }
            Message::LanAddress(address) => self.lan_address = address,
            Message::LanPassword(password) => self.lan_password = password,
            Message::SaveLanPassword => {
                if self.lan_password.0.is_empty() {
                    return Task::none();
                }
                let password = settings::LanPassword(std::mem::take(&mut self.lan_password.0));
                return Task::perform(
                    settings::save_lan_password(password),
                    Message::LanPasswordSaved,
                );
            }
            Message::LanPasswordSaved(saved) => {
                self.banner = Some(if saved {
                    "lan-password-saved"
                } else {
                    "lan-password-failed"
                });
            }
            Message::CopyLanUrl => {
                if let Some(url) = self.lan_url() {
                    return clipboard::write(url);
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
                return Task::perform(
                    async {
                        tokio::task::spawn_blocking(|| {
                            ruscello_platform::paths::AppPaths::discover()
                                .ok()
                                .is_some_and(|paths| {
                                    open::that_detached(paths.logs().join("ruscello")).is_ok()
                                })
                        })
                        .await
                        .unwrap_or(false)
                    },
                    Message::ActionResult,
                );
            }
            Message::TorrentOption(option, value) => {
                match option {
                    settings::TorrentOption::Dht => self.preferences.torrent.dht = value,
                    settings::TorrentOption::Pex => self.preferences.torrent.pex = value,
                    settings::TorrentOption::Upnp => self.preferences.torrent.upnp = value,
                }
                return self.save_preferences();
            }
            Message::TorrentField(field, value) => {
                match field {
                    settings::TorrentField::Port => self.preferences.torrent.port = value,
                    settings::TorrentField::Ratio => self.preferences.torrent.seed_ratio = value,
                    settings::TorrentField::Minutes => {
                        self.preferences.torrent.seed_minutes = value
                    }
                    settings::TorrentField::Interface => self.preferences.torrent.interface = value,
                }
                return self.save_preferences();
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
            _ => {}
        }
        Task::none()
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
        self.lan_address
            .map(|ip| format!("http://{ip}:{}", self.preferences.lan_port))
    }

    fn refresh_add(&mut self) -> Task<Message> {
        let Some(dialog) = &mut self.add else {
            return Task::none();
        };
        let urls = dialog.refresh();
        let paths = dialog.new_text_lists();
        self.pending_probes
            .retain(|url| dialog.previews.contains_key(url));
        self.pending_probes.extend(urls);
        let import = if paths.is_empty() {
            Task::none()
        } else {
            Task::perform(add::load_files(paths), Message::FilesLoaded)
        };
        Task::batch([self.run_probes(), import])
    }

    fn run_probes(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        while self.in_flight < 4 {
            let Some(url) = self.pending_probes.pop_front() else {
                break;
            };
            self.in_flight += 1;
            tasks.push(Task::perform(
                async move {
                    let preview = add::probe(url.clone()).await;
                    (url, preview)
                },
                |(url, preview)| Message::Probe(url, preview),
            ));
        }
        Task::batch(tasks)
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

    fn finish_close(&mut self) -> Task<Message> {
        if self.booting || self.saving {
            return Task::none();
        }
        if let Some(engine) = self.engine.take() {
            return Task::perform(
                async move {
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

fn i18n_status_key(status: Status) -> &'static str {
    crate::i18n::status(status)
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
