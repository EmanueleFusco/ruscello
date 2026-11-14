use iced::{
    Alignment, Element, Fill, Padding, Theme,
    widget::{
        self, button, column, container, mouse_area, progress_bar, row, rule, space, text,
        text_editor, tooltip,
    },
};
use ruscello_core::{Download, Source, Status, TorrentSource};
use ruscello_engine::{
    CandidateGroup, EngineLog, PieceBucketState, ScoreReason, TorrentDetailsSnapshot,
    TorrentFilePriority, TorrentPeerState, TorrentPeerTransport, TorrentPieceMap,
    TorrentTrackerStatus,
};

use crate::{
    actions::Action,
    add::{Input, Probe, TorrentPreview},
    app::{App, Column, DetailTab, Message},
    i18n::{self, Strings},
    icons::{self, icon},
    queue::{self, FILE_TYPES, ListRow, ROW_HEIGHT, StatusFilter},
    settings::{Page, UpdateStatus},
    settings_view, theme,
};

const TOOLBAR_BUTTON_SIZE: u32 = 36;
const ROW_ACTION_WIDTH: u32 = 32;
const ROW_ACTIONS_WIDTH: u32 = ROW_ACTION_WIDTH * 3;
const TABLE_CELL_GAP: u32 = 6;
const TABLE_RIGHT_PADDING: u32 = 12;

pub(crate) fn queue_id() -> widget::Id {
    widget::Id::new("download-list")
}

pub(crate) fn add_id() -> widget::Id {
    widget::Id::new("add-download-input")
}

pub(crate) fn search_id() -> widget::Id {
    widget::Id::new("search-downloads")
}

pub(crate) fn main(app: &App) -> Element<'_, Message> {
    let content = if let Some(dialog) = &app.add {
        add_dialog(app, dialog)
    } else if let Some((ids, delete)) = &app.remove {
        remove_dialog(app, ids.len(), *delete)
    } else if app.settings {
        settings_view::view(app)
    } else {
        column![
            toolbar(app),
            row![sidebar(app), center(app)].spacing(14).height(Fill),
            status_bar(app)
        ]
        .spacing(12)
        .into()
    };
    let mut body = column![content].spacing(8);
    if let Some(message) = app.banner {
        body = body.push(
            container(
                row![
                    text(app.strings.get(message)).width(Fill),
                    icon_button('\u{e1b2}', app.strings.get("dismiss"), Message::Dismiss)
                ]
                .align_y(Alignment::Center),
            )
            .padding([6, 10])
            .style(theme::panel),
        );
    }
    container(body).padding(14).width(Fill).height(Fill).into()
}

fn toolbar(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let mut toolbar = row![
        button(
            row![icon('\u{e13d}'), text(l.get("add"))]
                .align_y(Alignment::Center)
                .spacing(6)
        )
        .padding([8, 14])
        .style(theme::primary)
        .on_press(Message::OpenAdd),
        row![
            icon('\u{e151}').style(muted),
            theme::field(&l.get("search"), &app.queue.search)
                .on_input(Message::Search)
                .id(search_id())
                .width(Fill)
        ]
        .align_y(Alignment::Center)
        .spacing(8)
        .width(Fill),
        toolbar_icon(
            '\u{e42c}',
            l.get(if app.preferences.details {
                "hide-details"
            } else {
                "show-details"
            }),
            Some(Message::ToggleDetails),
            app.preferences.details
        ),
        toolbar_icon(
            if app.can_pause_all() {
                '\u{e12e}'
            } else {
                '\u{e13c}'
            },
            l.get(if app.can_pause_all() {
                "pause-all"
            } else {
                "resume-all"
            }),
            (app.can_pause_all() || app.can_resume_all()).then_some(Message::PauseAll),
            false
        ),
        toolbar_icon(
            '\u{e4f9}',
            l.get(if app.alternate_speed() {
                "alternate-off"
            } else {
                "alternate-on"
            }),
            Some(Message::AlternateSpeed),
            app.alternate_speed()
        ),
        toolbar_icon(
            '\u{e154}',
            l.get("settings"),
            Some(Message::Settings(true)),
            false
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if let UpdateStatus::Available(release) = &app.update_status {
        toolbar = toolbar.push(
            theme::action(text(
                l.with("update-available", [("version", release.tag_name.clone())]),
            ))
            .on_press(Message::SettingsPage(Page::About)),
        );
    }
    container(toolbar)
        .style(theme::panel)
        .padding([8, 10])
        .width(Fill)
        .into()
}

fn toolbar_icon(
    glyph: char,
    label: String,
    message: Option<Message>,
    selected: bool,
) -> Element<'static, Message> {
    let control = button(container(icon(glyph).size(19)).center(iced::Fill))
        .width(TOOLBAR_BUTTON_SIZE)
        .height(TOOLBAR_BUTTON_SIZE)
        .padding(0)
        .on_press_maybe(message)
        .style(move |theme, state| theme::toggle(theme, state, selected));
    tooltip(
        control,
        container(text(label)).padding(7).style(theme::panel),
        tooltip::Position::Bottom,
    )
    .into()
}

pub(crate) fn icon_button(
    glyph: char,
    label: String,
    message: Message,
) -> Element<'static, Message> {
    tooltip(
        button(container(icon(glyph)).center(Fill))
            .padding(0)
            .width(ROW_ACTION_WIDTH)
            .height(30)
            .style(theme::selection_plain)
            .on_press(message),
        container(text(label)).padding(5).style(theme::panel),
        tooltip::Position::Bottom,
    )
    .into()
}

pub(crate) fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme::surfaces(theme).muted),
    }
}

pub(crate) fn heading(glyph: char, title: String) -> Element<'static, Message> {
    row![
        container(icon(glyph).size(19))
            .center(36)
            .style(theme::accent_tile),
        text(title).size(23)
    ]
    .spacing(11)
    .align_y(Alignment::Center)
    .into()
}

fn sidebar_heading(label: String) -> Element<'static, Message> {
    container(text(label).size(12).style(muted))
        .padding([3, 8])
        .into()
}

