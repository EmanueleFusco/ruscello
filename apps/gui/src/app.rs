use std::{
    hash::{Hash, Hasher},
    path::PathBuf,
    time::Instant,
};

use iced::{
    Element, Size, Subscription, Task,
    futures::{Stream, stream},
    widget::{operation, scrollable::AbsoluteOffset},
    window,
};
use ruscello_core::{CategoryId, DownloadId, FileType, PackageId};
use ruscello_engine::{EngineEvent, EngineHandle, EngineOptions, EngineSnapshot};
use tokio::sync::broadcast;

use crate::{
    i18n::Strings,
    preferences::{Appearance, Locale, Preferences},
    queue::{self, Queue, StatusFilter},
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
    Scrolled { offset: f32, height: f32 },
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
}

impl App {
    pub(crate) fn boot(
        preferences: Preferences,
        config: PathBuf,
        database: PathBuf,
        destination: PathBuf,
        restore_failed: bool,
        strings: Strings,
    ) -> (Self, Task<Message>) {
        let (window, open) = window::open(window::Settings {
            size: Size::new(1120.0, 720.0),
            min_size: Some(Size::new(950.0, 490.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        });
        let options = EngineOptions::new(database, destination.clone());
        let app = Self {
            window,
            engine: None,
            queue: Queue::default(),
            strings,
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
                open.map(|_| Message::Noop),
                start,
                iced::system::theme().map(Message::SystemTheme),
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
                    self.check_disk()
                };
            }
            Message::Snapshot(snapshot) => {
                self.booting = false;
                if let Some(snapshot) = snapshot {
                    self.queue.replace(snapshot);
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
            Message::Event(EngineEvent::StatusChanged { .. } | EngineEvent::Removed(_)) => {
                return self.check_disk();
            }
            Message::Event(_) => {}
            Message::DiskSpace(bytes) => self.disk_space = bytes,
            Message::Close => {
                if self.closing {
                    return Task::none();
                }
                self.closing = true;
                return self.finish_close();
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
            Message::Select(id) => self.queue.selected = Some(id),
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
                return self.reset_scroll();
            }
            Message::Locale(locale) => {
                self.preferences.locale = locale;
                self.strings.set_locale(locale);
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
        }
        Task::none()
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
        let preferences = self.preferences;
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
