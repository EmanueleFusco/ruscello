use iced::{
    Alignment, Element, Fill, Point, Rectangle, Renderer as IcedRenderer, Size, Theme, mouse,
    widget::{button, canvas, checkbox, column, container, row, scrollable, text, text_input},
};
use ruscello_engine::Url;

use crate::{
    app::{App, Message},
    preferences::{Appearance, Locale, Renderer},
    settings::{Folder, Number, Page, UpdateStatus},
    theme,
    view::option,
};

pub(crate) fn view(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let navigation = column(Page::ALL.into_iter().map(|page| {
        button(text(l.get(page.key())))
            .width(Fill)
            .on_press(Message::SettingsPage(page))
            .style(move |theme, status| theme::selection(theme, status, page == app.page))
            .into()
    }))
    .spacing(5)
    .width(170);
    let content = match app.page {
        Page::General => general(app),
        Page::Downloads => downloads(app),
        Page::Speed => speed(app),
        Page::Torrent => text(l.get("torrent-unavailable")).into(),
        Page::Categories => categories(app),
        Page::Integrations => integrations(app),
        Page::Advanced => advanced(app),
        Page::About => about(app),
    };
    column![
        row![
            button(text(l.get("back"))).on_press(Message::Settings(false)),
            text(l.get("settings")).size(22)
        ]
        .align_y(Alignment::Center)
        .spacing(10),
        row![
            container(navigation).padding(8).style(theme::panel),
            scrollable(
                container(content)
                    .padding(18)
                    .width(Fill)
                    .style(theme::panel)
            )
            .width(Fill)
            .height(Fill)
        ]
        .spacing(12)
        .height(Fill),
    ]
    .spacing(12)
    .into()
}

fn general(app: &App) -> Element<'_, Message> {
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
        text(l.get("language")),
        locale,
        text(l.get("appearance")),
        appearance,
        text(l.get("renderer")),
        row![
            option(
                l.get("renderer-hardware"),
                app.preferences.renderer == Renderer::Hardware,
                Message::Renderer(Renderer::Hardware)
            ),
            option(
                l.get("renderer-software"),
                app.preferences.renderer == Renderer::Software,
                Message::Renderer(Renderer::Software)
            ),
        ]
        .spacing(6),
        text(l.get("renderer-note")),
    ]
    .spacing(12)
    .into()
}

fn downloads(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let destination = app
        .preferences
        .default_folder
        .as_deref()
        .unwrap_or_else(|| app.system_download_folder());
    let temporary = app
        .preferences
        .temporary_folder
        .as_ref()
        .map_or_else(|| l.get("automatic"), |path| path.display().to_string());
    column![
        text(l.get("default-folder")),
        row![
            text(destination.display().to_string()).width(Fill),
            button(text(l.get("choose-folder"))).on_press(Message::ChooseFolder(Folder::Default))
        ]
        .spacing(8),
        text(l.get("temporary-folder")),
        row![
            text(temporary).width(Fill),
            button(text(l.get("choose-folder"))).on_press(Message::ChooseFolder(Folder::Temporary)),
            button(text(l.get("reset"))).on_press(Message::ClearTemporary),
        ]
        .spacing(8),
        text(l.get("folder-restart")),
        number(app, Number::Concurrent),
        number(app, Number::PerHost),
        number(app, Number::PerDownload),
        save(app),
    ]
    .spacing(10)
    .into()
}

fn speed(app: &App) -> Element<'_, Message> {
    column![
        number(app, Number::Bandwidth),
        number(app, Number::Alternate),
        save(app)
    ]
    .spacing(10)
    .into()
}

fn categories(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let mut categories = column![text(l.get("categories")).size(18)].spacing(8);
    for policy in &app.queue.categories.categories {
        let category = &policy.category;
        let folder = category
            .destination
            .as_ref()
            .map_or_else(|| l.get("automatic"), |path| path.display().to_string());
        categories =
            categories.push(row![text(l.category(category)).width(180), text(folder)].spacing(10));
    }
    categories
        .push(text(l.with(
            "rules-count",
            [("count", app.queue.categories.rules.len().to_string())],
        )))
        .into()
}

