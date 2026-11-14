use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("../.."))
}

#[test]
fn browser_extension_manifests_icons_and_locales_are_valid_and_in_sync()
-> Result<(), Box<dyn std::error::Error>> {
    let ext_dir = workspace_root().join("extensions").join("browser");
    let required_perms: BTreeSet<&str> = [
        "contextMenus",
        "downloads",
        "cookies",
        "storage",
        "activeTab",
    ]
    .into_iter()
    .collect();

    for manifest_name in ["manifest.json", "manifest.firefox.json"] {
        let path = ext_dir.join(manifest_name);
        let raw = fs::read_to_string(&path)?;
        let json: Value = serde_json::from_str(&raw)?;
        assert_eq!(
            json["manifest_version"].as_u64(),
            Some(3),
            "{manifest_name}"
        );
        assert_eq!(
            json["default_locale"].as_str(),
            Some("en"),
            "{manifest_name}"
        );

        let perms: BTreeSet<&str> = json["permissions"]
            .as_array()
            .ok_or("missing permissions array")?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for perm in &required_perms {
            assert!(perms.contains(perm), "{manifest_name} missing {perm}");
        }

        let popup = json["action"]["default_popup"]
            .as_str()
            .ok_or("missing default_popup")?;
        assert!(ext_dir.join(popup).is_file(), "missing {popup}");

        let options = json["options_ui"]["page"]
            .as_str()
            .ok_or("missing options_ui.page")?;
        assert!(ext_dir.join(options).is_file(), "missing {options}");

        if let Some(icons) = json["icons"].as_object() {
            for icon_path in icons.values().filter_map(Value::as_str) {
                assert!(ext_dir.join(icon_path).is_file(), "missing {icon_path}");
            }
        }
        if let Some(worker) = json["background"]["service_worker"].as_str() {
            assert!(ext_dir.join(worker).is_file(), "missing {worker}");
        }
        if let Some(scripts) = json["background"]["scripts"].as_array() {
            for script in scripts.iter().filter_map(Value::as_str) {
                assert!(ext_dir.join(script).is_file(), "missing {script}");
            }
        }
    }

    for extra_asset in ["i18n.js", "popup.js", "options.js", "styles.css"] {
        assert!(ext_dir.join(extra_asset).is_file(), "missing {extra_asset}");
    }

    let en_raw = fs::read_to_string(ext_dir.join("_locales/en/messages.json"))?;
    let it_raw = fs::read_to_string(ext_dir.join("_locales/it/messages.json"))?;
    let en: Value = serde_json::from_str(&en_raw)?;
    let it: Value = serde_json::from_str(&it_raw)?;

    let en_map = en.as_object().ok_or("en messages must be object")?;
    let it_map = it.as_object().ok_or("it messages must be object")?;
    let en_keys: BTreeSet<&str> = en_map.keys().map(String::as_str).collect();
    let it_keys: BTreeSet<&str> = it_map.keys().map(String::as_str).collect();
    assert_eq!(en_keys, it_keys, "extension _locales keys out of sync");

    for (key, value) in en_map {
        let msg = value["message"].as_str().unwrap_or_default().trim();
        assert!(!msg.is_empty(), "empty English message for {key}");
    }
    for (key, value) in it_map {
        let msg = value["message"].as_str().unwrap_or_default().trim();
        assert!(!msg.is_empty(), "empty Italian message for {key}");
    }

    Ok(())
}

#[test]
fn embedded_webui_assets_exist_and_include_english_and_italian()
-> Result<(), Box<dyn std::error::Error>> {
    let webui_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/webui");
    let html = fs::read_to_string(webui_dir.join("index.html"))?;
    let css = fs::read_to_string(webui_dir.join("style.css"))?;
    let js = fs::read_to_string(webui_dir.join("app.js"))?;

    assert!(html.contains("/style.css"));
    assert!(html.contains("/app.js"));
    assert!(!css.trim().is_empty());
    assert!(js.contains("en:"));
    assert!(js.contains("it:"));
    Ok(())
}
