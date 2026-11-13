use iced::{
    Alignment, Element, Fill, Padding, Theme,
    widget::{
        self, button, column, container, mouse_area, progress_bar, row, scrollable, space, text,
        text_input, tooltip,
    },
};
use ruscello_core::{Download, Status};
use ruscello_engine::EngineLog;

use crate::{
    app::{App, Column, DetailTab, Message},
    i18n::{self, Strings},
    icons::{self, icon},
    preferences::{Appearance, Locale},
    queue::{self, FILE_TYPES, ListRow, ROW_HEIGHT, StatusFilter},
    theme,
};

pub(crate) fn queue_id() -> widget::Id {
    widget::Id::new("download-list")
}

pub(crate) fn main(app: &App) -> Element<'_, Message> {
    let content = if app.settings {
        settings(app)
    } else {
        column![
            toolbar(app),
            row![sidebar(app), center(app)].spacing(12).height(Fill),
            status_bar(app)
        ]
        .spacing(10)
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
    container(body).padding(12).width(Fill).height(Fill).into()
}

fn toolbar(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    row![
        text(l.get("downloads")).size(22),
        space::horizontal().width(10),
        icon('\u{e151}'),
        text_input(&l.get("search"), &app.queue.search)
            .on_input(Message::Search)
            .padding(8)
            .width(Fill),
        icon_button(
            '\u{e42c}',
            l.get(if app.preferences.details {
                "hide-details"
            } else {
                "show-details"
            }),
            Message::ToggleDetails
        ),
        icon_button('\u{e154}', l.get("settings"), Message::Settings(true)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

pub(crate) fn icon_button(
    glyph: char,
    label: String,
    message: Message,
) -> Element<'static, Message> {
    tooltip(
        button(icon(glyph))
            .padding(8)
            .style(button::secondary)
            .on_press(message),
        container(text(label)).padding(5).style(theme::panel),
        tooltip::Position::Bottom,
    )
    .into()
}

fn sidebar(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let q = &app.queue;
    let mut body = column![text(l.get("status")).size(12)].spacing(3);
    for status in StatusFilter::ALL {
        body = body.push(filter(
            l.get(status.key()),
            q.status_count(status),
            q.status == status,
            Message::Status(status),
        ));
    }
    body = body
        .push(space::vertical().height(8))
        .push(text(l.get("file-types")).size(12));
    for kind in FILE_TYPES {
        let selected = q.file_type == Some(kind);
        body = body.push(filter(
            l.get(i18n::file_type(kind)),
            q.type_count(kind),
            selected,
            Message::FileType((!selected).then_some(kind)),
        ));
    }
    body = body
        .push(space::vertical().height(8))
        .push(text(l.get("categories")).size(12));
    for item in &q.categories.categories {
        let id = item.category.id;
        let selected = q.category == Some(id);
        body = body.push(filter(
            l.category(&item.category),
            q.category_count(id),
            selected,
            Message::Category((!selected).then_some(id)),
        ));
    }
    container(scrollable(body).width(Fill).height(Fill))
        .width(180)
        .height(Fill)
        .padding(10)
        .style(theme::panel)
        .into()
}

fn filter(
    label: String,
    count: usize,
    selected: bool,
    message: Message,
) -> Element<'static, Message> {
    button(
        row![text(label).width(Fill), text(count.to_string()).size(12)]
            .spacing(4)
            .align_y(Alignment::Center),
    )
    .width(Fill)
    .padding([4, 7])
    .style(move |theme, state| theme::selection(theme, state, selected))
    .on_press(message)
    .into()
}

fn center(app: &App) -> Element<'_, Message> {
    let mut body = column![header(app)].width(Fill).height(Fill);
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
        body = body.push(controls);
    }
    body = body.push(download_list(app));
    if app.preferences.details {
        body = body.push(details(app));
    }
    body.spacing(4).into()
}

fn columns(app: &App) -> impl Iterator<Item = Column> + '_ {
    Column::ALL
        .into_iter()
        .filter(|column| app.preferences.columns & column.mask() != 0)
}

fn header(app: &App) -> Element<'_, Message> {
    let mut cells = row![text(app.strings.get("name")).size(12).width(Fill)].spacing(6);
    for column in columns(app) {
        cells = cells.push(
            text(app.strings.get(column.key()))
                .size(12)
                .width(column.width()),
        );
    }
    let header = container(cells)
        .width(Fill)
        .padding([7, 12])
        .style(theme::panel);
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
        return container(text(l.get(key))).center(Fill).into();
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
                    text(l.with(
                        "package",
                        [("id", id.0.to_string()), ("count", count.to_string())]
                    )),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .on_press(Message::Package(id))
            .style(button::secondary)
            .height(ROW_HEIGHT)
            .width(Fill)
            .into(),
        });
    }
    rows = rows.push(space::vertical().height((q.rows.len() - end) as f32 * ROW_HEIGHT));
    scrollable(rows)
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
    let name = queue::display_name(download);
    let selected = q.selected == Some(id);
    let mut cells = row![
        container(
            row![
                icon(icons::file_type(Some(queue::file_type(download)))),
                text(name).wrapping(text::Wrapping::None),
            ]
            .align_y(Alignment::Center)
            .spacing(7)
        )
        .clip(true)
        .width(Fill)
    ]
    .spacing(6)
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
            Column::Speed => text(if speed > 0 {
                rate(l, speed)
            } else {
                l.get("unknown")
            })
            .into(),
            Column::Eta => text(download.total_bytes.filter(|_| speed > 0).map_or_else(
                || l.get("unknown"),
                |total| {
                    duration(
                        l,
                        total
                            .saturating_sub(download.downloaded_bytes)
                            .div_ceil(speed),
                    )
                },
            ))
            .into(),
            Column::Status => text(l.get(if q.offline(id) {
                "offline"
            } else {
                i18n::status(status)
            }))
            .size(12)
            .into(),
        };
        cells = cells.push(container(content).width(column.width()).clip(true));
    }
    button(cells)
        .style(move |theme, state| theme::selection(theme, state, selected))
        .width(Fill)
        .height(ROW_HEIGHT)
        .padding(Padding {
            left: if download.package.is_some() {
                24.0
            } else {
                10.0
            },
            right: 12.0,
            top: 4.0,
            bottom: 4.0,
        })
        .on_press(Message::Select(id))
        .into()
}