fn integrations(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let mut controls = column![
        text(l.get("lan-title")).size(18),
        text(l.get("lan-pending")),
        checkbox(app.preferences.lan_enabled)
            .label(l.get("lan-enabled"))
            .on_toggle(Message::LanEnabled),
        number(app, Number::Port),
        checkbox(app.preferences.lan_auth)
            .label(l.get("lan-auth"))
            .on_toggle(Message::LanAuth),
        save(app),
    ]
    .spacing(10);
    if app.preferences.lan_enabled
        && let Some(ip) = app.lan_address
    {
        let url = format!("http://{ip}:{}", app.preferences.lan_port);
        if Url::parse(&url).is_ok()
            && let Ok(code) = qrcode::QrCode::new(&url)
        {
            controls = controls
                .push(text(url))
                .push(canvas(Qr(code)).width(160).height(160));
        }
    }
    controls
        .push(text(l.get("browser-extension")))
        .push(text(l.get("sidecars")))
        .into()
}

fn advanced(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    column![
        button(text(l.get("open-logs"))).on_press(Message::OpenLogs),
        button(text(l.get("copy-diagnostics"))).on_press(Message::CopyDiagnostics),
    ]
    .spacing(10)
    .into()
}

fn about(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let mut content = column![
        text(l.with(
            "version",
            [("version", env!("CARGO_PKG_VERSION").to_owned())]
        ))
        .size(18),
        checkbox(app.preferences.check_updates)
            .label(l.get("check-on-start"))
            .on_toggle(Message::CheckAutomatically),
        button(text(l.get("check-now"))).on_press_maybe(
            (!matches!(app.update_status, UpdateStatus::Checking)).then_some(Message::CheckUpdates)
        ),
    ]
    .spacing(10);
    let status = match &app.update_status {
        UpdateStatus::Idle => None,
        UpdateStatus::Checking => Some("update-checking"),
        UpdateStatus::Current => Some("update-current"),
        UpdateStatus::Failed => Some("update-error"),
        UpdateStatus::Available(release) => {
            content = content.push(
                text(l.with("update-available", [("version", release.tag_name.clone())])).size(18),
            );
            if let Some(notes) = &release.body {
                content = content.push(text(l.get("release-notes"))).push(text(notes));
            }
            content = content
                .push(button(text(l.get("open-release"))).on_press(Message::OpenRelease))
                .push(text(l.get("install-pending")));
            None
        }
    };
    if let Some(status) = status {
        content = content.push(text(l.get(status)));
    }
    content.into()
}

fn number(app: &App, field: Number) -> Element<'_, Message> {
    let label = app.strings.get(field.key());
    row![
        text(label.clone()).width(260),
        text_input(&label, &app.settings_draft.fields[field.index()])
            .on_input(move |value| Message::SettingNumber(field, value))
            .padding(7)
            .width(160)
    ]
    .align_y(Alignment::Center)
    .spacing(8)
    .into()
}

fn save(app: &App) -> Element<'_, Message> {
    button(text(app.strings.get("save")))
        .on_press(Message::SaveLimits)
        .into()
}

struct Qr(qrcode::QrCode);

impl canvas::Program<Message> for Qr {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &IcedRenderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), iced::Color::WHITE);
        let side = self.0.width();
        let cell = (bounds.width.min(bounds.height) / (side + 8) as f32).floor();
        for y in 0..side {
            for x in 0..side {
                if self.0[(x, y)] == qrcode::Color::Dark {
                    frame.fill_rectangle(
                        Point::new((x + 4) as f32 * cell, (y + 4) as f32 * cell),
                        Size::new(cell, cell),
                        iced::Color::BLACK,
                    );
                }
            }
        }
        vec![frame.into_geometry()]
    }
}
