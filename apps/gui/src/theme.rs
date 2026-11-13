use std::sync::LazyLock;

use iced::{
    Border, Color, Element, Theme, color,
    theme::Palette,
    widget::{button, checkbox, container, progress_bar, scrollable, text_editor, text_input},
};

use crate::preferences::Appearance;

const CONTROL_RADIUS: u32 = 8;
const CHECK_RADIUS: u32 = 4;
pub(crate) const SCROLLBAR_GUTTER: u32 = 12;
pub(crate) const CONTROL_PADDING: [u16; 2] = [7, 11];

pub(crate) fn theme(appearance: Appearance) -> Option<Theme> {
    static LIGHT: LazyLock<Theme> = LazyLock::new(|| {
        Theme::custom(
            "Ruscello Light",
            Palette {
                background: color!(0xf1f4f9),
                text: color!(0x17243a),
                primary: color!(0x225bc5),
                success: color!(0x19724c),
                warning: color!(0x855004),
                danger: color!(0xb42942),
            },
        )
    });
    static DARK: LazyLock<Theme> = LazyLock::new(|| {
        Theme::custom(
            "Ruscello Dark",
            Palette {
                background: color!(0x101621),
                text: color!(0xe9eef8),
                primary: color!(0x85b4ff),
                success: color!(0x6cdaaa),
                warning: color!(0xf4c174),
                danger: color!(0xff9daa),
            },
        )
    });
    Some(match appearance {
        Appearance::System => return None,
        Appearance::Light => LIGHT.clone(),
        Appearance::Dark => DARK.clone(),
    })
}

#[derive(Clone, Copy)]
pub(crate) struct Surfaces {
    pub(crate) panel: Color,
    inset: Color,
    raised: Color,
    line: Color,
    pub(crate) muted: Color,
    hover: Color,
    selected: Color,
    blue: Color,
    blue_hover: Color,
}

pub(crate) fn surfaces(theme: &Theme) -> Surfaces {
    if theme.extended_palette().is_dark {
        Surfaces {
            panel: color!(0x171f2d),
            inset: color!(0x111927),
            raised: color!(0x202c40),
            line: color!(0x314059),
            muted: color!(0xabb9ce),
            hover: color!(0x25354e),
            selected: color!(0x203a60),
            blue: color!(0x2c62c9),
            blue_hover: color!(0x326ed9),
        }
    } else {
        Surfaces {
            panel: Color::WHITE,
            inset: color!(0xf4f7fc),
            raised: color!(0xecf2fb),
            line: color!(0xd3deec),
            muted: color!(0x52647d),
            hover: color!(0xe5edf9),
            selected: color!(0xe1edff),
            blue: color!(0x225bc5),
            blue_hover: color!(0x174ba9),
        }
    }
}

fn outline(color: Color) -> Border {
    iced::border::rounded(CONTROL_RADIUS).color(color).width(1)
}

pub(crate) fn panel(theme: &Theme) -> container::Style {
    let colors = surfaces(theme);
    container::Style::default()
        .background(colors.panel)
        .color(theme.palette().text)
        .border(outline(colors.line))
}

pub(crate) fn inset(theme: &Theme) -> container::Style {
    container::Style::default()
        .background(surfaces(theme).inset)
        .border(iced::border::rounded(CONTROL_RADIUS))
}

pub(crate) fn accent_tile(theme: &Theme) -> container::Style {
    container::Style::default()
        .background(surfaces(theme).selected)
        .color(theme.palette().primary)
        .border(iced::border::rounded(CONTROL_RADIUS))
}

pub(crate) fn separator(theme: &Theme) -> iced::widget::rule::Style {
    iced::widget::rule::Style {
        color: surfaces(theme).line,
        radius: 0.0.into(),
        fill_mode: iced::widget::rule::FillMode::Full,
        snap: true,
    }
}

pub(crate) fn selection(theme: &Theme, state: button::Status, selected: bool) -> button::Style {
    let colors = surfaces(theme);
    let background = match (selected, state) {
        (true, button::Status::Hovered | button::Status::Pressed) => Some(colors.hover.into()),
        (true, _) => Some(colors.selected.into()),
        (false, button::Status::Hovered | button::Status::Pressed) => Some(colors.hover.into()),
        _ => None,
    };
    button::Style {
        background,
        text_color: if selected {
            theme.palette().primary
        } else {
            theme.palette().text
        },
        border: iced::border::rounded(CONTROL_RADIUS),
        ..button::Style::default()
    }
}

pub(crate) fn selection_plain(theme: &Theme, state: button::Status) -> button::Style {
    selection(theme, state, false)
}

pub(crate) fn animated_selection(
    theme: &Theme,
    state: button::Status,
    selected: bool,
    opacity: f32,
) -> button::Style {
    let mut style = selection(theme, state, selected);
    if selected {
        style.background = style
            .background
            .map(|background| background.scale_alpha(opacity));
    }
    style
}

pub(crate) fn secondary(theme: &Theme, state: button::Status) -> button::Style {
    let colors = surfaces(theme);
    let (background, text_color) = match state {
        button::Status::Active => (colors.raised, theme.palette().text),
        button::Status::Hovered | button::Status::Pressed => {
            (colors.hover, theme.palette().primary)
        }
        button::Status::Disabled => (colors.inset, colors.muted),
    };
    button::Style {
        text_color,
        border: outline(colors.line),
        ..button::Style::default().with_background(background)
    }
}

