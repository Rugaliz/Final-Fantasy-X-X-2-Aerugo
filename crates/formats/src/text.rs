//! The games' text encoding (see `docs/formats/text.md`).
//!
//! Strings are byte streams ending in 0x00. Characters are indexes into a per-language character list (the
//! charset file); bytes below 0x30 are line breaks, page prefixes and control codes.

use std::collections::HashMap;
use std::fmt::Write as _;

use aerugo_vfs::Language;

use crate::Game;

/// Characters reachable with one byte (0x30..=0xFF).
const PAGE: u16 = 0xD0;
/// First byte of a two-byte character; the page number is `lead - 0x2B` (so 0x2C is page 1).
const PAGE_LEAD_FIRST: u8 = 0x2C;
const PAGE_LEAD_LAST: u8 = 0x2F;
/// Prefix that moves the following character up by five pages (Chinese and Korean need more than 1040).
const EXTENDED: u8 = 0x04;
const EXTENDED_OFFSET: u16 = 5 * PAGE;
/// Largest index the encoding can express.
pub const MAX_INDEX: u16 = 2 * EXTENDED_OFFSET - 1;

pub const LINE_BREAK: u8 = 0x03;

/// One decoded element of a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Char(char),
    LineBreak,
    /// Control code. Codes 0x06..=0x2B carry one argument byte; 0x01, 0x02 and 0x05 carry none.
    Control {
        code: u8,
        arg: Option<u8>,
    },
    /// A character index past the end of the charset.
    Unmapped(u16),
    /// A multi-byte sequence that does not form a character (present in a few shipped strings; the game shows
    /// garbage for it). Kept byte for byte.
    Malformed {
        bytes: [u8; 3],
        len: u8,
    },
    /// The string ended inside a multi-byte sequence.
    Truncated,
}

/// The character list of one language.
#[derive(Debug, Clone, Default)]
pub struct Charset {
    chars: Vec<char>,
    index: HashMap<char, u16>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CharsetError {
    #[error("charset is not valid UTF-8")]
    NotUtf8,
}

impl Charset {
    /// Parse a charset file: UTF-8 text, one code point per character index. Trailing NULs are ignored.
    /// Lists may hold a few more characters than the encoding can reach; those are kept but never decoded.
    pub fn parse(bytes: &[u8]) -> Result<Charset, CharsetError> {
        let text = std::str::from_utf8(bytes).map_err(|_| CharsetError::NotUtf8)?;
        Ok(Self::from_chars(text.trim_end_matches('\0').chars()))
    }

