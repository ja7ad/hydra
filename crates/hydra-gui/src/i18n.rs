// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Translation, gettext-style: the msgid IS the English string.
//!
//! `tr("Add URL")` returns the active locale's translation, falling back to
//! the English text itself — a missing key can never render as an identifier.
//!
//! Catalogues are flat JSON maps. Built-in languages ship inside the binary
//! from `assets/locale/<tag>.json` (`en.json` is the identity template
//! translators copy); users can add or override with
//! `<app_dir>/locales/<tag>.json` on disk — disk entries win over built-ins
//! for the same tag.
//!
//! Persian deliberately keeps the LTR layout for now (per product decision);
//! full RTL mirroring (Arabic and friends) needs iced-level layout flipping
//! and is tracked as follow-up work.

use std::collections::HashMap;
use std::sync::RwLock;

/// Languages compiled into the binary: (tag, native display name, catalogue).
static BUILTIN: &[(&str, &str, &str)] = &[
    ("ar", "العربية", include_str!("../assets/locale/ar.json")),
    ("de", "German", include_str!("../assets/locale/de.json")),
    ("en", "English", include_str!("../assets/locale/en.json")),
    ("es", "Español", include_str!("../assets/locale/es.json")),
    ("fa", "فارسی", include_str!("../assets/locale/fa.json")),
    ("fr", "Français", include_str!("../assets/locale/fr.json")),
    ("he", "עברית", include_str!("../assets/locale/he.json")),
    ("ja", "日本語", include_str!("../assets/locale/ja.json")),
    ("ko", "한국어", include_str!("../assets/locale/ko.json")),
    ("nl", "Nederlands", include_str!("../assets/locale/nl.json")),
    (
        "pt-BR",
        "Português (Brasil)",
        include_str!("../assets/locale/pt-BR.json"),
    ),
    ("ru", "Русский", include_str!("../assets/locale/ru.json")),
    ("tr", "Türkçe", include_str!("../assets/locale/tr.json")),
    ("zh", "简体中文", include_str!("../assets/locale/zh.json")),
    (
        "zh-Hant",
        "繁體中文",
        include_str!("../assets/locale/zh-hant.json"),
    ),
];

static CATALOGUE: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// Translate one UI string. English in, active locale out.
pub fn tr(msgid: &str) -> String {
    if let Ok(guard) = CATALOGUE.read() {
        if let Some(map) = guard.as_ref() {
            if let Some(t) = map.get(msgid) {
                return t.clone();
            }
        }
    }
    msgid.to_string()
}

/// Locale tags available: built-ins plus any `<app_dir>/locales/*.json`.
pub fn available() -> Vec<String> {
    let mut tags: Vec<String> = BUILTIN.iter().map(|(t, _, _)| t.to_string()).collect();
    let dir = crate::model::app_dir().join("locales");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(tag) = name.strip_suffix(".json") {
                if !tags.iter().any(|t| t == tag) {
                    tags.push(tag.to_string());
                }
            }
        }
    }
    tags
}

/// Native display name for a tag ("fa" → "فارسی").
pub fn display_name(tag: &str) -> String {
    BUILTIN
        .iter()
        .find(|(t, _, _)| *t == tag)
        .map(|(_, n, _)| n.to_string())
        .unwrap_or_else(|| tag.to_string())
}

/// Switch locale. `"en"` (or legacy `"English"`) clears back to the built-in
/// English base; unknown/broken catalogues fall back to English too.
pub fn set_locale(tag: &str) {
    let map = if tag == "en" || tag == "English" {
        None
    } else {
        let mut merged: HashMap<String, String> = BUILTIN
            .iter()
            .find(|(t, _, _)| *t == tag)
            .and_then(|(_, _, json)| serde_json::from_str(json).ok())
            .unwrap_or_default();
        if let Ok(bytes) = std::fs::read(
            crate::model::app_dir()
                .join("locales")
                .join(format!("{tag}.json")),
        ) {
            if let Ok(disk) = serde_json::from_slice::<HashMap<String, String>>(&bytes) {
                merged.extend(disk);
            }
        }
        (!merged.is_empty()).then_some(merged)
    };
    crate::log::debug(&format!("locale -> {tag}"));
    if let Ok(mut guard) = CATALOGUE.write() {
        *guard = map;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_locales_valid_and_complete() {
        let en_raw = include_str!("../assets/locale/en.json");
        let en_map: HashMap<String, String> =
            serde_json::from_str(en_raw).expect("en.json should be valid JSON");

        for &(tag, name, raw) in BUILTIN {
            assert!(!tag.is_empty(), "tag should not be empty");
            assert!(!name.is_empty(), "display name should not be empty");
            let map: Result<HashMap<String, String>, _> = serde_json::from_str(raw);
            assert!(
                map.is_ok(),
                "locale '{tag}' failed to parse as JSON: {:?}",
                map.err()
            );
            let map = map.unwrap();
            for key in en_map.keys() {
                assert!(
                    map.contains_key(key),
                    "locale '{tag}' is missing translation for key: '{key}'"
                );
            }
        }
    }
}
