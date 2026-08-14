//! i18n: UI texts are loaded from external language files (lang/zh.yml, lang/en.yml),
//! so source code stays pure ASCII (immune to encoding/garbled-text issues in tools).
//! - t(key) looks up the loaded table; missing keys fall back to the English key itself.
//! - Log/error messages are NOT translated (kept English in code).
//! - Fallback when the language file is missing: t() returns the key (English).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// Supported languages
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Language file names inside the exe-dir "lang" folder (not translated themselves)
pub fn lang_file_name(l: Lang) -> &'static str {
    match l {
        Lang::Zh => "zh.yml",
        Lang::En => "en.yml",
    }
}

/// Set language from the settings.yml "lang" string ("en" -> En, anything else -> Zh)
pub fn set_lang(s: &str) {
    let l = if s == "en" { Lang::En } else { Lang::Zh };
    CURRENT.store(if l == Lang::En { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Zh
    }
}

static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

/// Load translations from a yml file (flat key -> value map).
/// Must be called once at startup after set_lang(); on failure the table stays empty
/// and t() falls back to the (English) key itself.
pub fn load_from_file(path: &std::path::Path) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("i18n: cannot read lang file {}: {}", path.display(), e);
            return;
        }
    };
    // tolerate a UTF-8 BOM left by editors
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(&content);
    match serde_yml::from_str::<HashMap<String, String>>(content) {
        Ok(map) => {
            let leaked: HashMap<&'static str, &'static str> = map
                .into_iter()
                .map(|(k, v)| {
                    let k: &'static str = Box::leak(k.into_boxed_str());
                    let v: &'static str = Box::leak(v.into_boxed_str());
                    (k, v)
                })
                .collect();
            log::info!("i18n: loaded {} entries from {}", leaked.len(), path.display());
            let _ = TABLE.set(leaked);
        }
        Err(e) => log::error!("i18n: parse lang file {} failed: {}", path.display(), e),
    }
}

/// Translate a UI key (English key in code) using the loaded language table.
/// Missing keys return the key itself (English), so a missing lang file degrades to English.
pub fn t(key: &'static str) -> &'static str {
    TABLE
        .get()
        .and_then(|m| m.get(key).copied())
        .unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both shipped language files must parse cleanly (a YAML syntax error silently
    /// degrades the whole UI to English keys, which happened with unquoted "xx:" values).
    #[test]
    fn shipped_lang_files_parse() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for name in ["zh.yml", "en.yml"] {
            let path = manifest_dir.join("lang").join(name);
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));
            let map: HashMap<String, String> = serde_yml::from_str(&content)
                .unwrap_or_else(|e| panic!("parse {} failed: {}", path.display(), e));
            assert!(!map.is_empty(), "{} has no entries", name);
            assert_eq!(
                map.get("new_config").map(|s| !s.is_empty()),
                Some(true),
                "{} missing new_config",
                name
            );
        }
    }

    /// t() must return the key itself when the table is empty (English fallback)
    #[test]
    fn empty_table_falls_back_to_key() {
        assert_eq!(t("some_key"), "some_key");
    }
}
