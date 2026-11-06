//! Turning `.lm` text into tokens.
//!
//! Newlines are tokens: whether one separates two items depends on where it
//! is (inside brackets, after an operator), which the parser decides.
//! Comments are not tokens; they are collected with their spans so a
//! formatter can put them back.

use crate::diagnostic::Diagnostic;
use crate::span::Span;

/// Words no name may be, unless written in backticks. They may still
/// follow `.` as a member, as in `python.module(...)`.
pub const RESERVED: [&str; 19] = [
    "module", "import", "export", "from", "as", "let", "fn", "if", "else", "for", "in", "return",
    "expect", "because", "need", "true", "false", "null", "self",
];

/// Whether `word` is reserved.
pub fn is_reserved(word: &str) -> bool {
    RESERVED.contains(&word)
}

/// What a token is. A name's text, and whether it is a keyword, is read
/// from the source by the parser, since most keywords are contextual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// A name, keyword or `_`.
    Name,
    /// A name written in backticks.
    EscapedName,
    /// An integer.
    Integer,
    /// A double-quoted string, quotes included.
    Text,
    /// `@base` or `@head`.
    Snapshot,
    /// A line break.
    Newline,
    /// `;`
    Semicolon,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// `=`
    Equals,
    /// `->`
    Arrow,
    /// `:-`
    Derives,
    /// `{`
    LeftBrace,
    /// `}`
    RightBrace,
    /// `(`
    LeftParen,
    /// `)`
    RightParen,
    /// `[`
    LeftBracket,
    /// `]`
    RightBracket,
    /// `!`
    Bang,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `==`
    EqualEqual,
    /// `!=`
    BangEqual,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `&&`
    AndAnd,
    /// `||`
    OrOr,
    /// A character the language does not use.
    Unknown,
    /// The end of the text.
    End,
}

/// A token and where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// What it is.
    pub kind: TokenKind,
    /// Its text's place.
    pub span: Span,
}

/// A comment, kept for formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Comment {
    /// Its text's place, markers included.
    pub span: Span,
    /// Whether it is a `/* */` comment.
    pub is_block: bool,
}

/// The tokens of a text, ending with [`TokenKind::End`].
#[derive(Debug, Clone, Default)]
pub struct Lexed {
    /// The tokens.
    pub tokens: Vec<Token>,
    /// The comments.
    pub comments: Vec<Comment>,
    /// Malformed literals and unknown characters.
    pub diagnostics: Vec<Diagnostic>,
}

/// Splits `source` into tokens.
pub fn lex(source: &str) -> Lexed {
    let mut lexer = Lexer {
        bytes: source.as_bytes(),
        position: 0,
        lexed: Lexed::default(),
    };
    while lexer.position < lexer.bytes.len() {
        lexer.next();
    }
    let end = source.len();
    lexer.lexed.tokens.push(Token {
        kind: TokenKind::End,
        span: Span::new(end, end),
    });
    lexer.lexed
}

struct Lexer<'a> {
    bytes: &'a [u8],
    position: usize,
    lexed: Lexed,
}