fn sidebar(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let q = &app.queue;
    let mut body = column![sidebar_heading(l.get("status"))].spacing(1);
    for status in StatusFilter::ALL {
        body = body.push(filter(
            l.get(status.key()),
            match status {
                StatusFilter::All => '\u{e106}',
                StatusFilter::Active => '\u{e0b2}',
                StatusFilter::Queued => '\u{e129}',
                StatusFilter::Paused => '\u{e12e}',
                StatusFilter::Completed => '\u{e06c}',
                StatusFilter::Failed => '\u{e077}',
                StatusFilter::Seeding => '\u{e04a}',
            },
            q.status_count(status),
            q.status == status,
            Message::Status(status),
        ));
    }
    body = body
        .push(space::vertical().height(9))
        .push(sidebar_heading(l.get("file-types")));
    for kind in FILE_TYPES {
        let selected = q.file_type == Some(kind);
        body = body.push(filter(
            l.get(i18n::file_type(kind)),
            icons::file_type(Some(kind)),
            q.type_count(kind),
            selected,
            Message::FileType((!selected).then_some(kind)),
        ));
    }
    body = body
        .push(space::vertical().height(9))
        .push(sidebar_heading(l.get("categories")));
    for item in &q.categories.categories {
        let id = item.category.id;
        let selected = q.category == Some(id);
        body = body.push(filter(
            l.category(&item.category),
            '\u{e0d7}',
            q.category_count(id),
            selected,
            Message::Category((!selected).then_some(id)),
        ));
    }
    container(
        theme::vertical(body.width(Fill))
            .spacing(6)
            .width(Fill)
            .height(Fill),
    )
    .width(208)
    .height(Fill)
    .padding(10)
    .style(theme::panel)
    .into()
}

fn filter(
    label: String,
    glyph: char,
    count: usize,
    selected: bool,
    message: Message,
) -> Element<'static, Message> {
    button(
        row![
            icon(glyph).size(14),
            text(label).size(13).width(Fill),
            text(count.to_string()).size(12).style(muted)
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .width(Fill)
    .padding([4, 8])
    .height(27)
    .style(move |theme, state| theme::selection(theme, state, selected))
    .on_press(message)
    .into()
}

fn center(app: &App) -> Element<'_, Message> {
    let mut table = column![header(app)].width(Fill).height(Fill);
    if app.show_columns {
        let controls = Column::ALL.into_iter().fold(
            row![text(app.strings.get("columns"))].spacing(4),
            |row, item| {
                let selected = app.preferences.columns & item.mask() != 0;
                row.push(
                    button(text(app.strings.get(item.key())))
                        .on_press(Message::ToggleColumn(item))
                        .style(move |theme, state| theme::selection(theme, state, selected)),
                )
            },
        );
        table = table.push(controls);
    }
    table = table.push(download_list(app));
    if app.menu.is_some() {
        table = table.push(context_menu(app));
    }
    column![
        container(table.spacing(4))
            .style(theme::panel)
            .clip(true)
            .padding(1)
            .width(Fill)
            .height(Fill),
        details(app, app.details_height())
    ]
    .spacing(10)
    .width(Fill)
    .height(Fill)
    .into()
}

fn columns(app: &App) -> impl Iterator<Item = Column> + '_ {
    Column::ALL
        .into_iter()
        .filter(|column| app.preferences.columns & column.mask() != 0)
}

fn header(app: &App) -> Element<'_, Message> {
    let mut cells = row![
        text(app.strings.get("name"))
            .size(12)
            .style(muted)
            .width(Fill)
    ]
    .spacing(TABLE_CELL_GAP);
    for column in columns(app) {
        cells = cells.push(
            text(app.strings.get(column.key()))
                .size(12)
                .style(muted)
                .width(column.width()),
        );
    }
    let header = container(cells)
        .width(Fill)
        .padding(Padding {
            left: 12.0,
            right: (TABLE_RIGHT_PADDING + ROW_ACTIONS_WIDTH + theme::SCROLLBAR_GUTTER) as f32,
            ..Padding::new(9.0)
        })
        .style(theme::inset);
    mouse_area(header)
        .on_right_press(Message::ToggleColumns)
        .into()
}

