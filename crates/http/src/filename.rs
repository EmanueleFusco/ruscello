use std::path::Path;

use percent_encoding::percent_decode_str;
use reqwest::{
    Url,
    header::{self, HeaderMap},
};

pub(crate) fn suggest_filename(headers: &HeaderMap, url: &Url) -> String {
    if let Some(filename) = headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(disposition_filename)
    {
        return filename;
    }

    let mut filename = url
        .path_segments()
        .and_then(|mut parts| parts.next_back())
        .and_then(|part| percent_decode_str(part).decode_utf8().ok())
        .and_then(|name| basename(&name))
        .unwrap_or_else(|| "download".into());
    if Path::new(&filename).extension().is_none()
        && let Some(extension) = headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| mime_guess::get_mime_extensions_str(value.split(';').next()?.trim()))
            .and_then(|extensions| extensions.first())
    {
        filename.push('.');
        filename.push_str(extension);
    }
    filename
}

fn disposition_filename(value: &str) -> Option<String> {
    let (_, mut rest) = value.split_once(';')?;
    let mut basic = None;
    let mut extended = None;
    while !rest.is_empty() {
        let parameter = next_parameter(&mut rest);
        let Some((key, value)) = parameter.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("filename*") && extended.is_none() {
            extended = decode_extended(value).and_then(|value| basename(&value));
        } else if key.eq_ignore_ascii_case("filename") && basic.is_none() {
            basic = unquote(value).and_then(|value| basename(&value));
        }
    }
    extended.or(basic)
}

fn next_parameter<'a>(rest: &mut &'a str) -> &'a str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, char) in rest.char_indices() {
        match char {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                let (current, remaining) = rest.split_at(index);
                *rest = &remaining[1..];
                return current;
            }
            _ => {}
        }
    }
    std::mem::take(rest)
}

fn unquote(value: &str) -> Option<String> {
    let Some(value) = value.strip_prefix('"') else {
        return (!value.contains('"')).then(|| value.to_owned());
    };
    let mut chars = value.chars();
    let mut decoded = String::with_capacity(value.len());
    while let Some(char) = chars.next() {
        match char {
            '"' => return chars.as_str().trim().is_empty().then_some(decoded),
            '\\' => decoded.push(chars.next()?),
            char => decoded.push(char),
        }
    }
    None
}

fn decode_extended(value: &str) -> Option<String> {
    let (charset, remainder) = value.split_once('\'')?;
    let (_, encoded) = remainder.split_once('\'')?;
    let mut bytes = encoded.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            for _ in 0..2 {
                if !bytes.next()?.is_ascii_hexdigit() {
                    return None;
                }
            }
        } else if !byte.is_ascii_alphanumeric() && !b"!#$&+-.^_`|~".contains(&byte) {
            return None;
        }
    }
    let bytes = percent_decode_str(encoded);
    if charset.eq_ignore_ascii_case("utf-8") {
        Some(bytes.decode_utf8().ok()?.into_owned())
    } else if charset.eq_ignore_ascii_case("iso-8859-1") {
        Some(bytes.map(char::from).collect())
    } else {
        None
    }
}

fn basename(value: &str) -> Option<String> {
    let name = value.rsplit(['/', '\\']).next()?.trim();
    if name.is_empty() || matches!(name, "." | "..") || name.chars().any(char::is_control) {
        return None;
    }
    Some(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::disposition_filename;

    #[test]
    fn prefers_valid_extended_parameter_and_preserves_literal_plus() {
        assert_eq!(
            disposition_filename(
                "attachment; filename*=UTF-8'it'caff%C3%A8+menu.pdf; filename=menu.pdf"
            )
            .as_deref(),
            Some("caffè+menu.pdf")
        );
        assert_eq!(
            disposition_filename("attachment; filename*=ISO-8859-1''caf%E9.txt").as_deref(),
            Some("café.txt")
        );
    }

    #[test]
    fn accepts_quoted_semicolons_and_escapes() {
        assert_eq!(
            disposition_filename(r#"attachment; filename="notes; \"review\".txt""#).as_deref(),
            Some("notes; \"review\".txt")
        );
    }

    #[test]
    fn falls_back_from_invalid_extended_parameters() {
        for malformed in ["unknown''file", "utf-8''bad%Q0", "utf-8''%FF", "utf-8''%0A"] {
            let value = format!("attachment; filename*={malformed}; filename=safe.txt");
            assert_eq!(disposition_filename(&value).as_deref(), Some("safe.txt"));
        }
        assert_eq!(
            disposition_filename(r#"attachment; filename="unclosed"#),
            None
        );
        assert_eq!(disposition_filename("attachment; filename=.."), None);
        assert_eq!(
            disposition_filename("attachment; filename=../../safe.txt").as_deref(),
            Some("safe.txt")
        );
    }
}
