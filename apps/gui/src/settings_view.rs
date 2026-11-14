use iced::{
    Alignment, Element, Fill, Point, Rectangle, Renderer as IcedRenderer, Size, Theme, mouse,
    widget::{button, canvas, column, container, keyed_column, row, rule, space, text},
};
use ruscello_engine::SidecarKind;

use crate::{
    app::{App, AutomationMsg, Message},
    icons::icon,
    preferences::{Appearance, Locale, Renderer},
    settings::{
        COMPLETION_ACTIONS, Folder, Number, OutsideHoursMode, Page, ScheduleRuleDraft,
        ScheduleRuleMode, TorrentField, TorrentOption, UpdateStatus, WATCH_BEHAVIORS,
        WatchFolderDraft, completion_action_key, watch_behavior_key,
    },
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
            Page::Automation => '\u{e129}',
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
        Page::Automation => automation(app),
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

fn automation(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    column![
        automation_power_section(app),
        automation_schedule_section(app),
        automation_post_process_section(app),
        automation_watch_folders_section(app),
        row![
            theme::action(text(l.get("save")))
                .style(theme::primary)
                .on_press(Message::Automation(AutomationMsg::Save)),
        ],
    ]
    .spacing(12)
    .into()
}

fn automation_power_section(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let draft = &app.automation_draft;
    let completion_choices =
        COMPLETION_ACTIONS
            .into_iter()
            .fold(row![].spacing(6), |row, action| {
                row.push(option(
                    l.get(completion_action_key(action)),
                    draft.completion_action == action,
                    Message::Automation(AutomationMsg::CompletionActionChanged(action)),
                ))
            });
    let mut section = column![
        text(l.get("automation-power-title")).size(16),
        theme::check(draft.prevent_sleep_when_active)
            .label(l.get("automation-prevent-sleep"))
            .on_toggle(|enabled| Message::Automation(AutomationMsg::TogglePreventSleep(enabled))),
        text(l.get("automation-completion-action")).size(14),
        completion_choices,
    ]
    .spacing(8);
    if matches!(
        draft.completion_action,
        ruscello_engine::QueueCompletionAction::Sleep
            | ruscello_engine::QueueCompletionAction::Shutdown
    ) {
        section = section.push(
            row![
                text(l.get("automation-countdown-secs")).width(220),
                theme::field(
                    &l.get("seconds-placeholder"),
                    &draft.completion_countdown_secs
                )
                .width(120)
                .on_input(|val| Message::Automation(
                    AutomationMsg::CompletionCountdownChanged(val)
                )),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    container(section)
        .padding(12)
        .width(Fill)
        .style(theme::panel)
        .into()
}

fn automation_schedule_section(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let draft = &app.automation_draft;
    let outside_choices = OutsideHoursMode::ALL
        .into_iter()
        .fold(row![].spacing(6), |row, mode| {
            row.push(option(
                l.get(mode.key()),
                draft.outside_mode == mode,
                Message::Automation(AutomationMsg::OutsideHoursModeChanged(mode)),
            ))
        });
    let mut section = column![
        text(l.get("automation-schedule-title")).size(16),
        theme::check(draft.schedule_enabled)
            .label(l.get("automation-schedule-enable"))
            .on_toggle(
                |enabled| Message::Automation(AutomationMsg::ToggleScheduleEnabled(enabled))
            ),
        text(l.get("schedule-outside-hours")).size(14),
        outside_choices,
    ]
    .spacing(8);
    if draft.outside_mode == OutsideHoursMode::LimitSpeed {
        section = section.push(
            row![
                text(l.get("schedule-speed-limit")).width(220),
                theme::field(&l.get("bytes-per-sec-hint"), &draft.outside_speed_bps)
                    .width(160)
                    .on_input(
                        |val| Message::Automation(AutomationMsg::OutsideHoursSpeedChanged(val))
                    ),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    for (index, rule) in draft.schedule_rules.iter().enumerate() {
        section = section.push(schedule_rule_card(app, index, rule));
    }
    section = section.push(
        theme::action(text(l.get("schedule-add-rule")))
            .on_press(Message::Automation(AutomationMsg::AddScheduleRule)),
    );
    container(section)
        .padding(12)
        .width(Fill)
        .style(theme::panel)
        .into()
}

fn schedule_rule_card<'a>(
    app: &'a App,
    index: usize,
    rule: &'a ScheduleRuleDraft,
) -> Element<'a, Message> {
    const DAY_KEYS: [&str; 7] = [
        "day-mon", "day-tue", "day-wed", "day-thu", "day-fri", "day-sat", "day-sun",
    ];
    let l = &app.strings;
    let days = (0_u8..7).fold(row![].spacing(4), |row, day| {
        let active = (rule.days.bits() & (1_u8 << day)) != 0;
        row.push(option(
            l.get(DAY_KEYS[day as usize]),
            active,
            Message::Automation(AutomationMsg::ScheduleRuleDay(index, day, !active)),
        ))
    });
    let modes = ScheduleRuleMode::ALL
        .into_iter()
        .fold(row![].spacing(6), |row, mode| {
            row.push(option(
                l.get(mode.key()),
                rule.mode == mode,
                Message::Automation(AutomationMsg::ScheduleRuleModeChanged(index, mode)),
            ))
        });
    let mut card = column![
        row![
            theme::check(rule.enabled).on_toggle(move |enabled| {
                Message::Automation(AutomationMsg::ToggleScheduleRuleEnabled(index, enabled))
            }),
            theme::field(&l.get("schedule-rule-name"), &rule.name).on_input(move |val| {
                Message::Automation(AutomationMsg::ScheduleRuleName(index, val))
            }),
            theme::action(text(l.get("remove"))).on_press(Message::Automation(
                AutomationMsg::RemoveScheduleRule(index)
            )),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        days,
        row![
            text(l.get("schedule-from")).size(12).style(muted),
            theme::field("23:00", &rule.start_hhmm)
                .width(80)
                .on_input(move |val| {
                    Message::Automation(AutomationMsg::ScheduleRuleStart(index, val))
                }),
            text(l.get("schedule-to")).size(12).style(muted),
            theme::field("07:00", &rule.end_hhmm)
                .width(80)
                .on_input(move |val| {
                    Message::Automation(AutomationMsg::ScheduleRuleEnd(index, val))
                }),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        modes,
    ]
    .spacing(6);
    if rule.mode == ScheduleRuleMode::SpeedLimit {
        card = card.push(
            row![
                text(l.get("schedule-speed-limit")).width(200),
                theme::field(&l.get("bytes-per-sec-hint"), &rule.speed_bps)
                    .width(160)
                    .on_input(move |val| {
                        Message::Automation(AutomationMsg::ScheduleRuleSpeedChanged(index, val))
                    }),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    container(card)
        .padding(10)
        .width(Fill)
        .style(theme::panel)
        .into()
}

fn automation_post_process_section(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let draft = &app.automation_draft;
    let mut passwords = column![].spacing(4);
    for (index, _password) in draft.archive_passwords.iter().enumerate() {
        passwords = passwords.push(
            row![
                text(format!(
                    "{} #{}: ••••••••",
                    l.get("extract-password"),
                    index + 1
                ))
                .width(Fill)
                .size(12),
                theme::action(text(l.get("remove"))).on_press(Message::Automation(
                    AutomationMsg::RemoveArchivePassword(index)
                )),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    let section = column![
        text(l.get("automation-post-title")).size(16),
        theme::check(draft.auto_verify_checksums)
            .label(l.get("automation-verify-checksums"))
            .on_toggle(
                |enabled| Message::Automation(AutomationMsg::ToggleAutoVerifyChecksums(enabled))
            ),
        rule::horizontal(1).style(theme::separator),
        theme::check(draft.auto_extract)
            .label(l.get("automation-auto-extract"))
            .on_toggle(|enabled| Message::Automation(AutomationMsg::ToggleAutoExtract(enabled))),
        theme::check(draft.extract_into_subfolder)
            .label(l.get("automation-extract-subfolder"))
            .on_toggle(
                |enabled| Message::Automation(AutomationMsg::ToggleExtractSubfolder(enabled))
            ),
        theme::check(draft.delete_archives_after_extraction)
            .label(l.get("automation-delete-archives"))
            .on_toggle(|enabled| Message::Automation(AutomationMsg::ToggleDeleteArchives(enabled))),
        row![
            theme::field(
                &l.get("extract-password-placeholder"),
                &draft.new_archive_password
            )
            .secure(true)
            .on_input(|val| Message::Automation(AutomationMsg::ArchivePasswordInput(val))),
            theme::action(text(l.get("extract-add-password")))
                .on_press(Message::Automation(AutomationMsg::AddArchivePassword)),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        passwords,
        row![
            text(l.get("automation-external-extractor")).width(210),
            theme::field("7z / unrar", &draft.external_extractor_path).on_input(|val| {
                Message::Automation(AutomationMsg::ExternalExtractorChanged(val))
            }),
            theme::action(text(l.get("choose-file")))
                .on_press(Message::Automation(AutomationMsg::PickExternalExtractor)),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        rule::horizontal(1).style(theme::separator),
        text(l.get("automation-post-command")).size(14),
        theme::field(
            &l.get("automation-post-command-placeholder"),
            &draft.post_download_command,
        )
        .on_input(|val| Message::Automation(AutomationMsg::PostCommandChanged(val))),
        text(l.get("automation-post-command-help"))
            .size(12)
            .style(muted),
        row![
            text(l.get("automation-post-timeout")).width(210),
            theme::field("60", &draft.post_download_timeout_secs)
                .width(120)
                .on_input(|val| Message::Automation(AutomationMsg::PostTimeoutChanged(val))),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
    ]
    .spacing(8);
    container(section)
        .padding(12)
        .width(Fill)
        .style(theme::panel)
        .into()
}

fn automation_watch_folders_section(app: &App) -> Element<'_, Message> {
    let l = &app.strings;
    let draft = &app.automation_draft;
    let mut section = column![
        text(l.get("automation-watch-title")).size(16),
        theme::check(draft.watch_enabled)
            .label(l.get("automation-watch-enable"))
            .on_toggle(|enabled| Message::Automation(AutomationMsg::ToggleWatchEnabled(enabled))),
        row![
            text(l.get("automation-watch-interval")).width(210),
            theme::field("5", &draft.watch_poll_interval_secs)
                .width(120)
                .on_input(|val| Message::Automation(AutomationMsg::WatchPollChanged(val))),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
    ]
    .spacing(8);
    for (index, rule) in draft.watch_rules.iter().enumerate() {
        section = section.push(watch_folder_card(app, index, rule));
    }
    section = section.push(
        row![
            theme::action(text(l.get("watch-add-folder")))
                .on_press(Message::Automation(AutomationMsg::AddWatchRule)),
            theme::action(text(l.get("watch-browse-new")))
                .on_press(Message::Automation(AutomationMsg::PickNewWatchRuleDir)),
            theme::action(text(l.get("watch-scan-now")))
                .on_press(Message::Automation(AutomationMsg::ScanWatchNow)),
        ]
        .spacing(8),
    );
    container(section)
        .padding(12)
        .width(Fill)
        .style(theme::panel)
        .into()
}

fn watch_folder_card<'a>(
    app: &'a App,
    index: usize,
    rule: &'a WatchFolderDraft,
) -> Element<'a, Message> {
    let l = &app.strings;
    let behaviors = WATCH_BEHAVIORS
        .into_iter()
        .fold(row![].spacing(6), |row, behavior| {
            row.push(option(
                l.get(watch_behavior_key(behavior)),
                rule.processed_behavior == behavior,
                Message::Automation(AutomationMsg::WatchRuleBehaviorChanged(index, behavior)),
            ))
        });
    let card = column![
        row![
            theme::check(rule.enabled).on_toggle(move |enabled| {
                Message::Automation(AutomationMsg::ToggleWatchRuleEnabled(index, enabled))
            }),
            theme::field(&l.get("folder"), &rule.directory).on_input(move |val| {
                Message::Automation(AutomationMsg::WatchRuleDirChanged(index, val))
            }),
            theme::action(text(l.get("choose-folder")))
                .on_press(Message::Automation(AutomationMsg::PickWatchRuleDir(index))),
            theme::action(text(l.get("remove")))
                .on_press(Message::Automation(AutomationMsg::RemoveWatchRule(index))),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        row![
            theme::check(rule.import_torrents)
                .label(".torrent")
                .on_toggle(move |enabled| {
                    Message::Automation(AutomationMsg::ToggleWatchRuleTorrents(index, enabled))
                }),
            theme::check(rule.import_dlc)
                .label(".dlc")
                .on_toggle(move |enabled| {
                    Message::Automation(AutomationMsg::ToggleWatchRuleDlc(index, enabled))
                }),
            theme::check(rule.import_txt)
                .label(".txt")
                .on_toggle(move |enabled| {
                    Message::Automation(AutomationMsg::ToggleWatchRuleTxt(index, enabled))
                }),
        ]
        .spacing(12),
        row![
            text(l.get("category")).width(120),
            theme::field(&l.get("optional-category"), &rule.category).on_input(move |val| {
                Message::Automation(AutomationMsg::WatchRuleCategoryChanged(index, val))
            }),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        behaviors,
    ]
    .spacing(6);
    container(card)
        .padding(10)
        .width(Fill)
        .style(theme::panel)
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
        rule::horizontal(1).style(theme::separator),
        text(l.get("lan-title")).size(18),
        text(if !app.preferences.lan_enabled {
            l.get("lan-status-disabled")
        } else {
            match &app.lan_status {
                ruscello_api::LanStatus::Active { display_url, .. } => {
                    l.with("lan-status-active", [("url", display_url.clone())])
                }
                ruscello_api::LanStatus::Error(error) => {
                    l.with("lan-status-error", [("error", error.clone())])
                }
                ruscello_api::LanStatus::Disabled => l.get("lan-status-disabled"),
            }
        }),
        theme::check(app.preferences.lan_enabled)
            .label(l.get("lan-enabled"))
            .on_toggle(Message::LanEnabled),
        number(app, Number::Port),
        theme::check(app.preferences.lan_auth)
            .label(l.get("lan-auth"))
            .on_toggle(Message::LanAuth),
        save(app),
    ]
    .spacing(8);
    if let Some(control) = app.api_control.as_ref() {
        controls = controls.push(
            text(l.with(
                "lan-local-api",
                [("port", control.local_addr().port().to_string())],
            ))
            .size(12)
            .style(muted),
        );
    }
    if app.preferences.lan_auth {
        if app.lan_password_hash.is_none() {
            controls = controls.push(text(l.get("lan-password-missing")).size(12).style(muted));
        }
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
        controls = controls.push(
            row![
                canvas(Qr(code)).width(150).height(150),
                column![
                    theme::action(text(url)).on_press(Message::CopyLanUrl),
                    row![
                        theme::action(text(l.get("lan-copy-url"))).on_press(Message::CopyLanUrl),
                        theme::action(text(l.get("lan-open-url"))).on_press(Message::OpenLanUrl),
                    ]
                    .spacing(8),
                    text(l.get("lan-qr-caption")).size(12).style(muted),
                ]
                .spacing(8),
            ]
            .align_y(Alignment::Center)
            .spacing(12),
        );
    }
    let mut controls = controls
        .push(rule::horizontal(1).style(theme::separator))
        .push(sidecars(app))
        .push(rule::horizontal(1).style(theme::separator))
        .push(text(l.get("link-imports-title")).size(18))
        .push(text(l.get("extension-pair-detail")).size(12).style(muted))
        .push(text(l.with(
            "extension-paired-count",
            [("count", app.pairing.paired_count.to_string())],
        )));
    let mut pair_actions = row![if app.pairing.window_active {
        theme::action(text(l.get("extension-pair-cancel"))).on_press(Message::CancelPairing)
    } else {
        theme::action(text(l.get("extension-pair-start")))
            .on_press_maybe(app.api_control.as_ref().map(|_| Message::StartPairing))
    }]
    .spacing(8);
    if app.pairing.paired_count > 0 && app.api_control.is_some() {
        pair_actions = pair_actions.push(
            theme::action(text(l.get("extension-pair-revoke"))).on_press(Message::RevokePairing),
        );
    }
    controls = controls.push(pair_actions);
    if app.pairing.window_active
        && let Some(pin) = app.pairing.pin.as_ref()
    {
        controls = controls.push(text(l.with(
            "extension-pair-active",
            [
                ("seconds", app.pairing.remaining_secs.to_string()),
                ("pin", pin.clone()),
            ],
        )));
    }
    for req in &app.pairing.pending {
        controls = controls.push(
            row![
                text(l.with(
                    "extension-pair-pending",
                    [
                        ("client", req.client_name.clone()),
                        ("origin", req.origin.clone()),
                    ],
                )),
                theme::action(text(l.get("extension-pair-approve")))
                    .on_press(Message::ApprovePairing(req.request_id.clone())),
                theme::action(text(l.get("extension-pair-reject")))
                    .on_press(Message::RejectPairing(req.request_id.clone())),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        );
    }
    controls
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