fn download_list(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let q = &app.queue;
    if q.rows.is_empty() {
        let key = if app.booting {
            "loading"
        } else if q.downloads.is_empty() {
            "empty-queue"
        } else {
            "no-matches"
        };
        let mut content = column![
            container(icon('\u{e0b2}').size(25))
                .center(48)
                .style(theme::accent_tile),
            text(l.get(key)).size(17),
        ]
        .spacing(12)
        .align_x(Alignment::Center);
        if !app.booting && q.downloads.is_empty() {
            content = content.push(text(l.get("empty-queue-hint")).size(13).style(muted));
        }
        return container(content).center(Fill).into();
    }
    let range = queue::visible_rows(app.offset, app.viewport_height, q.rows.len());
    let start = range.start;
    let end = range.end;
    let mut rows = column![space::vertical().height(start as f32 * ROW_HEIGHT)].width(Fill);
    for item in &q.rows[range] {
        rows = rows.push(match *item {
            ListRow::Download(id) => download_row(app, &q.downloads[&id]),
            ListRow::Package(id, count) => button(
                row![
                    icon(if q.collapsed.contains(&id) {
                        '\u{e06f}'
                    } else {
                        '\u{e06d}'
                    }),
                    icon('\u{e129}'),
                    text(q.packages.get(&id).map_or_else(
                        || l.with(
                            "package",
                            [("id", id.0.to_string()), ("count", count.to_string())]
                        ),
                        |name| l.with(
                            "package-named",
                            [("name", name.clone()), ("count", count.to_string())]
                        )
                    )),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .on_press(Message::Package(id))
            .style(theme::selection_plain)
            .height(ROW_HEIGHT)
            .width(Fill)
            .into(),
        });
    }
    rows = rows.push(space::vertical().height((q.rows.len() - end) as f32 * ROW_HEIGHT));
    theme::vertical(container(rows).padding(Padding {
        right: theme::SCROLLBAR_GUTTER as f32,
        ..Padding::ZERO
    }))
    .id(queue_id())
    .height(Fill)
    .width(Fill)
    .on_scroll(|scroll| Message::Scrolled {
        offset: scroll.absolute_offset().y,
        height: scroll.bounds().height,
    })
    .into()
}

fn download_row<'a>(app: &'a App, download: &Download) -> Element<'a, Message> {
    let l = &app.strings;
    let q = &app.queue;
    let status = download.status();
    let id = download.id;
    let speed = q.speed(id);
    let upload = q.upload_speed(id);
    let name = queue::display_name(download);
    let selected = app.selection.contains(&id);
    let mut cells = row![
        container(
            row![
                icon(icons::file_type(Some(queue::file_type(download)))),
                text(name.clone()).wrapping(text::Wrapping::None),
            ]
            .align_y(Alignment::Center)
            .spacing(7)
        )
        .clip(true)
        .width(Fill)
    ]
    .spacing(TABLE_CELL_GAP)
    .align_y(Alignment::Center);
    for column in columns(app) {
        let content: Element<'_, Message> = match column {
            Column::Size => text(
                download
                    .total_bytes
                    .map_or_else(|| l.get("unknown"), |bytes| size(l, bytes)),
            )
            .into(),
            Column::Progress => {
                let value = if status == Status::Completed {
                    Some(100.0)
                } else {
                    download
                        .total_bytes
                        .filter(|bytes| *bytes > 0)
                        .map(|total| {
                            (download.downloaded_bytes as f64 * 100.0 / total as f64).min(100.0)
                                as f32
                        })
                };
                let label = value.map_or_else(
                    || size(l, download.downloaded_bytes),
                    |value| l.with("percent", [("value", format!("{value:.0}"))]),
                );
                row![
                    progress_bar(0.0..=100.0, value.unwrap_or_default())
                        .length(Fill)
                        .girth(5)
                        .style(move |theme| theme::progress(theme, status)),
                    text(label).size(12)
                ]
                .align_y(Alignment::Center)
                .spacing(7)
                .into()
            }
            Column::Speed => text(if speed > 0 && upload > 0 {
                format!("↓ {} · ↑ {}", rate(l, speed), rate(l, upload))
            } else if upload > 0 {
                format!("↑ {}", rate(l, upload))
            } else if speed > 0 {
                rate(l, speed)
            } else {
                l.get("unknown")
            })
            .into(),
            Column::Eta => text(
                q.eta_secs(id)
                    .map_or_else(|| l.get("unknown"), |secs| duration(l, secs)),
            )
            .into(),
            Column::Status => text(l.get(status_key(q, download)))
                .size(12)
                .style(move |theme| text::Style {
                    color: Some(theme::status_color(theme, status)),
                })
                .into(),
        };
        cells = cells.push(container(content).width(column.width()).clip(true));
    }
    let row_button = button(cells)
        .style(move |theme, state| theme::selection(theme, state, selected))
        .width(Fill)
        .height(ROW_HEIGHT)
        .padding(Padding {
            left: if download.package.is_some() {
                24.0
            } else {
                10.0
            },
            right: TABLE_RIGHT_PADDING as f32,
            top: 4.0,
            bottom: 4.0,
        })
        .on_press(Message::Select(id));
    let row_button = tooltip(
        row_button,
        container(text(name).size(13).wrapping(text::Wrapping::WordOrGlyph))
            .max_width(450)
            .padding(7)
            .style(theme::panel),
        tooltip::Position::FollowCursor,
    )
    .delay(std::time::Duration::from_millis(550));
    let pause = if status == Status::WaitingUser {
        icon_button(
            '\u{e102}',
            l.get("open-verification"),
            Message::OpenVerification(id),
        )
    } else if crate::actions::can_pause(status) {
        icon_button(
            '\u{e12e}',
            l.get("pause"),
            Message::SingleAction(id, Action::TogglePause),
        )
    } else if crate::actions::can_resume(status) {
        icon_button(
            '\u{e13c}',
            l.get("resume"),
            Message::SingleAction(id, Action::TogglePause),
        )
    } else {
        space::horizontal().width(ROW_ACTION_WIDTH).into()
    };
    let controls: Element<'_, Message> = if selected || app.hovered == Some(id) {
        row![
            pause,
            icon_button(
                '\u{e247}',
                l.get("open-folder"),
                Message::SingleAction(id, Action::OpenFolder)
            ),
            icon_button('\u{e0b6}', l.get("actions"), Message::Menu(id)),
        ]
        .align_y(Alignment::Center)
        .into()
    } else {
        space::horizontal().width(ROW_ACTIONS_WIDTH).into()
    };
    mouse_area(
        row![row_button, controls]
            .align_y(Alignment::Center)
            .height(ROW_HEIGHT),
    )
    .on_enter(Message::Hover(Some(id)))
    .on_exit(Message::Hover(None))
    .on_right_press(Message::Menu(id))
    .into()
}

fn status_key(q: &queue::Queue, download: &Download) -> &'static str {
    let status = download.status();
    if q.offline(download.id) {
        "offline"
    } else if matches!(download.source, Source::Torrent(_)) && status == Status::Resolving {
        "fetching-metadata"
    } else {
        i18n::status(status)
    }
}