pub(crate) fn toggle(theme: &Theme, state: button::Status, selected: bool) -> button::Style {
    if selected {
        let mut style = selection(theme, state, true);
        style.border = outline(theme.palette().primary);
        style
    } else {
        secondary(theme, state)
    }
}

pub(crate) fn primary(theme: &Theme, state: button::Status) -> button::Style {
    let colors = surfaces(theme);
    let (background, text_color) = match state {
        button::Status::Active => (colors.blue, Color::WHITE),
        button::Status::Hovered | button::Status::Pressed => (colors.blue_hover, Color::WHITE),
        button::Status::Disabled => (colors.raised, colors.muted),
    };
    button::Style {
        text_color,
        border: iced::border::rounded(CONTROL_RADIUS),
        ..button::Style::default().with_background(background)
    }
}

pub(crate) fn danger(theme: &Theme, state: button::Status) -> button::Style {
    let mut style = secondary(theme, state);
    if state != button::Status::Disabled {
        style.text_color = theme.palette().danger;
        style.border = outline(style.text_color);
    }
    style
}

pub(crate) fn action<'a, Message>(
    content: impl Into<Element<'a, Message>>,
) -> button::Button<'a, Message> {
    button(content).padding(CONTROL_PADDING).style(secondary)
}

pub(crate) fn field<'a, Message>(
    placeholder: &str,
    value: &str,
) -> text_input::TextInput<'a, Message>
where
    Message: Clone,
{
    text_input(placeholder, value)
        .padding(CONTROL_PADDING)
        .style(input)
}

pub(crate) fn check<'a, Message>(checked: bool) -> checkbox::Checkbox<'a, Message> {
    checkbox(checked).style(|theme, state| {
        let colors = surfaces(theme);
        let (background, border) = match state {
            checkbox::Status::Active { is_checked: true } => (colors.blue, colors.blue),
            checkbox::Status::Hovered { is_checked: true } => {
                (colors.blue_hover, colors.blue_hover)
            }
            checkbox::Status::Hovered { is_checked: false } => {
                (colors.hover, theme.palette().primary)
            }
            checkbox::Status::Active { is_checked: false } => (colors.inset, colors.muted),
            checkbox::Status::Disabled { .. } => (colors.raised, colors.line),
        };
        checkbox::Style {
            background: background.into(),
            icon_color: Color::WHITE,
            border: iced::border::rounded(CHECK_RADIUS).width(1).color(border),
            text_color: matches!(state, checkbox::Status::Disabled { .. }).then_some(colors.muted),
        }
    })
}

fn input(theme: &Theme, state: text_input::Status) -> text_input::Style {
    let colors = surfaces(theme);
    let border = match state {
        text_input::Status::Focused { .. } => theme.palette().primary,
        text_input::Status::Hovered => colors.muted,
        _ => colors.line,
    };
    text_input::Style {
        background: colors.inset.into(),
        border: outline(border),
        icon: colors.muted,
        placeholder: colors.muted,
        value: theme.palette().text,
        selection: colors.hover,
    }
}

pub(crate) fn editor(theme: &Theme, state: text_editor::Status) -> text_editor::Style {
    let state = match state {
        text_editor::Status::Active => text_input::Status::Active,
        text_editor::Status::Hovered => text_input::Status::Hovered,
        text_editor::Status::Focused { is_hovered } => text_input::Status::Focused { is_hovered },
        text_editor::Status::Disabled => text_input::Status::Disabled,
    };
    let style = input(theme, state);
    text_editor::Style {
        background: style.background,
        border: style.border,
        placeholder: style.placeholder,
        value: style.value,
        selection: style.selection,
    }
}

fn scrollbar() -> scrollable::Scrollbar {
    scrollable::Scrollbar::new().width(6).scroller_width(4)
}

pub(crate) fn vertical<'a, Message>(
    content: impl Into<Element<'a, Message>>,
) -> scrollable::Scrollable<'a, Message> {
    scrollable(content)
        .direction(scrollable::Direction::Vertical(scrollbar()))
        .style(scroll)
}

pub(crate) fn horizontal<'a, Message>(
    content: impl Into<Element<'a, Message>>,
) -> scrollable::Scrollable<'a, Message> {
    scrollable(content)
        .direction(scrollable::Direction::Horizontal(scrollbar()))
        .spacing(6)
        .style(scroll)
}

fn scroll(theme: &Theme, state: scrollable::Status) -> scrollable::Style {
    let mut style = scrollable::default(theme, state);
    let colors = surfaces(theme);
    for rail in [&mut style.vertical_rail, &mut style.horizontal_rail] {
        rail.background = None;
        rail.scroller.border = iced::border::rounded(3);
        rail.scroller.background = if matches!(state, scrollable::Status::Active { .. }) {
            colors.line.into()
        } else {
            colors.muted.into()
        };
    }
    style
}

pub(crate) fn progress(theme: &Theme, status: ruscello_core::Status) -> progress_bar::Style {
    let colors = surfaces(theme);
    progress_bar::Style {
        background: colors.raised.into(),
        bar: status_color(theme, status).into(),
        border: iced::border::rounded(4),
    }
}

pub(crate) fn status_color(theme: &Theme, status: ruscello_core::Status) -> Color {
    use ruscello_core::Status;
    let palette = theme.palette();
    match status {
        Status::Completed => palette.success,
        Status::WaitingUser => palette.warning,
        Status::Failed { .. } => palette.danger,
        Status::Paused | Status::Queued => surfaces(theme).muted,
        _ => palette.primary,
    }
}