    pub fn from_chars(chars: impl IntoIterator<Item = char>) -> Charset {
        let chars: Vec<char> = chars.into_iter().collect();
        let mut index = HashMap::with_capacity(chars.len());
        for (i, &c) in chars.iter().enumerate() {
            // Some lists repeat a character; the first index is the one used for encoding.
            if i <= MAX_INDEX as usize {
                index.entry(c).or_insert(i as u16);
            }
        }
        Charset { chars, index }
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn get(&self, index: u16) -> Option<char> {
        self.chars.get(index as usize).copied()
    }

    pub fn index_of(&self, c: char) -> Option<u16> {
        self.index.get(&c).copied()
    }
}

/// Which charset file a language uses.
pub fn charset_code(lang: Language) -> &'static str {
    match lang {
        Language::Japanese => "jp",
        Language::Korean => "kr",
        Language::ChineseTraditional => "ch",
        Language::ChineseSimplified => "cn",
        Language::English | Language::German | Language::French | Language::Italian | Language::Spanish => {
            "us"
        }
    }
}

/// Language of the folder a localized file was found in (`…/master/new_uspc/…` is English, `…/master/jppc/…`
/// Japanese). A file that fell back to another language's folder must be decoded with that language's charset.
pub fn folder_language(path: &str) -> Option<Language> {
    let rest = &path[path.find("/master/")? + "/master/".len()..];
    let folder = rest.split('/').next()?;
    let code = folder.strip_prefix("new_").unwrap_or(folder).strip_suffix("pc")?;
    Language::from_code(code)
}

/// Archive path of the charset file for a game and language.
pub fn charset_path(game: Game, lang: Language) -> String {
    let code = charset_code(lang);
    match game {
        Game::Ffx => format!("ffx_ps2/ffx/master/jppc/ffx_encoding/ffxsjistbl_{code}.bin"),
        Game::Ffx2 => format!("ffx_ps2/ffx2/master/jppc/ffx2_encoding/ffx2sjistbl_{code}.bin"),
    }
}

/// The bytes of the string starting at `start`, up to (not including) its terminating NUL or the end of `data`.
pub fn string_at(data: &[u8], start: usize) -> Option<&[u8]> {
    let rest = data.get(start..)?;
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Decode one string. Stops at the first NUL.
///
/// Byte lengths follow the game: an extension prefix followed by any byte below 0x30 takes three bytes, and a
/// page lead takes two. Sequences that do not form a character become [`Token::Malformed`].
pub fn decode(bytes: &[u8], charset: &Charset) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    let char_token = |index: u16| charset.get(index).map_or(Token::Unmapped(index), Token::Char);
    // Byte at `at`, treating NUL like the end of the string.
    let at = |k: usize| bytes.get(k).copied().filter(|&b| b != 0);
    while let Some(b) = at(i) {
        let (token, len) = match b {
            LINE_BREAK => (Token::LineBreak, 1),
            0x01 | 0x02 | 0x05 => (Token::Control { code: b, arg: None }, 1),
            EXTENDED => match (at(i + 1), at(i + 2)) {
                (Some(lead @ PAGE_LEAD_FIRST..=PAGE_LEAD_LAST), Some(second)) if second >= 0x30 => {
                    (char_token(EXTENDED_OFFSET + paged(lead, second)), 3)
                }
                (Some(c), _) if c >= 0x30 => (char_token(EXTENDED_OFFSET + u16::from(c - 0x30)), 2),
                (Some(_), Some(_)) => {
                    (Token::Malformed { bytes: [b, bytes[i + 1], bytes[i + 2]], len: 3 }, 3)
                }
                _ => (Token::Truncated, 0),
            },
            0x06..=0x2B => match at(i + 1) {
                Some(arg) => (Token::Control { code: b, arg: Some(arg) }, 2),
                None => (Token::Truncated, 0),
            },
            PAGE_LEAD_FIRST..=PAGE_LEAD_LAST => match at(i + 1) {
                Some(second) if second >= 0x30 => (char_token(paged(b, second)), 2),
                Some(second) => (Token::Malformed { bytes: [b, second, 0], len: 2 }, 2),
                None => (Token::Truncated, 0),
            },
            _ => (char_token(u16::from(b - 0x30)), 1),
        };
        out.push(token);
        if len == 0 {
            break;
        }
        i += len;
    }
    out
}

fn paged(lead: u8, second: u8) -> u16 {
    u16::from(lead - 0x2B) * PAGE + u16::from(second - 0x30)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EncodeError {
    #[error("character {0:?} is not in the charset")]
    MissingChar(char),
    #[error("index {0} cannot be encoded")]
    IndexOutOfRange(u16),
    #[error("control code {0:#04x} is not valid with this argument")]
    BadControl(u8),
}

/// Encode tokens (without the terminating NUL). `Truncated` tokens are skipped.
pub fn encode(tokens: &[Token], charset: &Charset) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    for &t in tokens {
        match t {
            Token::Char(c) => {
                let index = charset.index_of(c).ok_or(EncodeError::MissingChar(c))?;
                encode_index(index, &mut out)?;
            }
            Token::Unmapped(index) => encode_index(index, &mut out)?,
            Token::LineBreak => out.push(LINE_BREAK),
            Token::Control { code, arg: None } if matches!(code, 0x01 | 0x02 | 0x05) => out.push(code),
            Token::Control { code, arg: Some(arg) } if (0x06..=0x2B).contains(&code) && arg != 0 => {
                out.extend([code, arg]);
            }
            Token::Control { code, .. } => return Err(EncodeError::BadControl(code)),
            Token::Malformed { bytes, len } => out.extend_from_slice(&bytes[..usize::from(len).min(3)]),
            Token::Truncated => {}
        }
    }
    Ok(out)
}

fn encode_index(index: u16, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut rest = index;
    if rest > MAX_INDEX {
        return Err(EncodeError::IndexOutOfRange(index));
    }
    if rest >= EXTENDED_OFFSET {
        out.push(EXTENDED);
        rest -= EXTENDED_OFFSET;
    }
    let page = rest / PAGE;
    if page > 0 {
        out.push(0x2B + page as u8);
    }
    out.push(0x30 + (rest % PAGE) as u8);
    Ok(())
}

/// Human-readable form: characters as-is, line breaks as `\n`, everything else in braces (`{0a:41}`, `{?1234}`).
pub fn to_display(tokens: &[Token]) -> String {
    let mut s = String::with_capacity(tokens.len());
    for t in tokens {
        match *t {
            Token::Char(c) => s.push(c),
            Token::LineBreak => s.push('\n'),
            Token::Control { code, arg: None } => {
                let _ = write!(s, "{{{code:02x}}}");
            }
            Token::Control { code, arg: Some(a) } => {
                let _ = write!(s, "{{{code:02x}:{a:02x}}}");
            }
            Token::Unmapped(i) => {
                let _ = write!(s, "{{?{i}}}");
            }
            Token::Malformed { bytes, len } => {
                s.push_str("{!");
                for b in &bytes[..usize::from(len).min(3)] {
                    let _ = write!(s, "{b:02x}");
                }
                s.push('}');
            }
            Token::Truncated => s.push_str("{…}"),
        }
    }
    s
}

/// Decode to display form in one step.
pub fn decode_display(bytes: &[u8], charset: &Charset) -> String {
    to_display(&decode(bytes, charset))
}

/// True when every character mapped to the charset and every sequence was complete.
pub fn is_clean(tokens: &[Token]) -> bool {
    !tokens.iter().any(|t| matches!(t, Token::Unmapped(_) | Token::Malformed { .. } | Token::Truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A synthetic charset large enough to need every page and the extension prefix.
    fn big_charset() -> Charset {
        Charset::from_chars((0..=MAX_INDEX as u32).map(|i| char::from_u32(0x4E00 + i).unwrap()))
    }

    fn latin() -> Charset {
        Charset::parse("0123456789 !ABCabc".as_bytes()).unwrap()
    }

    #[test]
    fn single_byte_characters_and_controls() {
        let cs = latin();
        // "Ab", line break, colour code with argument, "1", end, then garbage that must be ignored.
        let bytes = [0x30 + 12, 0x30 + 15, 0x03, 0x0A, 0x41, 0x31, 0x00, 0x99];
        let t = decode(&bytes, &cs);
        assert_eq!(
            t,
            vec![
                Token::Char('A'),
                Token::Char('a'),
                Token::LineBreak,
                Token::Control { code: 0x0A, arg: Some(0x41) },
                Token::Char('1'),
            ]
        );
        assert_eq!(to_display(&t), "Aa\n{0a:41}1");
    }

    #[test]
    fn paged_and_extended_indexes() {
        let cs = big_charset();
        let at = |i: u16| Token::Char(cs.get(i).unwrap());
        assert_eq!(decode(&[0x2C, 0x30], &cs), vec![at(0xD0)]);
        assert_eq!(decode(&[0x2F, 0xFF], &cs), vec![at(4 * 0xD0 + 0xCF)]);
        assert_eq!(decode(&[0x04, 0x30], &cs), vec![at(1040)]);
        assert_eq!(decode(&[0x04, 0x2D, 0x31], &cs), vec![at(1040 + 2 * 0xD0 + 1)]);
    }

    #[test]
    fn indexes_past_the_charset_are_reported() {
        let cs = latin();
        let t = decode(&[0x2C, 0x30], &cs);
        assert_eq!(t, vec![Token::Unmapped(0xD0)]);
        assert!(!is_clean(&t));
        assert_eq!(decode(&[0x2C], &cs), vec![Token::Truncated]);
        assert_eq!(decode(&[0x0A], &cs), vec![Token::Truncated]);
        assert_eq!(decode(&[0x0A, 0x00, 0x31], &cs), vec![Token::Truncated]);
    }

    #[test]
    fn malformed_sequences_keep_the_game_lengths() {
        let cs = big_charset();
        let t = decode(&[0x04, 0x04, 0x54, 0x30], &cs);
        assert_eq!(
            t,
            vec![Token::Malformed { bytes: [0x04, 0x04, 0x54], len: 3 }, Token::Char(cs.get(0).unwrap())]
        );
        assert_eq!(encode(&t, &cs).unwrap(), vec![0x04, 0x04, 0x54, 0x30]);
        assert_eq!(to_display(&t[..1]), "{!040454}");
        let t = decode(&[0x2D, 0x12, 0x30], &cs);
        assert_eq!(t[0], Token::Malformed { bytes: [0x2D, 0x12, 0], len: 2 });
    }

    #[test]
    fn charset_ignores_trailing_nuls_and_rejects_bad_utf8() {
        assert_eq!(Charset::parse(b"ab\0\0").unwrap().len(), 2);
        assert_eq!(Charset::parse(&[0xFF]).unwrap_err(), CharsetError::NotUtf8);
    }

    #[test]
    fn folder_language_reads_the_master_folder() {
        assert_eq!(
            folder_language("ffx_ps2/ffx/master/new_uspc/battle/kernel/a.bin"),
            Some(Language::English)
        );
        assert_eq!(folder_language("ffx_ps2/ffx/master/jppc/battle/kernel/a.bin"), Some(Language::Japanese));
        assert_eq!(folder_language("ffx_ps2/ffx/master/new_sppc/x"), Some(Language::Spanish));
        assert_eq!(folder_language("ffx_ps2/ffx/master/inpc/x"), None);
        assert_eq!(folder_language("ffx_data/x"), None);
    }

    #[test]
    fn every_index_round_trips() {
        let cs = big_charset();
        for i in 0..=MAX_INDEX {
            let mut bytes = Vec::new();
            encode_index(i, &mut bytes).unwrap();
            assert_eq!(decode(&bytes, &cs), vec![Token::Char(cs.get(i).unwrap())], "index {i}");
        }
    }

    proptest! {
        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let _ = decode(&bytes, &latin());
        }

        #[test]
        fn encode_then_decode_is_identity(indexes in proptest::collection::vec(0..=MAX_INDEX, 0..32),
                                          controls in proptest::collection::vec((0x06u8..=0x2B, 1u8..=255), 0..4)) {
            let cs = big_charset();
            let mut tokens: Vec<Token> = indexes.iter().map(|&i| Token::Char(cs.get(i).unwrap())).collect();
            for (n, (code, arg)) in controls.into_iter().enumerate() {
                tokens.insert(n.min(tokens.len()), Token::Control { code, arg: Some(arg) });
            }
            tokens.push(Token::LineBreak);
            let bytes = encode(&tokens, &cs).unwrap();
            prop_assert_eq!(decode(&bytes, &cs), tokens);
        }
    }
}
