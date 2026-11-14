use iced::{
    Alignment, Element, Fill, Point, Rectangle, Renderer as IcedRenderer, Size, Theme, mouse,
    widget::{button, canvas, column, container, keyed_column, row, rule, space, text},
};
use ruscello_engine::SidecarKind;

use crate::{
    app::{App, Message},
    icons::icon,
    preferences::{Appearance, Locale, Renderer},
    settings::{Folder, Number, Page, TorrentField, TorrentOption, UpdateStatus},
    theme,
    view::{heading, muted, option},
};

pub(crate) fn view(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let opacity = app.selection_opacity();
    let navigation = column(Page::ALL.into_iter().map(|page| {
        let glyph = match page {
            Page::General => '\u{e154}',
            Page::Downloads => '\u{e0b2}',
            Page::Speed => '\u{e29a}',
            Page::Torrent => '\u{e102}',
            Page::Categories => '\u{e0d7}',
            Page::Integrations => '\u{e247}',
            Page::Hosters => '\u{e154}',
            Page::Advanced => '\u{e29a}',
            Page::About => '\u{e0f9}',
        };
        button(
            row![icon(glyph).size(15), text(l.get(page.key())).width(Fill)]
                .spacing(9)
                .align_y(Alignment::Center),
        )
        .width(Fill)
        .padding([8, 10])
        .on_press(Message::SettingsPage(page))
        .style(move |theme, status| {
            theme::animated_selection(theme, status, page == app.page, opacity)
        })
        .into()
    }))
    .spacing(3)
    .width(Fill);
    let content = match app.page {
        Page::General => general(app),
        Page::Downloads => downloads(app),
        Page::Speed => speed(app),
        Page::Torrent => torrent(app),
        Page::Categories => categories(app),
        Page::Integrations => integrations(app),
        Page::Hosters => hosters(app),
        Page::Advanced => advanced(app),
        Page::About => about(app),
    };
    column![
        row![
            heading('\u{e154}', l.get("settings")),
            space::horizontal().width(Fill),
            theme::action(text(l.get("back"))).on_press(Message::Settings(false)),
        ]
        .align_y(Alignment::Center)
        .spacing(10),
        row![
            container(theme::vertical(navigation).spacing(6))
                .padding(8)
                .width(210)
                .height(Fill)
                .style(theme::panel),
            keyed_column![(
                app.page,
                theme::vertical(
                    column![
                        text(l.get(app.page.key())).size(20),
                        rule::horizontal(1).style(theme::separator),
                        container(content)
                            .padding(18)
                            .width(Fill)
                            .style(theme::panel)
                    ]
                    .spacing(12)
                    .width(Fill)
                )
                .spacing(6)
                .width(Fill)
                .height(Fill)
            )]
            .width(Fill)
            .height(Fill)
        ]
        .spacing(12)
        .height(Fill),
    ]
    .spacing(14)
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
    let controls = column![
        text(l.get("language")).size(15),
        locale,
        rule::horizontal(1).style(theme::separator),
        text(l.get("appearance")).size(15),
        appearance,
        theme::check(app.preferences.motion)
            .label(l.get("interface-motion"))
            .on_toggle(Message::Motion),
        rule::horizontal(1).style(theme::separator),
        text(l.get("renderer")).size(15),
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
        text(l.get("renderer-note")).size(13).style(muted),
    ];
    #[cfg(windows)]
    let controls = controls.push(nvidia_profile(app));
    controls
        .push(rule::horizontal(1).style(theme::separator))
        .push(
            theme::check(app.preferences.close_to_tray)
                .label(l.get("close-to-tray"))
                .on_toggle(Message::CloseToTray),
        )
        .push(
            theme::check(app.preferences.notifications)
                .label(l.get("notifications"))
                .on_toggle(Message::Notifications),
        )
        .push(
            theme::check(app.preferences.autostart)
                .label(l.get("autostart"))
                .on_toggle(Message::Autostart),
        )
        .spacing(12)
        .into()
}