impl Lexer<'_> {
    fn peek(&self, ahead: usize) -> u8 {
        self.bytes.get(self.position + ahead).copied().unwrap_or(0)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.lexed.tokens.push(Token {
            kind,
            span: Span::new(start, self.position),
        });
    }

    fn next(&mut self) {
        let start = self.position;
        let byte = self.peek(0);
        match byte {
            b' ' | b'\t' | b'\r' => self.position += 1,
            b'\n' => {
                self.position += 1;
                self.push(TokenKind::Newline, start);
            }
            b'/' if self.peek(1) == b'/' => self.line_comment(),
            b'/' if self.peek(1) == b'*' => self.block_comment(),
            b'"' => self.text(),
            b'`' => self.escaped_name(),
            b'@' => self.snapshot(),
            b'0'..=b'9' => self.integer(),
            _ if is_name_start(byte) => self.name(),
            _ => self.punctuation(),
        }
    }

    fn line_comment(&mut self) {
        let start = self.position;
        while self.position < self.bytes.len() && self.peek(0) != b'\n' {
            self.position += 1;
        }
        self.lexed.comments.push(Comment {
            span: Span::new(start, self.position),
            is_block: false,
        });
    }

    fn block_comment(&mut self) {
        let start = self.position;
        self.position += 2;
        while self.position < self.bytes.len() && !(self.peek(0) == b'*' && self.peek(1) == b'/') {
            self.position += 1;
        }
        if self.position >= self.bytes.len() {
            self.lexed.diagnostics.push(Diagnostic::error(
                Span::new(start, start + 2),
                "this comment is never closed with `*/`",
            ));
        } else {
            self.position += 2;
        }
        self.lexed.comments.push(Comment {
            span: Span::new(start, self.position),
            is_block: true,
        });
    }

    fn text(&mut self) {
        let start = self.position;
        self.position += 1;
        loop {
            if self.position >= self.bytes.len() || self.peek(0) == b'\n' {
                self.lexed.diagnostics.push(Diagnostic::error(
                    Span::new(start, self.position),
                    "this string is never closed with `\"`",
                ));
                break;
            }
            match self.peek(0) {
                b'"' => {
                    self.position += 1;
                    break;
                }
                b'\\' => self.position += 2,
                _ => self.position += 1,
            }
        }
        self.position = self.position.min(self.bytes.len());
        self.push(TokenKind::Text, start);
    }

    fn escaped_name(&mut self) {
        let start = self.position;
        self.position += 1;
        while self.position < self.bytes.len() && !matches!(self.peek(0), b'`' | b'\n') {
            self.position += 1;
        }
        if self.peek(0) == b'`' {
            self.position += 1;
        } else {
            self.lexed.diagnostics.push(Diagnostic::error(
                Span::new(start, self.position),
                "this name is never closed with a backtick",
            ));
        }
        self.push(TokenKind::EscapedName, start);
    }

    fn snapshot(&mut self) {
        let start = self.position;
        self.position += 1;
        while is_name_continue(self.peek(0)) {
            self.position += 1;
        }
        let word = &self.bytes[start + 1..self.position];
        if word != b"base" && word != b"head" {
            self.lexed.diagnostics.push(Diagnostic::error(
                Span::new(start, self.position),
                "a snapshot is `@base` or `@head`",
            ));
        }
        self.push(TokenKind::Snapshot, start);
    }

    fn integer(&mut self) {
        let start = self.position;
        while self.peek(0).is_ascii_digit() {
            self.position += 1;
        }
        self.push(TokenKind::Integer, start);
    }

    fn name(&mut self) {
        let start = self.position;
        while is_name_continue(self.peek(0)) {
            self.position += 1;
        }
        self.push(TokenKind::Name, start);
    }

    fn punctuation(&mut self) {
        let start = self.position;
        let (kind, length) = punctuation(self.peek(0), self.peek(1));
        self.position += length;
        if kind == TokenKind::Unknown {
            // Step over a whole character, not one byte of it.
            while self.position < self.bytes.len() && (self.peek(0) & 0b1100_0000) == 0b1000_0000 {
                self.position += 1;
            }
            self.lexed.diagnostics.push(Diagnostic::error(
                Span::new(start, self.position),
                "this character is not part of the language",
            ));
        }
        self.push(kind, start);
    }
}

fn punctuation(first: u8, second: u8) -> (TokenKind, usize) {
    let pair = match (first, second) {
        (b'-', b'>') => Some(TokenKind::Arrow),
        (b':', b'-') => Some(TokenKind::Derives),
        (b'=', b'=') => Some(TokenKind::EqualEqual),
        (b'!', b'=') => Some(TokenKind::BangEqual),
        (b'<', b'=') => Some(TokenKind::LessEqual),
        (b'>', b'=') => Some(TokenKind::GreaterEqual),
        (b'&', b'&') => Some(TokenKind::AndAnd),
        (b'|', b'|') => Some(TokenKind::OrOr),
        _ => None,
    };
    if let Some(kind) = pair {
        return (kind, 2);
    }
    let kind = match first {
        b';' => TokenKind::Semicolon,
        b',' => TokenKind::Comma,
        b':' => TokenKind::Colon,
        b'.' => TokenKind::Dot,
        b'=' => TokenKind::Equals,
        b'{' => TokenKind::LeftBrace,
        b'}' => TokenKind::RightBrace,
        b'(' => TokenKind::LeftParen,
        b')' => TokenKind::RightParen,
        b'[' => TokenKind::LeftBracket,
        b']' => TokenKind::RightBracket,
        b'!' => TokenKind::Bang,
        b'+' => TokenKind::Plus,
        b'-' => TokenKind::Minus,
        b'*' => TokenKind::Star,
        b'/' => TokenKind::Slash,
        b'%' => TokenKind::Percent,
        b'<' => TokenKind::Less,
        b'>' => TokenKind::Greater,
        _ => TokenKind::Unknown,
    };
    (kind, 1)
}

