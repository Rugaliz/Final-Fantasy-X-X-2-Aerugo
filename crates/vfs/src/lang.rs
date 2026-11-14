//! Language overlays.
//!
//! The archives keep localized data in two schemes:
//! - a base tree `…/master/jppc/…` with per-language replacement trees `…/master/new_<xx>pc/…` (and a few
//!   files in `…/master/<xx>pc/…`);
//! - folders with a language suffix, such as `map_2d_ui_us`.
//!
//! Callers name the base path (`…/master/jppc/…`) or use a `{lang}` placeholder for suffix folders
//! (`…/map_2d_ui_{lang}/…`); [`localized_candidates`] lists the paths to try, most specific first.

/// Text languages shipped with the remaster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    English,
    Japanese,
    German,
    French,
    Italian,
    Spanish,
    Korean,
    ChineseSimplified,
    ChineseTraditional,
}

impl Language {
    pub const ALL: [Language; 9] = [
        Language::English,
        Language::Japanese,
        Language::German,
        Language::French,
        Language::Italian,
        Language::Spanish,
        Language::Korean,
        Language::ChineseSimplified,
        Language::ChineseTraditional,
    ];

    /// Code used in `master/new_<xx>pc` folder names.
    pub fn master_code(self) -> &'static str {
        match self {
            Language::Spanish => "sp",
            other => other.code(),
        }
    }

    /// Code used in folder suffixes (`_us`, `_es`, …) and in settings.
    pub fn code(self) -> &'static str {
        match self {
            Language::English => "us",
            Language::Japanese => "jp",
            Language::German => "de",
            Language::French => "fr",
            Language::Italian => "it",
            Language::Spanish => "es",
            Language::Korean => "kr",
            Language::ChineseSimplified => "cn",
            Language::ChineseTraditional => "ch",
        }
    }

    /// Accepts either code scheme.
    pub fn from_code(code: &str) -> Option<Language> {
        let code = code.to_ascii_lowercase();
        Language::ALL.into_iter().find(|l| l.code() == code || l.master_code() == code)
    }

    pub fn name(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Japanese => "Japanese",
            Language::German => "German",
            Language::French => "French",
            Language::Italian => "Italian",
            Language::Spanish => "Spanish",
            Language::Korean => "Korean",
            Language::ChineseSimplified => "Chinese (simplified)",
            Language::ChineseTraditional => "Chinese (traditional)",
        }
    }
}

const MASTER_BASE: &str = "/master/jppc/";
const PLACEHOLDER: &str = "{lang}";

/// Paths to try for `path` in `lang`, most specific first. Paths without a localized part give themselves.
pub fn localized_candidates(path: &str, lang: Language) -> Vec<String> {
    if let Some(pos) = path.find(MASTER_BASE) {
        let (head, tail) = (&path[..pos], &path[pos + MASTER_BASE.len()..]);
        let code = lang.master_code();
        return vec![
            format!("{head}/master/new_{code}pc/{tail}"),
            format!("{head}/master/{code}pc/{tail}"),
            path.to_owned(),
        ];
    }
    if path.contains(PLACEHOLDER) {
        let mut out = vec![path.replace(PLACEHOLDER, lang.code())];
        for fallback in [Language::English, Language::Japanese] {
            if fallback != lang {
                out.push(path.replace(PLACEHOLDER, fallback.code()));
            }
        }
        return out;
    }
    vec![path.to_owned()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_tree_overlays() {
        let c = localized_candidates("ffx_ps2/ffx/master/jppc/battle/kernel/x.bin", Language::Spanish);
        assert_eq!(
            c,
            [
                "ffx_ps2/ffx/master/new_sppc/battle/kernel/x.bin",
                "ffx_ps2/ffx/master/sppc/battle/kernel/x.bin",
                "ffx_ps2/ffx/master/jppc/battle/kernel/x.bin",
            ]
        );
    }

    #[test]
    fn suffix_folders_fall_back_to_english_then_japanese() {
        let c = localized_candidates("ffx-2_data/gamedata/ps3data/map_2d_ui_{lang}/a.dds", Language::German);
        assert_eq!(c.len(), 3);
        assert!(c[0].contains("_de/") && c[1].contains("_us/") && c[2].contains("_jp/"));
        assert_eq!(localized_candidates("plain/file.bin", Language::English), ["plain/file.bin"]);
    }

    #[test]
    fn codes_round_trip() {
        for l in Language::ALL {
            assert_eq!(Language::from_code(l.code()), Some(l));
            assert_eq!(Language::from_code(l.master_code()), Some(l));
        }
        assert_eq!(Language::from_code("SP"), Some(Language::Spanish));
        assert_eq!(Language::from_code("xx"), None);
    }
}