fn details(app: &App, height: f32) -> Element<'_, Message> {
    let l = &app.strings;
    let toggle = button(
        row![
            icon(if app.preferences.details {
                '\u{e06d}'
            } else {
                '\u{e06f}'
            })
            .size(14),
            text(l.get("details")).size(15),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([5, 8])
    .style(theme::selection_plain)
    .on_press(Message::ToggleDetails);
    let mut tabs = row![toggle]
        .spacing(8)
        .height(30)
        .align_y(Alignment::Center);
    let panel = |body| {
        container(body)
            .height(height)
            .clip(true)
            .width(Fill)
            .style(theme::panel)
            .padding([9, 13])
            .into()
    };
    let tabs_opacity = app.details_tabs_opacity();
    if tabs_opacity == 0.0 {
        return panel(column![tabs]);
    }
    let download = app
        .queue
        .selected
        .and_then(|id| app.queue.downloads.get(&id));
    if let Some(download) = download.filter(|download| download.status() == Status::WaitingUser) {
        tabs = tabs.push(
            theme::action(text(l.get("open-verification")))
                .on_press(Message::OpenVerification(download.id)),
        );
    }
    let is_torrent = download.is_some_and(|item| matches!(item.source, Source::Torrent(_)));
    let visible_tabs: Vec<(DetailTab, &'static str)> = if is_torrent {
        vec![
            (DetailTab::General, "general"),
            (DetailTab::Pieces, "pieces"),
            (DetailTab::Files, "files"),
            (DetailTab::Peers, "peers"),
            (DetailTab::Trackers, "trackers"),
            (DetailTab::Log, "log"),
        ]
    } else {
        vec![
            (DetailTab::General, "general"),
            (DetailTab::Segments, "segments"),
            (DetailTab::Log, "log"),
        ]
    };
    let active_tab = if app.detail_tab.allowed(is_torrent) {
        app.detail_tab
    } else {
        DetailTab::General
    };
    let selection_opacity = app.selection_opacity();
    for (tab, key) in visible_tabs {
        let selected = active_tab == tab;
        tabs = tabs.push(
            button(text(l.get(key)))
                .on_press_maybe(app.preferences.details.then_some(Message::DetailTab(tab)))
                .style(move |theme, state| {
                    let mut style =
                        theme::animated_selection(theme, state, selected, selection_opacity);
                    style.text_color = style.text_color.scale_alpha(tabs_opacity);
                    style.background = style
                        .background
                        .map(|color| color.scale_alpha(tabs_opacity));
                    style
                }),
        );
    }
    let torrent_details = download.and_then(|item| {
        app.torrent_details
            .as_ref()
            .filter(|(id, _)| *id == item.id)
            .map(|(_, snapshot)| snapshot)
    });
    let content: Element<'_, Message> = match (download, active_tab) {
        (None, _) => text(l.get("no-details")).style(muted).into(),
        (Some(download), DetailTab::General) => {
            let source = match &download.source {
                Source::Torrent(TorrentSource::File(path)) => path.to_string_lossy().into_owned(),
                source => queue::source_text(source).to_owned(),
            };
            let mut fields = column![
                field(l.get("source"), source),
                field(l.get("folder"), download.destination.display().to_string()),
                field(l.get("status"), l.get(status_key(&app.queue, download))),
            ]
            .spacing(6);
            if app.queue.resumable(download.id) == Some(false) {
                fields = fields.push(text(l.get("not-resumable")));
            }
            if let Some(checksum) = &download.checksum {
                fields = fields.push(field(l.get("checksum"), checksum.to_string()));
            }
            if let Some(info) = app.queue.torrent_info(download.id) {
                fields = fields
                    .push(field(
                        l.get("peers-connected"),
                        format!("{} / {}", info.peers_connected, info.peers_total),
                    ))
                    .push(field(l.get("uploaded"), size(l, info.uploaded_bytes)))
                    .push(field(
                        l.get("share-ratio"),
                        format!("{:.2}", info.seed_ratio),
                    ));
            }
            fields.into()
        }
        (Some(download), DetailTab::Segments) => {
            if let Some(segments) = app.queue.segments(download.id) {
                segment_map(l, segments)
            } else {
                text(l.get("no-segments")).into()
            }
        }
        (Some(_), DetailTab::Pieces) => {
            if let Some(details) = torrent_details
                && let Some(map) = &details.piece_map
            {
                piece_map_view(l, map, details)
            } else {
                text(l.get("no-pieces")).into()
            }
        }
        (Some(download), DetailTab::Files) => {
            if let Some(details) = torrent_details
                && !details.files.is_empty()
            {
                torrent_files_view(l, download.id, details)
            } else {
                text(l.get("no-files")).into()
            }
        }
        (Some(_), DetailTab::Peers) => {
            if let Some(details) = torrent_details
                && !details.peers.is_empty()
            {
                torrent_peers_view(l, details)
            } else {
                text(l.get("no-peers")).into()
            }
        }
        (Some(_), DetailTab::Trackers) => {
            if let Some(details) = torrent_details
                && !details.trackers.is_empty()
            {
                torrent_trackers_view(l, details)
            } else {
                text(l.get("no-trackers")).into()
            }
        }
        (Some(download), DetailTab::Log) => {
            if let Some(logs) = app.queue.logs.get(&download.id) {
                column(
                    logs.iter()
                        .rev()
                        .map(|message| text(l.get(log_key(*message))).into()),
                )
                .spacing(4)
                .into()
            } else {
                text(l.get("no-log")).into()
            }
        }
    };
    panel(
        column![
            tabs,
            rule::horizontal(1).style(move |theme| {
                let mut style = theme::separator(theme);
                style.color = style.color.scale_alpha(tabs_opacity);
                style
            }),
            theme::vertical(content).spacing(6).height(Fill)
        ]
        .spacing(8),
    )
}

fn piece_map_view(
    l: &Strings,
    map: &TorrentPieceMap,
    details: &TorrentDetailsSnapshot,
) -> Element<'static, Message> {
    let cells = map
        .buckets(64)
        .into_iter()
        .fold(row![].spacing(2), |cells, state| {
            cells.push(container(space::horizontal()).width(Fill).height(12).style(
                move |theme: &Theme| {
                    let palette = theme.palette();
                    let color = match state {
                        PieceBucketState::Complete => palette.success,
                        PieceBucketState::Partial => palette.warning,
                        PieceBucketState::Missing => {
                            theme.extended_palette().background.strong.color
                        }
                    };
                    container::Style::default().background(color)
                },
            ))
        });
    column![
        cells,
        text(l.with(
            "piece-summary",
            [
                ("have", map.have_count.to_string()),
                ("total", map.piece_count.to_string()),
                ("size", size(l, u64::from(map.piece_length))),
            ]
        ))
        .size(12),
        field(l.get("uploaded"), size(l, details.uploaded_bytes)),
        field(l.get("share-ratio"), format!("{:.2}", details.seed_ratio)),
    ]
    .spacing(6)
    .into()
}

