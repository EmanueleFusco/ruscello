use iced::{
    Font,
    widget::{Text, text},
};
use ruscello_core::FileType;

pub(crate) const FONT: &[u8] = include_bytes!("../assets/lucide.ttf");

pub(crate) fn icon(glyph: char) -> Text<'static> {
    text(glyph.to_string())
        .font(Font::with_name("lucide"))
        .size(16)
}

pub(crate) fn file_type(kind: Option<FileType>) -> char {
    match kind {
        Some(FileType::Video) => '\u{e1a5}',
        Some(FileType::Audio) => '\u{e122}',
        Some(FileType::Images) => '\u{e0f6}',
        Some(FileType::Documents) => '\u{e0cc}',
        Some(FileType::Archives) => '\u{e041}',
        Some(FileType::Programs) => '\u{e2d0}',
        Some(FileType::Torrents) => '\u{e102}',
        _ => '\u{e0c0}',
    }
}
