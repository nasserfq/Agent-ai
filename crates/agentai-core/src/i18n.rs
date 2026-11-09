//! Interface languages. Strings are written in English in the code; each language's catalog
//! (`locales/<code>.tsv`, one `English<TAB>translation` per line) translates them. A string
//! missing from a catalog shows in English.

use std::collections::HashMap;
use std::sync::OnceLock;

pub struct Lang {
    pub code: &'static str,
    /// The language's own name.
    pub native: &'static str,
    /// The English name (for the model's system prompt).
    pub english: &'static str,
    pub rtl: bool,
}

/// Every interface language, Arabic first.
pub const LANGS: &[Lang] = &[
    Lang { code: "ar", native: "العربية", english: "Arabic", rtl: true },
    Lang { code: "en", native: "English", english: "English", rtl: false },
    Lang { code: "fr", native: "Français", english: "French", rtl: false },
    Lang { code: "es", native: "Español", english: "Spanish", rtl: false },
    Lang { code: "de", native: "Deutsch", english: "German", rtl: false },
    Lang { code: "pt", native: "Português", english: "Portuguese", rtl: false },
    Lang { code: "tr", native: "Türkçe", english: "Turkish", rtl: false },
    Lang { code: "id", native: "Bahasa Indonesia", english: "Indonesian", rtl: false },
    Lang { code: "ur", native: "اردو", english: "Urdu", rtl: true },
];

const CATALOGS: &[(&str, &str)] = &[
    ("ar", include_str!("../../../locales/ar.tsv")),
    ("fr", include_str!("../../../locales/fr.tsv")),
    ("es", include_str!("../../../locales/es.tsv")),
    ("de", include_str!("../../../locales/de.tsv")),
    ("pt", include_str!("../../../locales/pt.tsv")),
    ("tr", include_str!("../../../locales/tr.tsv")),
    ("id", include_str!("../../../locales/id.tsv")),
    ("ur", include_str!("../../../locales/ur.tsv")),
];

type Catalogs = HashMap<&'static str, HashMap<String, String>>;

fn catalogs() -> &'static Catalogs {
    static C: OnceLock<Catalogs> = OnceLock::new();
    C.get_or_init(|| {
        CATALOGS
            .iter()
            .map(|(code, text)| {
                let mut c = parse(text);
                if lang(code).rtl {
                    for v in c.values_mut() {
                        rtl_start(v);
                    }
                }
                (*code, c)
            })
            .collect()
    })
}

/// A right-to-left translation that starts with a Latin word (`Claude، …`) would be laid out
/// left-to-right; a right-to-left mark in front keeps the paragraph right-to-left.
fn rtl_start(s: &mut String) {
    let first_strong = s.chars().find(|c| c.is_alphabetic());
    if first_strong.is_some_and(|c| c.is_ascii_alphabetic()) {
        s.insert(0, '\u{200F}');
    }
}

/// `English<TAB>translation` lines; blank lines and lines starting with `#` are skipped. `\n` in
/// either column stands for a line break.
pub fn parse(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .filter(|(_, t)| !t.trim().is_empty())
        .map(|(k, v)| (k.replace("\\n", "\n"), v.replace("\\n", "\n")))
        .collect()
}

/// The language for `code` (a code like `ar` or `ar-SA`), or English.
pub fn lang(code: &str) -> &'static Lang {
    let base = code.split(['-', '_', '.']).next().unwrap_or("").to_lowercase();
    LANGS.iter().find(|l| l.code == base).or_else(|| LANGS.get(1)).unwrap_or(&FALLBACK)
}

static FALLBACK: Lang = Lang { code: "en", native: "English", english: "English", rtl: false };

/// `english` in language `code`.
pub fn tr(code: &str, english: &'static str) -> &'static str {
    catalogs().get(lang(code).code).and_then(|c| c.get(english)).map(String::as_str).unwrap_or(english)
}

/// The language to start in: the computer's, when it is one of [`LANGS`], else Arabic.
pub fn pick(system_locales: &[String]) -> &'static str {
    for l in system_locales {
        let base = l.split(['-', '_', '.']).next().unwrap_or("").to_lowercase();
        if let Some(found) = LANGS.iter().find(|x| x.code == base) {
            return found.code;
        }
    }
    "ar"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_parses_and_covers_arabic_keys() {
        let ar = parse(CATALOGS[0].1);
        assert!(ar.len() > 50);
        for (code, text) in CATALOGS {
            let c = parse(text);
            for key in c.keys() {
                assert!(ar.contains_key(key), "{code}: {key:?} is not in the Arabic catalog (a typo in the English?)");
            }
            for l in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
                assert!(l.contains('\t'), "{code}: line without a tab: {l:?}");
            }
        }
    }

    #[test]
    fn lookup_and_fallback() {
        assert_eq!(tr("ar", "Settings"), "الإعدادات");
        assert_eq!(tr("xx", "Settings"), "Settings");
        assert_eq!(tr("ar", "no such string"), "no such string");
        assert_eq!(lang("ar-SA").code, "ar");
        assert_eq!(pick(&["fr-FR".into()]), "fr");
        assert_eq!(pick(&["ja-JP".into()]), "ar");
        assert!(tr("ar", "Add an MCP server").starts_with("إضافة"));
        assert!(tr("ur", "AI services").starts_with('\u{200F}'));
    }
}