fn torrent_files_view(
    l: &Strings,
    id: ruscello_core::DownloadId,
    details: &TorrentDetailsSnapshot,
) -> Element<'static, Message> {
    let mut rows = column![].spacing(4);
    for file in &details.files {
        let index = file.index;
        let include = file.priority == TorrentFilePriority::Download;
        let label = file.path.to_string_lossy().into_owned();
        rows = rows.push(
            row![
                theme::check(include)
                    .label(label)
                    .on_toggle(move |checked| Message::SetTorrentFilePriority(id, index, checked))
                    .width(Fill),
                text(format!(
                    "{} / {}",
                    size(l, file.downloaded_bytes),
                    size(l, file.length)
                ))
                .size(12),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    rows.into()
}

fn torrent_peers_view(l: &Strings, details: &TorrentDetailsSnapshot) -> Element<'static, Message> {
    let mut rows = column![].spacing(4);
    for peer in &details.peers {
        let client = peer.client.clone().unwrap_or_else(|| l.get("unknown"));
        let transport = match peer.transport {
            Some(TorrentPeerTransport::Tcp) => "TCP",
            Some(TorrentPeerTransport::Utp) => "uTP",
            Some(TorrentPeerTransport::Socks) => "SOCKS",
            None => "—",
        };
        let state_key = match peer.state {
            TorrentPeerState::Live => "peer-live",
            TorrentPeerState::Connecting => "peer-connecting",
            TorrentPeerState::Queued => "peer-queued",
            TorrentPeerState::Dead => "peer-dead",
            TorrentPeerState::NotNeeded => "peer-not-needed",
        };
        rows = rows.push(
            row![
                text(peer.address.clone()).width(180).size(12),
                text(client).width(Fill).size(12),
                text(transport).width(54).size(12),
                text(l.get(state_key)).width(100).size(12),
                text(format!(
                    "↓ {} · ↑ {}",
                    rate(l, peer.download_speed_bps),
                    rate(l, peer.upload_speed_bps)
                ))
                .size(12),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    rows.into()
}

fn torrent_trackers_view(
    l: &Strings,
    details: &TorrentDetailsSnapshot,
) -> Element<'static, Message> {
    let mut rows = column![].spacing(4);
    for tracker in &details.trackers {
        let status_key = match tracker.status {
            TorrentTrackerStatus::Announcing => "tracker-announcing",
            TorrentTrackerStatus::Standby => "tracker-standby",
            TorrentTrackerStatus::DisabledPrivate => "tracker-disabled-private",
        };
        rows = rows.push(
            row![
                text(tracker.url.clone()).width(Fill).size(12),
                text(tracker.protocol.clone()).width(70).size(12),
                text(l.get(status_key)).width(130).size(12),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    rows.into()
}

fn segment_map(
    l: &Strings,
    segments: ruscello_engine::SegmentProgress,
) -> Element<'static, Message> {
    let cells = (0..64).fold(row![].spacing(2), |cells, index| {
        let mask = 1 << index;
        cells.push(container(space::horizontal()).width(Fill).height(12).style(
            move |theme: &Theme| {
                let palette = theme.palette();
                let color = if segments.active & mask != 0 {
                    palette.primary
                } else if segments.complete & mask != 0 {
                    palette.success
                } else {
                    theme.extended_palette().background.strong.color
                };
                container::Style::default().background(color)
            },
        ))
    });
    column![
        cells,
        text(l.with(
            "segment-connections",
            [("count", segments.connections.to_string())]
        ))
    ]
    .spacing(8)
    .into()
}

fn field(label: String, value: String) -> Element<'static, Message> {
    row![text(label).style(muted).width(105), text(value).width(Fill)]
        .spacing(6)
        .into()
}

fn status_bar(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let q = &app.queue;
    let limit = if app.alternate_speed() {
        Some(app.preferences.alternate_bandwidth)
    } else {
        q.limits.bandwidth
    };
    let speed = limit.map_or_else(
        || l.get("limit-off"),
        |limit| l.with("speed-limit", [("speed", rate(l, limit.get()))]),
    );
    let active = q.active_count;
    let disk = app.disk_space.map_or_else(
        || l.get("disk-unknown"),
        |bytes| l.with("disk-free", [("space", size(l, bytes))]),
    );
    let mut status = row![
        icon('\u{e042}').size(14).style(muted),
        text(l.with("download-rate", [("speed", rate(l, q.total_speed()))])).size(12),
        icon('\u{e04a}').size(14).style(muted),
        text(l.with("upload-rate", [("speed", rate(l, q.total_upload_speed()))])).size(12),
        rule::vertical(1).style(theme::separator),
        text(speed).size(12).style(muted),
        text(l.with(
            "active-limit",
            [
                ("active", active.to_string()),
                ("limit", q.limits.concurrent_downloads.to_string())
            ]
        ))
        .size(12)
        .style(muted),
    ]
    .align_y(Alignment::Center)
    .spacing(9)
    .height(16);
    if let Some(nodes) = q.torrent_session.dht_nodes {
        status = status.push(
            text(l.with("dht-nodes", [("count", nodes.to_string())]))
                .size(12)
                .style(muted),
        );
    }
    status = status
        .push(space::horizontal().width(Fill))
        .push(icon('\u{e0ed}').size(14).style(muted))
        .push(text(disk).size(12));
    container(status)
        .width(Fill)
        .padding([8, 12])
        .style(theme::panel)
        .into()
}

pub(crate) fn option(label: String, selected: bool, message: Message) -> Element<'static, Message> {
    theme::action(text(label))
        .on_press(message)
        .style(move |theme, state| theme::toggle(theme, state, selected))
        .into()
}

fn size(l: &Strings, bytes: u64) -> String {
    let mut scaled = bytes as f64;
    for unit in ["bytes", "kib", "mib", "gib", "tib"] {
        if scaled < 1024.0 || unit == "tib" {
            let value = if unit == "bytes" {
                bytes.to_string()
            } else {
                format!("{scaled:.1}")
            };
            return l.with(unit, [("value", value)]);
        }
        scaled /= 1024.0;
    }
    l.get("unknown")
}

fn rate(l: &Strings, bytes: u64) -> String {
    l.with("rate", [("size", size(l, bytes))])
}

fn duration(l: &Strings, seconds: u64) -> String {
    let (unit, value) = if seconds >= 3600 {
        ("hours", seconds.div_ceil(3600))
    } else if seconds >= 60 {
        ("minutes", seconds.div_ceil(60))
    } else {
        ("seconds", seconds)
    };
    l.with(unit, [("value", value.to_string())])
}

pub(crate) fn log_key(message: EngineLog) -> &'static str {
    match message {
        EngineLog::DownloadFailed(_) => "log-download-failed",
        EngineLog::DatabaseWriteFailed => "log-database",
        EngineLog::CleanupFailed => "log-cleanup",
        EngineLog::DiskSpaceLow => "log-disk",
        EngineLog::TorrentSessionInitFailed => "log-torrent-session",
        EngineLog::VerificationRequired => "waiting-user",
    }
}

fn torrent_preview_row<'a>(
    l: &'a Strings,
    dialog: &'a crate::add::Dialog,
    key: &'a str,
) -> Element<'a, Message> {
    let mut details = column![text(key.to_owned()).wrapping(text::Wrapping::None)].spacing(4);
    match dialog.torrent_previews.get(key) {
        Some(TorrentPreview::Pending) | None => {
            details = details.push(text(l.get("probing")).size(12).style(muted));
        }
        Some(TorrentPreview::Failed(error)) => {
            let note = if matches!(crate::add::torrent_source(key), TorrentSource::File(_)) {
                format!("{}: {error}", l.get("torrent-invalid"))
            } else {
                l.get("torrent-metadata-pending")
            };
            details = details.push(text(note).size(12).style(muted));
        }
        Some(TorrentPreview::Ready(info)) => {
            let selected = dialog.torrent_files.get(key);
            let selected_bytes: u64 = info
                .files
                .iter()
                .filter(|file| selected.is_some_and(|set| set.contains(&file.index)))
                .map(|file| file.length)
                .sum();
            details = details.push(
                text(l.with(
                    "torrent-files-summary",
                    [
                        ("name", info.name.clone()),
                        ("selected", size(l, selected_bytes)),
                        ("total", size(l, info.total_bytes)),
                        ("count", info.files.len().to_string()),
                    ],
                ))
                .size(12)
                .style(muted),
            );
            if info.files.len() > 1 {
                let all_key = key.to_owned();
                let none_key = key.to_owned();
                details = details.push(
                    row![
                        theme::action(text(l.get("select-all")).size(11))
                            .padding([4, 8])
                            .on_press(Message::ToggleAllTorrentFiles(all_key, true)),
                        theme::action(text(l.get("select-none")).size(11))
                            .padding([4, 8])
                            .on_press(Message::ToggleAllTorrentFiles(none_key, false)),
                    ]
                    .spacing(6),
                );
            }
            for file in &info.files {
                let included = selected.is_some_and(|set| set.contains(&file.index));
                let file_key = key.to_owned();
                let index = file.index;
                details = details.push(
                    row![
                        theme::check(included)
                            .label(file.path.to_string_lossy().into_owned())
                            .on_toggle(move |_| Message::ToggleTorrentFile(file_key.clone(), index))
                            .width(Fill),
                        text(size(l, file.length)).size(11).style(muted),
                    ]
                    .align_y(Alignment::Center)
                    .spacing(6),
                );
            }
        }
    }
    container(
        row![
            text(l.get("torrent-input"))
                .size(12)
                .style(muted)
                .width(152),
            details.width(Fill),
        ]
        .spacing(8),
    )
    .clip(true)
    .padding([6, 9])
    .into()
}

