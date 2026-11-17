//! The user's language from the environment.

/// `"de_DE.UTF-8@euro"` → `"de_DE"`; empty and POSIX locales give `None`.
pub fn normalize_language(locale: &str) -> Option<String> {
    let base = locale.split(['.', '@']).next().unwrap_or_default().trim();
    match base {
        "" | "C" | "POSIX" => None,
        other => Some(other.to_owned()),
    }
}

/// The first language from `LANGUAGE`, `LC_ALL`, `LC_MESSAGES` or `LANG`. `LANGUAGE=C` (how
/// `I18n.set-language("")` selects the untranslated texts) means no language, as for gettext.
pub fn current_language() -> Option<String> {
    language_from(|name| std::env::var(name).ok())
}

fn language_from(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(list) = var("LANGUAGE")
        && let Some(first) = list.split(':').map(str::trim).find(|entry| !entry.is_empty())
    {
        return normalize_language(first);
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| var(name).and_then(|v| normalize_language(&v)))
}

/// Languages by their own name ("endonym"), for language pickers. Region-specific entries come
/// before the plain language.
const ENDONYMS: &[(&str, &str)] = &[
    ("zh_TW", "繁體中文"),
    ("zh_HK", "繁體中文（香港）"),
    ("zh_Hant", "繁體中文"),
    ("zh_CN", "简体中文"),
    ("zh_SG", "简体中文（新加坡）"),
    ("zh_Hans", "简体中文"),
    ("zh", "中文"),
    ("pt_BR", "Português (Brasil)"),
    ("en_GB", "English (UK)"),
    ("en", "English"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("it", "Italiano"),
    ("pt", "Português"),
    ("ru", "Русский"),
    ("uk", "Українська"),
    ("pl", "Polski"),
    ("cs", "Čeština"),
    ("nl", "Nederlands"),
    ("sv", "Svenska"),
    ("da", "Dansk"),
    ("nb", "Norsk bokmål"),
    ("fi", "Suomi"),
    ("el", "Ελληνικά"),
    ("hu", "Magyar"),
    ("ro", "Română"),
    ("tr", "Türkçe"),
    ("ar", "العربية"),
    ("he", "עברית"),
    ("fa", "فارسی"),
    ("hi", "हिन्दी"),
    ("th", "ไทย"),
    ("vi", "Tiếng Việt"),
    ("id", "Bahasa Indonesia"),
];

/// The name of a language in that language, e.g. "zh_TW" → "繁體中文", "de_AT" → "Deutsch";
/// unknown codes are returned unchanged, and "" is English (untranslated texts).
pub fn language_name(code: &str) -> String {
    let code = normalize_language(code).unwrap_or_else(|| "en".to_owned());
    let base = code.split(['_', '-']).next().unwrap_or_default();
    ENDONYMS
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(&code.replace('-', "_")))
        .or_else(|| ENDONYMS.iter().find(|(c, _)| c.eq_ignore_ascii_case(base)))
        .map_or(code.clone(), |(_, name)| (*name).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_languages() {
        assert_eq!(language_name("zh_TW"), "繁體中文");
        assert_eq!(language_name("zh_TW.UTF-8"), "繁體中文");
        assert_eq!(language_name("zh-CN"), "简体中文");
        assert_eq!(language_name("de_AT"), "Deutsch");
        assert_eq!(language_name(""), "English");
        assert_eq!(language_name("tlh"), "tlh");
    }

    #[test]
    fn normalizes_locales() {
        assert_eq!(normalize_language("de_DE.UTF-8@euro").as_deref(), Some("de_DE"));
        assert_eq!(normalize_language("zh_TW.UTF-8").as_deref(), Some("zh_TW"));
        assert_eq!(normalize_language("C.UTF-8"), None);
        assert_eq!(normalize_language("POSIX"), None);
    }

    #[test]
    fn language_precedence() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
        };
        assert_eq!(
            language_from(env(&[("LANGUAGE", "fr:de"), ("LANG", "en_US.UTF-8")])).as_deref(),
            Some("fr")
        );
        assert_eq!(
            language_from(env(&[("LC_ALL", "ja_JP.UTF-8"), ("LANG", "en_US")])).as_deref(),
            Some("ja_JP")
        );
        assert_eq!(language_from(env(&[("LANG", "C")])), None);
        // The untranslated texts chosen at runtime win over the system language.
        assert_eq!(language_from(env(&[("LANGUAGE", "C"), ("LANG", "zh_TW.UTF-8")])), None);
        assert_eq!(
            language_from(env(&[("LANGUAGE", ""), ("LANG", "zh_TW.UTF-8")])).as_deref(),
            Some("zh_TW")
        );
    }
}
