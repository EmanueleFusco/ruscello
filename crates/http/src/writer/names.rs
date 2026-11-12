use std::path::Path;

const MAX_NAME_BYTES: usize = 220;
const MAX_SUFFIX_BYTES: usize = 48;

/// Converts a suggested name into a portable filename without path separators or device names.
pub fn sanitize_filename(value: &str) -> String {
    let mut name: String = value
        .chars()
        .map(|character| {
            if character.is_control() || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    name = name.trim().trim_end_matches(['.', ' ']).to_owned();
    if name.is_empty() {
        return "download".into();
    }
    let device = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let port = device
        .strip_prefix("COM")
        .or_else(|| device.strip_prefix("LPT"));
    if matches!(
        device.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || port.is_some_and(|number| {
        matches!(
            number,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    }) {
        name.insert(0, '_');
    }
    if name.len() > MAX_NAME_BYTES {
        let (stem, suffix) = split_suffix(&name);
        let budget = MAX_NAME_BYTES - suffix.len();
        name = format!("{}{suffix}", &stem[..stem.floor_char_boundary(budget)]);
    }
    name
}

pub(super) fn candidate(name: &str, index: u32) -> String {
    if index == 0 {
        return name.to_owned();
    }
    let (stem, suffix) = split_suffix(name);
    format!("{stem} ({index}){suffix}")
}

fn split_suffix(name: &str) -> (&str, &str) {
    match Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some(extension) if extension.len() < MAX_SUFFIX_BYTES => {
            name.split_at(name.len() - extension.len() - 1)
        }
        _ => (name, ""),
    }
}