fn add_dialog<'a>(app: &'a App, dialog: &'a crate::add::Dialog) -> Element<'a, Message> {
    let l = &app.strings;
    let mut choices = row![option(
        l.get("automatic"),
        dialog.category.is_none(),
        Message::AddCategory(None)
    )]
    .spacing(5);
    for item in &app.queue.categories.categories {
        let category = &item.category;
        choices = choices.push(option(
            l.category(category),
            dialog.category == Some(category.id),
            Message::AddCategory(Some(category.id)),
        ));
    }
    let previews = column(dialog.entries.iter().map(|input| {
        if let Input::Torrent(key) = input {
            return torrent_preview_row(l, dialog, key);
        }
        let (key, detail) = match input {
            Input::Http(url) => match dialog.previews.get(url) {
                Some(Probe::File(name, bytes)) => (
                    "direct-link",
                    bytes.map_or_else(
                        || name.clone(),
                        |bytes| format!("{name} · {}", size(l, bytes)),
                    ),
                ),
                Some(Probe::Web(title, _)) => ("web-input", title.clone()),
                Some(Probe::Hoster(plugin)) => ("hoster-input", plugin.clone()),
                Some(Probe::Media) => ("media-input", l.get("media-unavailable")),
                Some(Probe::Unavailable) => ("probe-unavailable", String::new()),
                Some(Probe::Verification) => ("waiting-user", String::new()),
                Some(Probe::AccountRequired) => ("hoster-input", l.get("hoster-token-required")),
                Some(Probe::Pending) | None => ("probing", String::new()),
            },
            Input::Dlc(path) | Input::Text(path) => {
                match dialog.containers.get(path).and_then(Option::as_ref) {
                    Some(Ok(_)) => (input.key(), String::new()),
                    Some(Err(error)) => (input.key(), l.get(error)),
                    None => ("probing", String::new()),
                }
            }
            _ => (input.key(), String::new()),
        };
        let mut line = row![
            text(l.get(key)).size(12).style(muted).width(152),
            column![
                text(input.label()).wrapping(text::Wrapping::None),
                text(detail).size(12).style(muted)
            ]
            .width(Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if app.preferences.browser_helper
            && let Input::Http(url) = input
        {
            let key = match dialog.previews.get(url) {
                Some(Probe::Verification) => Some(("open-verification", true)),
                Some(Probe::Web(..)) => Some(("analyze-in-browser", false)),
                _ => None,
            };
            if let Some((key, verify)) = key {
                line = line.push(
                    theme::action(text(l.get(key)).size(12))
                        .on_press(Message::AnalyzeInBrowser(url.clone(), verify)),
                );
            }
        }
        container(line).clip(true).padding([6, 9]).into()
    }))
    .spacing(4);
    let destination = dialog
        .destination
        .as_ref()
        .map_or_else(|| l.get("automatic"), |path| path.display().to_string());
    let can_submit = !app.booting && dialog.can_submit();
    let preview: Element<'_, Message> = if dialog.entries.is_empty() {
        row![
            icon('\u{e102}').style(muted),
            text(l.get("add-preview-empty")).size(13).style(muted),
        ]
        .align_y(Alignment::Center)
        .spacing(9)
        .into()
    } else {
        let preview_height = if dialog
            .entries
            .iter()
            .any(|entry| matches!(entry, Input::Torrent(_)))
        {
            148
        } else {
            90
        };
        theme::vertical(previews)
            .spacing(6)
            .height(preview_height)
            .into()
    };
    let mut advanced = column![
        button(
            row![
                icon(if dialog.advanced {
                    '\u{e06d}'
                } else {
                    '\u{e06f}'
                })
                .size(14),
                text(l.get("advanced"))
            ]
            .spacing(6)
            .align_y(Alignment::Center)
        )
        .on_press(Message::AddAdvanced)
        .padding(theme::CONTROL_PADDING)
        .style(theme::selection_plain)
    ]
    .spacing(5);
    if dialog.advanced {
        advanced = advanced.push(
            row![
                column![
                    text(l.get("add-connections")).size(13).style(muted),
                    theme::field("", &dialog.connections).on_input(Message::AddConnections),
                ]
                .spacing(5)
                .width(Fill),
                column![
                    text(l.get("checksum")).size(13).style(muted),
                    theme::field(&l.get("checksum-format"), &dialog.checksum)
                        .on_input(Message::AddChecksum),
                ]
                .spacing(5)
                .width(Fill),
                column![
                    text(l.get("download-bandwidth")).size(13).style(muted),
                    theme::field("", &dialog.bandwidth).on_input(Message::AddBandwidth),
                ]
                .spacing(5)
                .width(Fill),
            ]
            .spacing(8),
        );
    }
    let sources = column![
        row![
            text(l.get("source")).size(15).width(Fill),
            theme::action(
                row![icon('\u{e0d7}').size(15), text(l.get("choose-files"))]
                    .spacing(7)
                    .align_y(Alignment::Center)
            )
            .on_press(Message::PickFiles),
        ]
        .align_y(Alignment::Center),
        text_editor(&app.editor)
            .id(add_id())
            .placeholder(l.get("add-placeholder"))
            .on_action(Message::EditAdd)
            .style(theme::editor)
            .height(104)
            .padding(12),
        container(preview)
            .width(Fill)
            .padding([9, 10])
            .style(theme::inset),
    ]
    .spacing(10);
    let options = column![
        text(l.get("category")).size(13).style(muted),
        theme::horizontal(choices).width(Fill),
        row![
            icon('\u{e0d7}').style(muted),
            column![
                text(l.get("folder")).size(12).style(muted),
                text(destination)
            ]
            .spacing(3)
            .width(Fill),
            theme::action(text(l.get("choose-folder"))).on_press(Message::PickDestination)
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        advanced,
    ]
    .spacing(10);
    let add_scroll_gutter = Padding {
        right: theme::SCROLLBAR_GUTTER as f32,
        ..Padding::ZERO
    };
    let body = column![
        container(sources)
            .padding(15)
            .width(Fill)
            .style(theme::panel),
    ]
    .extend(grabber(app, dialog))
    .push(
        container(options)
            .padding(15)
            .width(Fill)
            .style(theme::panel),
    )
    .spacing(10)
    .padding(add_scroll_gutter);
    container(
        column![
            container(
                row![
                    heading('\u{e13d}', l.get("add-download")),
                    space::horizontal().width(Fill),
                    theme::action(text(l.get("back")))
                        .on_press_maybe((!dialog.submitting).then_some(Message::CloseAdd))
                ]
                .align_y(Alignment::Center)
            )
            .padding(add_scroll_gutter),
            theme::vertical(body).height(Fill).width(Fill),
            container(rule::horizontal(1).style(theme::separator)).padding(add_scroll_gutter),
            container(
                row![
                    theme::check(!dialog.paused)
                        .label(l.get("start-immediately"))
                        .on_toggle(Message::StartImmediately),
                    space::horizontal().width(Fill),
                    theme::action(
                        row![
                            icon('\u{e13d}'),
                            text(l.get(
                                if dialog.page_candidates().next().is_some()
                                    || dialog.imported_packages().next().is_some()
                                {
                                    "download-selected"
                                } else {
                                    "add"
                                }
                            ))
                        ]
                        .spacing(7)
                        .align_y(Alignment::Center)
                    )
                    .padding([8, 14])
                    .style(theme::primary)
                    .on_press_maybe(can_submit.then_some(Message::SubmitAdd)),
                ]
                .align_y(Alignment::Center)
                .spacing(8)
            )
            .padding(add_scroll_gutter),
        ]
        .spacing(12)
        .max_width(804)
        .height(Fill),
    )
    .width(Fill)
    .height(Fill)
    .align_x(Alignment::Center)
    .padding([5, 10])
    .into()
}

const CANDIDATE_GROUPS: [(CandidateGroup, &str); 4] = [
    (CandidateGroup::Likely, "grabber-likely"),
    (CandidateGroup::OtherFiles, "grabber-other"),
    (CandidateGroup::Media, "grabber-media"),
    (CandidateGroup::LinkedPages, "grabber-pages"),
];

fn grabber<'a>(app: &'a App, dialog: &'a crate::add::Dialog) -> Option<Element<'a, Message>> {
    if dialog.page_candidates().next().is_none() && dialog.imported_packages().next().is_none() {
        return None;
    }
    let l = &app.strings;
    let search = dialog.grabber_search.trim().to_lowercase();
    let matches = |name: &str, url: &ruscello_engine::Url| {
        search.is_empty()
            || name.to_lowercase().contains(&search)
            || url.as_str().to_lowercase().contains(&search)
    };
    let mut list = column![
        text(l.get("linkgrabber")).size(16),
        theme::field(&l.get("grabber-search"), &dialog.grabber_search)
            .on_input(Message::GrabberSearch),
    ]
    .spacing(8);
    if dialog.page_candidates().next().is_some() {
        let mut filters = row![option(
            l.get("all"),
            dialog.grabber_filter.is_none(),
            Message::GrabberFilter(None)
        )]
        .spacing(5);
        for (group, label) in CANDIDATE_GROUPS {
            filters = filters.push(option(
                l.get(label),
                dialog.grabber_filter == Some(group),
                Message::GrabberFilter(Some(group)),
            ));
        }
        list = list.push(theme::horizontal(filters));
    }
    for package in dialog.imported_packages() {
        list = list.push(text(&package.name).size(14));
        for item in &package.links {
            let name = item
                .filename
                .as_deref()
                .unwrap_or_else(|| item.url.as_str());
            if matches(name, &item.url) {
                list = list.push(candidate_row(l, dialog, &item.url, name, item.size, None));
            }
        }
    }
    for (title, candidates) in dialog.page_candidates() {
        list = list.push(text(title).size(14));
        if candidates.is_empty() {
            list = list.push(text(l.get("grabber-empty")).style(muted));
        }
        for (group, label) in CANDIDATE_GROUPS {
            if dialog.grabber_filter.is_some_and(|filter| group != filter) {
                continue;
            }
            let selected = candidates
                .iter()
                .filter(|candidate| {
                    candidate.group == group && matches(&candidate.filename, &candidate.url)
                })
                .collect::<Vec<_>>();
            if selected.is_empty() {
                continue;
            }
            let collapsed = dialog.collapsed_groups.contains(&group);
            list = list.push(
                theme::action(text(format!(
                    "{} {} ({})",
                    if collapsed { "▸" } else { "▾" },
                    l.get(label),
                    selected.len()
                )))
                .on_press(Message::GrabberGroup(group)),
            );
            if !collapsed {
                for candidate in selected {
                    list = list.push(candidate_row(
                        l,
                        dialog,
                        &candidate.url,
                        &candidate.filename,
                        candidate.size,
                        Some(candidate),
                    ));
                }
            }
        }
    }
    Some(
        container(list)
            .padding(15)
            .width(Fill)
            .style(theme::panel)
            .into(),
    )
}

fn candidate_row<'a>(
    l: &'a Strings,
    dialog: &'a crate::add::Dialog,
    url: &'a ruscello_engine::Url,
    name: &'a str,
    bytes: Option<u64>,
    candidate: Option<&'a ruscello_engine::Candidate>,
) -> Element<'a, Message> {
    let mut check = theme::check(dialog.selected_links.contains(url)).label(name);
    if candidate.is_none_or(|candidate| candidate.group != CandidateGroup::Media) {
        check = check.on_toggle(|checked| Message::SelectLink(url.clone(), checked));
    }
    let mut content = row![
        check.width(Fill),
        text(url.host_str().unwrap_or_default())
            .size(12)
            .style(muted),
        text(bytes.map_or_else(|| "—".into(), |bytes| size(l, bytes)))
            .size(12)
            .style(muted),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if let Some(candidate) = candidate {
        let explanation = candidate
            .reasons
            .iter()
            .map(|reason| l.get(score_reason(*reason)))
            .collect::<Vec<_>>()
            .join(" · ");
        let score = text(candidate.score.to_string()).size(12);
        content = content.push(
            tooltip(score, text(explanation).size(12), tooltip::Position::Top).style(theme::panel),
        );
    }
    container(content)
        .padding([4, 6])
        .width(Fill)
        .clip(true)
        .into()
}

fn score_reason(reason: ScoreReason) -> &'static str {
    match reason {
        ScoreReason::FileExtension => "score-extension",
        ScoreReason::LargeFile => "score-size",
        ScoreReason::DownloadLabel => "score-label",
        ScoreReason::TitleMatch => "score-title",
        ScoreReason::KnownHoster => "score-hoster",
        ScoreReason::Attachment => "score-attachment",
        ScoreReason::Tracker => "score-tracker",
        ScoreReason::TinyFile => "score-tiny",
        ScoreReason::InterfaceAsset => "score-asset",
        ScoreReason::SocialOrLogin => "score-social",
    }
}

