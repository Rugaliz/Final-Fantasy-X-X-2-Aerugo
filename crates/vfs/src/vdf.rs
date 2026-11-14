//! Minimal reader for Valve's text KeyValues ("VDF") files, as used by Steam's
//! `libraryfolders.vdf` and `appmanifest_*.acf`.
//!
//! Supports quoted and unquoted tokens, `{ }` nesting, `//` comments and the usual
//! backslash escapes. Conditional tags like `[$WIN32]` are skipped.

use std::fmt;

/// A KeyValues value: either a string or a nested list of key/value pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Case-insensitive lookup of a child key (first match).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(items) => items.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Value::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    /// Children of an object (empty for strings).
    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Obj(items) => items,
            Value::Str(_) => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VDF parse error at line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    line: usize,
}

impl Lexer<'_> {
    fn err(&self, message: &'static str) -> ParseError {
        ParseError { line: self.line, message }
    }

    fn next_token(&mut self) -> Result<Option<Token>, ParseError> {
        loop {
            let Some(&c) = self.chars.peek() else { return Ok(None) };
            match c {
                '\n' => {
                    self.line += 1;
                    self.chars.next();
                }
                c if c.is_whitespace() => {
                    self.chars.next();
                }
                '/' => {
                    self.chars.next();
                    if self.chars.peek() == Some(&'/') {
                        for c in self.chars.by_ref() {
                            if c == '\n' {
                                self.line += 1;
                                break;
                            }
                        }
                    } else {
                        return Ok(Some(Token::Str(self.unquoted(String::from("/")))));
                    }
                }
                '{' => {
                    self.chars.next();
                    return Ok(Some(Token::Open));
                }
                '}' => {
                    self.chars.next();
                    return Ok(Some(Token::Close));
                }
                '[' => {
                    // conditional tag such as [$WIN32]: ignore
                    for c in self.chars.by_ref() {
                        if c == ']' {
                            break;
                        }
                    }
                }
                '"' => {
                    self.chars.next();
                    let mut s = String::new();
                    loop {
                        match self.chars.next() {
                            None => return Err(self.err("unterminated string")),
                            Some('"') => break,
                            Some('\\') => match self.chars.next() {
                                Some('n') => s.push('\n'),
                                Some('t') => s.push('\t'),
                                Some(other) => s.push(other),
                                None => return Err(self.err("unterminated escape")),
                            },
                            Some('\n') => {
                                self.line += 1;
                                s.push('\n');
                            }
                            Some(other) => s.push(other),
                        }
                    }
                    return Ok(Some(Token::Str(s)));
                }
                _ => return Ok(Some(Token::Str(self.unquoted(String::new())))),
            }
        }
    }

    fn unquoted(&mut self, mut s: String) -> String {
        while let Some(&c) = self.chars.peek() {
            if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                break;
            }
            s.push(c);
            self.chars.next();
        }
        s
    }
}

/// Parse a KeyValues document. The result is an object holding the top-level pairs.
pub fn parse(text: &str) -> Result<Value, ParseError> {
    let mut lexer = Lexer { chars: text.chars().peekable(), line: 1 };
    let items = parse_items(&mut lexer, false)?;
    Ok(Value::Obj(items))
}

fn parse_items(lexer: &mut Lexer<'_>, nested: bool) -> Result<Vec<(String, Value)>, ParseError> {
    let mut items = Vec::new();
    loop {
        let key = match lexer.next_token()? {
            None if nested => return Err(lexer.err("missing closing brace")),
            None => return Ok(items),
            Some(Token::Close) if nested => return Ok(items),
            Some(Token::Close) => return Err(lexer.err("unexpected closing brace")),
            Some(Token::Open) => return Err(lexer.err("unexpected opening brace")),
            Some(Token::Str(k)) => k,
        };
        let value = match lexer.next_token()? {
            Some(Token::Str(v)) => Value::Str(v),
            Some(Token::Open) => Value::Obj(parse_items(lexer, true)?),
            Some(Token::Close) | None => return Err(lexer.err("key without value")),
        };
        items.push((key, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY: &str = r#"
"libraryfolders"
{
    "0"
    {
        "path"      "/home/user/.local/share/Steam"
        "apps"
        {
            "228980"    "123"
        }
    }
    // second library
    "1"
    {
        "path"      "D:\\SteamLibrary"
        "apps"      { "359870" "456" }
    }
}
"#;

    #[test]
    fn parses_library_folders() {
        let v = parse(LIBRARY).unwrap();
        let root = v.get("LibraryFolders").unwrap();
        let paths: Vec<_> = root.entries().iter().filter_map(|(_, lib)| lib.get("path")?.as_str()).collect();
        assert_eq!(paths, ["/home/user/.local/share/Steam", "D:\\SteamLibrary"]);
        assert!(root.get("1").unwrap().get("apps").unwrap().get("359870").is_some());
    }

    #[test]
    fn rejects_unbalanced_input() {
        assert!(parse("\"a\" { \"b\" \"c\"").is_err());
        assert!(parse("}").is_err());
        assert!(parse("\"key\"").is_err());
    }

    proptest::proptest! {
        #[test]
        fn never_panics(s in "\\PC*") {
            let _ = parse(&s);
        }
    }
}