fn details(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let mut tabs = row![
        text(l.get("details")).size(16),
        space::horizontal().width(10)
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    for (tab, key) in [
        (DetailTab::General, "general"),
        (DetailTab::Segments, "segments"),
        (DetailTab::Log, "log"),
    ] {
        let selected = app.detail_tab == tab;
        tabs = tabs.push(
            button(text(l.get(key)))
                .on_press(Message::DetailTab(tab))
                .style(move |theme, state| theme::selection(theme, state, selected)),
        );
    }
    let download = app
        .queue
        .selected
        .and_then(|id| app.queue.downloads.get(&id));
    let content: Element<'_, Message> = match (download, app.detail_tab) {
        (None, _) => text(l.get("no-details")).into(),
        (Some(download), DetailTab::General) => {
            let source = match &download.source {
                ruscello_core::Source::Torrent(ruscello_core::TorrentSource::File(path)) => {
                    path.to_string_lossy().into_owned()
                }
                source => queue::source_text(source).to_owned(),
            };
            let mut fields = column![
                field(l.get("source"), source),
                field(l.get("folder"), download.destination.display().to_string()),
                field(l.get("status"), l.get(i18n::status(download.status()))),
            ]
            .spacing(6);
            if app.queue.resumable(download.id) == Some(false) {
                fields = fields.push(text(l.get("not-resumable")));
            }
            fields.into()
        }
        (Some(_), DetailTab::Segments) => text(l.get("no-segments")).into(),
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
    container(column![tabs, scrollable(content).height(Fill)].spacing(5))
        .height(142)
        .width(Fill)
        .style(theme::panel)
        .padding(10)
        .into()
}

fn field(label: String, value: String) -> Element<'static, Message> {
    row![text(label).width(105), text(value).width(Fill)]
        .spacing(6)
        .into()
}

fn status_bar(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let q = &app.queue;
    let speed = q.limits.bandwidth.map_or_else(
        || l.get("limit-off"),
        |limit| l.with("speed-limit", [("speed", rate(l, limit.get()))]),
    );
    let active = q.active_count;
    let disk = app.disk_space.map_or_else(
        || l.get("disk-unknown"),
        |bytes| l.with("disk-free", [("space", size(l, bytes))]),
    );
    row![
        icon('\u{e042}'),
        text(l.with("download-rate", [("speed", rate(l, q.total_speed()))])).size(12),
        icon('\u{e04a}'),
        text(l.with("upload-rate", [("speed", rate(l, 0))])).size(12),
        text(speed).size(12),
        text(l.with(
            "active-limit",
            [
                ("active", active.to_string()),
                ("limit", q.limits.concurrent_downloads.to_string())
            ]
        ))
        .size(12),
        space::horizontal().width(Fill),
        icon('\u{e0ed}'),
        text(disk).size(12),
    ]
    .align_y(Alignment::Center)
    .spacing(8)
    .into()
}

fn settings(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let locale = [Locale::System, Locale::English, Locale::Italian]
        .into_iter()
        .fold(row![].spacing(6), |row, locale| {
            row.push(option(
                l.get(locale.key()),
                app.preferences.locale == locale,
                Message::Locale(locale),
            ))
        });
    let appearance = [Appearance::System, Appearance::Light, Appearance::Dark]
        .into_iter()
        .fold(row![].spacing(6), |row, appearance| {
            row.push(option(
                l.get(appearance.key()),
                app.preferences.appearance == appearance,
                Message::Appearance(appearance),
            ))
        });
    column![
        row![
            button(text(l.get("back"))).on_press(Message::Settings(false)),
            text(l.get("settings")).size(22)
        ]
        .spacing(12)
        .align_y(Alignment::Center),
        container(
            column![
                text(l.get("general")).size(18),
                text(l.get("language")),
                locale,
                text(l.get("appearance")),
                appearance,
            ]
            .spacing(10)
        )
        .style(theme::panel)
        .padding(16)
        .width(Fill),
        space::vertical().height(Fill),
    ]
    .spacing(12)
    .into()
}

fn option(label: String, selected: bool, message: Message) -> Element<'static, Message> {
    button(text(label))
        .on_press(message)
        .style(move |theme: &Theme, state| {
            if selected {
                button::primary(theme, state)
            } else {
                button::secondary(theme, state)
            }
        })
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
    }
}