fn context_menu(app: &App) -> Element<'_, Message> {
    use ruscello_core::Priority;
    let l = &app.strings;
    let selected_torrent = app.selection.iter().any(|id| {
        app.queue
            .downloads
            .get(id)
            .is_some_and(crate::actions::can_recheck)
    });
    let mut actions = row![].spacing(4);
    if app.selection.iter().any(|id| {
        app.queue
            .downloads
            .get(id)
            .is_some_and(|download| download.status() == Status::WaitingUser)
    }) {
        actions = actions.push(
            theme::action(text(l.get("open-verification")).size(12))
                .on_press(Message::RowAction(Action::Verify)),
        );
    }
    for (key, action) in [
        ("pause-resume", Action::TogglePause),
        ("open-file", Action::OpenFile),
        ("open-folder", Action::OpenFolder),
        ("copy-link", Action::CopyLink),
        ("priority-low", Action::Priority(Priority::Low)),
        ("priority-normal", Action::Priority(Priority::Normal)),
        ("priority-high", Action::Priority(Priority::High)),
        ("move-up", Action::MoveUp),
        ("move-down", Action::MoveDown),
        ("remove", Action::Cancel),
    ] {
        actions = actions.push(
            theme::action(text(l.get(key)).size(12))
                .on_press(Message::RowAction(action))
                .style(move |theme, state| match action {
                    Action::Cancel => theme::danger(theme, state),
                    _ => theme::secondary(theme, state),
                }),
        );
    }
    if selected_torrent {
        actions = actions.push(
            theme::action(text(l.get("force-recheck")).size(12))
                .on_press(Message::RowAction(Action::ForceRecheck)),
        );
    }
    container(theme::horizontal(actions).width(Fill))
        .width(Fill)
        .style(theme::panel)
        .padding(6)
        .into()
}

fn remove_dialog(app: &App, count: usize, delete: bool) -> Element<'_, Message> {
    let l = &app.strings;
    container(
        container(
            column![
                text(l.with("confirm-remove", [("count", count.to_string())])).size(20),
                theme::check(delete)
                    .label(l.get("delete-files"))
                    .on_toggle(Message::DeleteFiles),
                row![
                    theme::action(text(l.get("back"))).on_press(Message::CancelRemove),
                    theme::action(text(l.get("remove")))
                        .on_press(Message::ConfirmRemove)
                        .style(theme::danger)
                ]
                .spacing(8),
            ]
            .spacing(12),
        )
        .style(theme::panel)
        .padding(20),
    )
    .center(Fill)
    .into()
}
