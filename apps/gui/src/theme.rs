use iced::{
    Color, Theme, color,
    theme::Palette,
    widget::{button, container, progress_bar},
};

use crate::preferences::Appearance;

pub(crate) fn theme(appearance: Appearance) -> Option<Theme> {
    Some(match appearance {
        Appearance::System => return None,
        Appearance::Light => Theme::custom(
            "Ruscello Light",
            Palette {
                background: color!(0xf6f5f3),
                text: color!(0x242522),
                primary: color!(0x006964),
                success: color!(0x257442),
                warning: color!(0x8b5800),
                danger: color!(0xb22935),
            },
        ),
        Appearance::Dark => Theme::custom(
            "Ruscello Dark",
            Palette {
                background: color!(0x242523),
                text: color!(0xf1f0ee),
                primary: color!(0x79d0c7),
                success: color!(0x75cf98),
                warning: color!(0xf0bb65),
                danger: color!(0xff9c9c),
            },
        ),
    })
}

pub(crate) fn panel(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style::default()
        .background(palette.background.weak.color)
        .color(palette.background.weak.text)
        .border(iced::border::rounded(8))
}

pub(crate) fn selection(theme: &Theme, state: button::Status, selected: bool) -> button::Style {
    let style = if selected {
        button::secondary(theme, state)
    } else {
        button::text(theme, state)
    };
    button::Style {
        border: iced::border::rounded(6),
        ..style
    }
}

pub(crate) fn progress(theme: &Theme, status: ruscello_core::Status) -> progress_bar::Style {
    use ruscello_core::Status;
    let palette = theme.palette();
    let color: Color = match status {
        Status::Completed => palette.success,
        Status::WaitingUser => palette.warning,
        Status::Failed { .. } => palette.danger,
        Status::Paused | Status::Queued => theme.extended_palette().background.strong.color,
        _ => palette.primary,
    };
    progress_bar::Style {
        background: theme.extended_palette().background.weak.color.into(),
        bar: color.into(),
        border: iced::border::rounded(4),
    }
}