#[cfg(windows)]
fn nvidia_profile(app: &App) -> Element<'_, Message> {
    use crate::nvidia::{Action, State};

    let l = &app.strings;
    let (status, action) = match app.nvidia_profile {
        State::Checking => ("nvidia-checking", None),
        State::Available if app.preferences.renderer == Renderer::Hardware => (
            "nvidia-description",
            Some(("nvidia-enable", Action::Enable)),
        ),
        State::Available => ("nvidia-requires-hardware", None),
        State::External => ("nvidia-external", None),
        State::Managed => ("nvidia-managed", Some(("nvidia-restore", Action::Restore))),
        State::Shared => ("nvidia-shared", None),
        State::Changed => ("nvidia-changed", None),
        State::Unavailable => ("nvidia-unavailable", None),
        State::Failed => ("nvidia-failed", Some(("nvidia-retry", Action::Read))),
    };
    let mut controls = column![
        rule::horizontal(1).style(theme::separator),
        text(l.get("nvidia-refresh")).size(15),
        text(l.get(status)).size(13).style(muted),
    ]
    .spacing(8);
    if let Some((label, action)) = action {
        controls = controls
            .push(theme::action(text(l.get(label))).on_press(Message::NvidiaProfile(action)));
    }
    if app.nvidia_restart {
        controls = controls.push(text(l.get("nvidia-restart")).size(13).style(muted));
    }
    controls.into()
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
            theme::action(text(l.get("choose-folder")))
                .on_press(Message::ChooseFolder(Folder::Default))
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        text(l.get("temporary-folder")),
        row![
            text(temporary).width(Fill),
            theme::action(text(l.get("choose-folder")))
                .on_press(Message::ChooseFolder(Folder::Temporary)),
            theme::action(text(l.get("reset"))).on_press(Message::ClearTemporary),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        text(l.get("folder-restart")).style(muted),
        rule::horizontal(1).style(theme::separator),
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

fn torrent(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let preferences = &app.preferences.torrent;
    let mut controls = column![].spacing(10);
    if let Some(port) = app.queue.torrent_session.listen_port {
        controls = controls.push(
            text(l.with("torrent-listen-active", [("port", port.to_string())]))
                .size(13)
                .style(muted),
        );
    }
    for (key, value, choice) in [
        ("torrent-dht", preferences.dht, TorrentOption::Dht),
        ("torrent-pex", preferences.pex, TorrentOption::Pex),
        ("torrent-upnp", preferences.upnp, TorrentOption::Upnp),
        ("torrent-lsd", preferences.lsd, TorrentOption::Lsd),
    ] {
        controls = controls.push(
            theme::check(value)
                .label(l.get(key))
                .on_toggle(move |value| Message::TorrentOption(choice, value)),
        );
    }
    controls = controls.push(rule::horizontal(1).style(theme::separator));
    for (key, value, field) in [
        ("torrent-port", &preferences.port, TorrentField::Port),
        (
            "torrent-download-limit",
            &preferences.download_limit,
            TorrentField::DownloadLimit,
        ),
        (
            "torrent-upload-limit",
            &preferences.upload_limit,
            TorrentField::UploadLimit,
        ),
        (
            "torrent-ratio",
            &preferences.seed_ratio,
            TorrentField::Ratio,
        ),
        (
            "torrent-minutes",
            &preferences.seed_minutes,
            TorrentField::Minutes,
        ),
        (
            "torrent-max-peers",
            &preferences.max_peers,
            TorrentField::MaxPeers,
        ),
        (
            "torrent-interface",
            &preferences.interface,
            TorrentField::Interface,
        ),
    ] {
        let label = l.get(key);
        controls = controls.push(
            row![
                text(label.clone()).width(260),
                theme::field(&label, value)
                    .on_input(move |value| Message::TorrentField(field, value))
                    .width(160),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    let blocklist_label = l.get("torrent-blocklist");
    controls = controls.push(
        row![
            text(blocklist_label.clone()).width(260),
            theme::field(&blocklist_label, &preferences.blocklist)
                .on_input(|value| Message::TorrentField(TorrentField::Blocklist, value))
                .width(Fill),
            theme::action(text(l.get("choose-files"))).on_press(Message::PickBlocklist),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
    );
    controls = controls.push(
        theme::action(text(l.get("save")))
            .style(theme::primary)
            .on_press(Message::SaveTorrentSettings),
    );
    controls.into()
}

fn categories(app: &App) -> Element<'_, Message> {
    use crate::settings::CategoryEdit;
    use ruscello_engine::CategoryMatch;
    let l = &app.strings;
    let mut categories = column![].spacing(8);
    let mut choices = row![].spacing(5);
    for policy in &app.queue.categories.categories {
        let category = &policy.category;
        let folder = category
            .destination
            .as_ref()
            .map_or_else(|| l.get("automatic"), |path| path.display().to_string());
        categories = categories.push(
            row![
                text(l.category(category)).width(180),
                text(folder).width(Fill),
                theme::action(text(l.get("choose-folder")))
                    .on_press(Message::ChooseFolder(Folder::Category(category.id))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
        choices = choices.push(option(
            l.category(category),
            app.category_draft.category == Some(category.id),
            Message::CategoryTarget(category.id),
        ));
    }
    categories = categories.push(
        row![
            theme::field(&l.get("category-name"), &app.category_draft.name)
                .on_input(|name| Message::CategoryEdit(CategoryEdit::Name, name)),
            theme::action(text(l.get("create-category")))
                .style(theme::primary)
                .on_press_maybe(
                    (!app.category_draft.name.trim().is_empty()).then_some(Message::CreateCategory)
                ),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
    );
    categories = categories.push(text(l.with(
        "rules-count",
        [("count", app.queue.categories.rules.len().to_string())],
    )));
    for (index, rule) in app.queue.categories.rules.iter().enumerate() {
        let (kind, value) = match &rule.matcher {
            CategoryMatch::Domain(value) => ("domain", value.clone()),
            CategoryMatch::Extension(value) => ("extension", value.clone()),
            CategoryMatch::Regex(value) => ("regex", value.clone()),
            CategoryMatch::Source(value) => ("source", format!("{value:?}")),
        };
        let category = app
            .queue
            .categories
            .categories
            .iter()
            .find(|policy| policy.category.id == rule.category)
            .map_or_else(|| l.get("unknown"), |policy| l.category(&policy.category));
        categories = categories.push(
            row![
                text(format!("{kind}:{value}")).width(Fill),
                text(category),
                theme::action(text(l.get("remove")))
                    .style(theme::danger)
                    .on_press(Message::RemoveCategoryRule(index)),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    categories
        .push(theme::horizontal(choices).width(Fill))
        .push(text(l.get("rule-pattern")).size(13).style(muted))
        .push(
            row![
                theme::field("", &app.category_draft.rule)
                    .on_input(|rule| Message::CategoryEdit(CategoryEdit::Rule, rule)),
                theme::action(text(l.get("add-rule"))).on_press_maybe(
                    (app.category_draft.category.is_some()
                        && !app.category_draft.rule.trim().is_empty())
                    .then_some(Message::AddCategoryRule)
                ),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        )
        .into()
}

fn integrations(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    use ruscello_engine::InstalledBrowser;
    let can_import = app.preferences.browser_helper
        && !app.booting
        && !app.cookie_importing
        && !app.cookie_site.trim().is_empty();
    let mut controls = column![
        text(l.get("browser-helper-title")).size(18),
        theme::check(app.preferences.browser_helper)
            .label(l.get("browser-helper-enabled"))
            .on_toggle(Message::BrowserHelper),
        text(l.get("browser-helper-detail")).size(12).style(muted),
        theme::field(&l.get("cookie-site"), &app.cookie_site).on_input(Message::CookieSite),
        row![
            theme::action(text(l.get("cookie-import-firefox"))).on_press_maybe(
                can_import.then_some(Message::ImportCookies(InstalledBrowser::Firefox))
            ),
            theme::action(text(l.get("cookie-import-chromium"))).on_press_maybe(
                can_import.then_some(Message::ImportCookies(InstalledBrowser::Chromium))
            ),
        ]
        .spacing(8),
        text(if app.cookie_importing {
            l.get("cookie-import-running")
        } else {
            match app.cookie_count {
                Some(0) => l.get("cookie-import-empty"),
                Some(count) => l.with("cookie-imported", [("count", count.to_string())]),
                None => l.get("cookie-import-detail"),
            }
        })
        .size(12)
        .style(muted),
        text(l.get("lan-title")).size(18),
        text(l.get("lan-pending")),
        theme::check(app.preferences.lan_enabled)
            .label(l.get("lan-enabled"))
            .on_toggle(Message::LanEnabled),
        number(app, Number::Port),
        theme::check(app.preferences.lan_auth)
            .label(l.get("lan-auth"))
            .on_toggle(Message::LanAuth),
        save(app),
    ]
    .spacing(10);
    if app.preferences.lan_auth {
        controls = controls.push(
            row![
                theme::field(&l.get("lan-password"), &app.lan_password.0)
                    .secure(true)
                    .on_input(
                        |password| Message::LanPassword(crate::settings::LanPassword(password))
                    ),
                theme::action(text(l.get("save-password"))).on_press_maybe(
                    (!app.lan_password.0.is_empty()).then_some(Message::SaveLanPassword)
                ),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    if app.preferences.lan_enabled
        && let Some(url) = app.lan_url()
        && let Ok(code) = qrcode::QrCode::new(&url)
    {
        controls = controls
            .push(theme::action(text(url)).on_press(Message::CopyLanUrl))
            .push(canvas(Qr(code)).width(160).height(160));
    }
    controls
        .push(text(l.get("browser-extension")))
        .push(sidecars(app))
        .push(
            theme::check(app.preferences.monitor_clipboard)
                .label(l.get("clipboard-monitor"))
                .on_toggle(Message::MonitorClipboard),
        )
        .push(
            theme::check(app.preferences.auto_accept_hosters)
                .label(l.get("auto-accept-hosters"))
                .on_toggle(Message::AutoAcceptHosters),
        )
        .push(
            theme::check(app.preferences.dlc_fallback)
                .label(l.get("dlc-fallback"))
                .on_toggle(Message::DlcFallback),
        )
        .push(text(l.get("dlc-fallback-detail")).size(12).style(muted))
        .into()
}

fn hosters(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let draft = &app.hoster_draft;
    let available = !draft.busy && !draft.host.trim().is_empty();
    column![
        text(l.get("hoster-accounts")).size(18),
        text(l.get("hoster-keyring")),
        row![
            option(
                "pixeldrain.com".into(),
                draft.host == "pixeldrain.com",
                Message::HosterDomain("pixeldrain.com".into())
            ),
            option(
                "gofile.io".into(),
                draft.host == "gofile.io",
                Message::HosterDomain("gofile.io".into())
            ),
        ]
        .spacing(5),
        theme::field(&l.get("hoster-username"), &draft.username).on_input(Message::HosterUsername),
        theme::field(&l.get("hoster-secret"), &draft.password.0)
            .secure(true)
            .on_input(|password| Message::HosterPassword(crate::settings::LanPassword(password))),
        row![
            theme::action(text(l.get("hoster-save"))).on_press_maybe(
                (available && !draft.password.0.is_empty()).then_some(Message::SaveHoster)
            ),
            theme::action(text(l.get("hoster-remove")))
                .on_press_maybe(available.then_some(Message::RemoveHoster)),
        ]
        .spacing(8),
    ]
    .extend(draft.result.map(|result| text(l.get(result)).into()))
    .spacing(10)
    .into()
}
fn advanced(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let controls = column![
        theme::action(text(l.get("open-logs"))).on_press(Message::OpenLogs),
        theme::action(text(l.get("copy-diagnostics"))).on_press(Message::CopyDiagnostics),
    ]
    .spacing(10);
    #[cfg(not(target_os = "macos"))]
    let controls = controls.push(
        theme::action(text(l.get("register-associations"))).on_press(Message::RegisterAssociations),
    );
    controls.into()
}

fn about(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let mut content = column![
        text(l.with(
            "version",
            [("version", env!("CARGO_PKG_VERSION").to_owned())]
        ))
        .size(18),
        theme::check(app.preferences.check_updates)
            .label(l.get("check-on-start"))
            .on_toggle(Message::CheckAutomatically),
        theme::action(text(l.get("check-now"))).on_press_maybe(
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
                .push(theme::action(text(l.get("open-release"))).on_press(Message::OpenRelease))
                .push(theme::action(text(l.get("install-update"))).style(theme::primary))
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
        theme::field(&label, &app.settings_draft.fields[field.index()])
            .on_input(move |value| Message::SettingNumber(field, value))
            .width(160)
    ]
    .align_y(Alignment::Center)
    .spacing(8)
    .into()
}

fn save(app: &App) -> Element<'_, Message> {
    theme::action(text(app.strings.get("save")))
        .style(theme::primary)
        .on_press(Message::SaveLimits)
        .into()
}

fn sidecars(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let mut section = column![
        text(l.get("sidecars-title")).size(18),
        text(l.get("sidecars-detail")).size(12).style(muted),
        sidecar_card(app, SidecarKind::YtDlp),
        sidecar_card(app, SidecarKind::Ffmpeg),
        row![
            theme::action(text(l.get("sidecar-apply-path"))).on_press(Message::SaveSidecarSettings)
        ]
        .spacing(8),
    ]
    .spacing(10);
    if let Some(key) = app.sidecar_draft.message {
        section = section.push(text(l.get(key)).size(12).style(muted));
    }
    section.into()
}

fn sidecar_card(app: &App, kind: SidecarKind) -> Element<'_, Message> {
    let l = &app.strings;
    let (tool_name, title_key, consent_key, consented, binary_status) = match kind {
        SidecarKind::YtDlp => (
            "yt-dlp",
            "sidecar-ytdlp",
            "sidecar-ytdlp-consent",
            app.preferences.sidecars.ytdlp_consent,
            app.sidecar_draft.status.as_ref().map(|s| &s.ytdlp),
        ),
        SidecarKind::Ffmpeg => (
            "ffmpeg",
            "sidecar-ffmpeg",
            "sidecar-ffmpeg-consent",
            app.preferences.sidecars.ffmpeg_consent,
            app.sidecar_draft.status.as_ref().map(|s| &s.ffmpeg),
        ),
    };
    let available = binary_status.is_some_and(|status| status.installed);
    let version = binary_status.and_then(|status| status.version.as_deref());
    let busy_here = app.sidecar_draft.busy == Some(kind);
    let status_text = if busy_here {
        l.get("sidecar-installing")
    } else if available {
        match version {
            Some(version) => l.with("sidecar-installed", [("version", version.to_owned())]),
            None => l.get("sidecar-installed-unknown"),
        }
    } else if consented {
        l.get("sidecar-not-installed")
    } else {
        l.get("sidecar-consent-needed")
    };
    let custom_value = app.sidecar_draft.path_for(kind);
    let action_key = if available && custom_value.trim().is_empty() {
        "sidecar-update"
    } else {
        "sidecar-install"
    };
    let can_install = consented && !app.booting && app.sidecar_draft.busy.is_none();

    let mut card = column![
        row![
            text(l.get(title_key)).width(Fill),
            text(status_text).size(12).style(muted),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        theme::check(consented)
            .label(l.get(consent_key))
            .on_toggle(move |value| Message::SidecarConsent(kind, value)),
    ]
    .spacing(8);

    if kind == SidecarKind::YtDlp {
        card = card.push(
            theme::check(app.preferences.sidecars.ytdlp_auto_update)
                .label(l.get("sidecar-ytdlp-autoupdate"))
                .on_toggle(Message::SidecarAutoUpdate),
        );
    }

    if let Some(path) = binary_status.and_then(|status| status.path.as_ref()) {
        card = card.push(text(path.display().to_string()).size(11).style(muted));
    }

    card = card.push(
        row![
            theme::field(&l.get("sidecar-custom-path"), custom_value)
                .on_input(move |value| Message::SidecarCustomPath(kind, value))
                .width(Fill),
            theme::action(text(
                l.with("sidecar-pick-binary", [("name", tool_name.to_owned())])
            ))
            .on_press(Message::PickSidecarPath(kind)),
            theme::action(text(l.with(action_key, [("name", tool_name.to_owned())])))
                .on_press_maybe(can_install.then_some(Message::InstallSidecar(kind))),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
    );

    container(card)
        .padding(12)
        .width(Fill)
        .style(theme::panel)
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