fn is_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_name_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Decodes a string token's text, quotes included, with JSON escapes. Bad
/// escapes are kept as written, and reported by [`check_escapes`].
pub fn decode_text(raw: &str) -> String {
    let inner = raw
        .strip_prefix('"')
        .map_or(raw, |rest| rest.strip_suffix('"').unwrap_or(rest));
    let mut decoded = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => decoded.push('\n'),
            Some('t') => decoded.push('\t'),
            Some('r') => decoded.push('\r'),
            Some('b') => decoded.push('\u{8}'),
            Some('f') => decoded.push('\u{c}'),
            Some('u') => decoded.push(unicode_escape(&mut characters)),
            Some(other) => decoded.push(other),
            None => decoded.push('\\'),
        }
    }
    decoded
}

fn unicode_escape(characters: &mut std::str::Chars<'_>) -> char {
    let digits: String = characters.by_ref().take(4).collect();
    u32::from_str_radix(&digits, 16)
        .ok()
        .and_then(char::from_u32)
        .unwrap_or(char::REPLACEMENT_CHARACTER)
}

/// Escapes in a string token that JSON does not define.
pub fn check_escapes(raw: &str, span: Span) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            index += 1;
            continue;
        }
        let escaped = bytes.get(index + 1).copied().unwrap_or(0);
        let is_short_unicode = escaped == b'u'
            && !bytes
                .get(index + 2..index + 6)
                .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit));
        let is_known = matches!(
            escaped,
            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' | b'u'
        );
        if !is_known || is_short_unicode {
            let start = span.start as usize + index;
            diagnostics.push(Diagnostic::error(
                Span::new(start, start + 2),
                "this escape is not one JSON defines",
            ));
        }
        index += 2;
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        lex(source).tokens.iter().map(|token| token.kind).collect()
    }

    #[test]
    fn operators_of_two_characters_are_one_token() {
        let found = kinds("a -> b :- c != d");

        assert_eq!(
            found,
            [
                TokenKind::Name,
                TokenKind::Arrow,
                TokenKind::Name,
                TokenKind::Derives,
                TokenKind::Name,
                TokenKind::BangEqual,
                TokenKind::Name,
                TokenKind::End
            ]
        );
    }

    #[test]
    fn comments_are_kept_aside_and_newlines_are_tokens() {
        let lexed = lex("let a = 1 // one\n/* two */ let b = 2");

        assert_eq!(lexed.comments.len(), 2);
        assert!(lexed.comments[1].is_block);
        assert!(
            lexed
                .tokens
                .iter()
                .any(|token| token.kind == TokenKind::Newline)
        );
    }

    #[test]
    fn snapshots_are_single_tokens() {
        let found = kinds("operation@base(x)");

        assert_eq!(found[1], TokenKind::Snapshot);
    }

    #[test]
    fn an_unclosed_string_is_reported() {
        let lexed = lex("let a = \"open\nlet b = 1");

        assert_eq!(lexed.diagnostics.len(), 1);
    }

    #[test]
    fn strings_decode_json_escapes() {
        assert_eq!(decode_text(r#""a\"b\nA""#), "a\"b\nA");
    }

    #[test]
    fn a_unicode_escape_needs_four_hex_digits() {
        let raw = r#""\u12""#;

        assert_eq!(check_escapes(raw, Span::new(0, raw.len())).len(), 1);
        assert_eq!(check_escapes(r#""\u0041""#, Span::new(0, 8)), []);
    }

    #[test]
    fn unknown_escapes_are_reported() {
        let raw = r#""a\qb""#;

        let diagnostics = check_escapes(raw, Span::new(10, 10 + raw.len()));

        assert_eq!(diagnostics[0].span, Span::new(12, 14));
    }
}
