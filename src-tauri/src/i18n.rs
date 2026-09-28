//! Localized strings for the Rust side (notifications, dialogs, the tray
//! menu, error messages). The tables are the same `src/locales/*.json` files
//! the frontend uses, built from the Mac app's String Catalog by
//! `scripts/extract_locales.py`, so both sides always agree.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// Languages shipped with the app, in the same order as getaktar.com. Names
/// are native so they're recognizable whatever the UI language is.
pub const SUPPORTED: &[(&str, &str)] = &[
    ("en", "English"),
    ("tr", "Türkçe"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("pt-BR", "Português (Brasil)"),
    ("ja", "日本語"),
    ("zh-Hans", "简体中文"),
];

type Table = HashMap<String, String>;

fn tables() -> &'static HashMap<&'static str, Table> {
    static TABLES: OnceLock<HashMap<&'static str, Table>> = OnceLock::new();
    TABLES.get_or_init(|| {
        let sources: [(&str, &str); 8] = [
            ("en", include_str!("../../src/locales/en.json")),
            ("tr", include_str!("../../src/locales/tr.json")),
            ("de", include_str!("../../src/locales/de.json")),
            ("fr", include_str!("../../src/locales/fr.json")),
            ("es", include_str!("../../src/locales/es.json")),
            ("pt-BR", include_str!("../../src/locales/pt-BR.json")),
            ("ja", include_str!("../../src/locales/ja.json")),
            ("zh-Hans", include_str!("../../src/locales/zh-Hans.json")),
        ];
        sources
            .into_iter()
            .map(|(code, json)| (code, serde_json::from_str(json).unwrap_or_default()))
            .collect()
    })
}

fn current_lock() -> &'static RwLock<String> {
    static CURRENT: OnceLock<RwLock<String>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new("en".to_string()))
}

/// The language the app is showing right now.
pub fn current() -> String {
    current_lock().read().map(|code| code.clone()).unwrap_or_else(|_| "en".into())
}

/// Switches to the override if there is one, otherwise to the best match
/// for the Windows display language. Returns the code now in effect.
pub fn apply(language_override: Option<&str>) -> String {
    let code = language_override
        .filter(|code| SUPPORTED.iter().any(|(supported, _)| supported == code))
        .map(str::to_string)
        .unwrap_or_else(system_language);
    if let Ok(mut current) = current_lock().write() {
        *current = code.clone();
    }
    code
}

/// Maps the user's preferred Windows languages onto a shipped localization,
/// taking the first one that matches, and English if none does.
pub fn system_language() -> String {
    for locale in sys_locale::get_locales() {
        if let Some(code) = match_locale(&locale) {
            return code.to_string();
        }
    }
    "en".to_string()
}

fn match_locale(locale: &str) -> Option<&'static str> {
    let lower = locale.to_ascii_lowercase().replace('_', "-");
    let language = lower.split('-').next().unwrap_or_default();
    match language {
        "en" => Some("en"),
        "tr" => Some("tr"),
        "de" => Some("de"),
        "fr" => Some("fr"),
        "es" => Some("es"),
        "pt" => Some("pt-BR"),
        "ja" => Some("ja"),
        "zh" => Some("zh-Hans"),
        _ => None,
    }
}

/// Looks `key` up in the current language, falling back to the English
/// source text, and fills `{0}`, `{1}`... with `args`.
pub fn tr(key: &str, args: &[&str]) -> String {
    let code = current();
    let text = tables()
        .get(code.as_str())
        .and_then(|table| table.get(key))
        .map(String::as_str)
        .unwrap_or(key);
    let mut result = text.to_string();
    for (index, arg) in args.iter().enumerate() {
        result = result.replace(&format!("{{{index}}}"), arg);
    }
    result
}

/// `t!("Uploaded")`, or `t!("Delete {0} files?", count)` with arguments.
#[macro_export]
macro_rules! t {
    ($key:expr) => {
        $crate::i18n::tr($key, &[])
    };
    ($key:expr, $($arg:expr),+ $(,)?) => {
        $crate::i18n::tr($key, &[$(&$arg.to_string()),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_regional_variants() {
        assert_eq!(match_locale("tr-TR"), Some("tr"));
        assert_eq!(match_locale("pt-PT"), Some("pt-BR"));
        assert_eq!(match_locale("zh-Hans-CN"), Some("zh-Hans"));
        assert_eq!(match_locale("en_GB"), Some("en"));
        assert_eq!(match_locale("nl-NL"), None);
    }

    #[test]
    fn fills_placeholders() {
        apply(Some("en"));
        assert_eq!(tr("Delete {0} files?", &["3"]), "Delete 3 files?");
        assert_eq!(tr("not a key {0}", &["x"]), "not a key x");
    }
}
