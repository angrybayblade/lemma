//! A recursive-descent parser for `.lm` modules.
//!
//! It never stops at the first error: a malformed item is reported and
//! skipped up to the next separator, so an editor still sees everything
//! else. Newlines separate items inside braces and at the top level, and
//! are whitespace inside parentheses and brackets; after an operator, a
//! comma, `:`, `=`, `->` or `:-` the parser skips them explicitly, which is
//! how a line continues (`.specs/13-lm-grammar.md` §1).

mod expressions;
mod items;
mod rules;
mod statements;

use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::syntax::ast::{File, Name};
use crate::syntax::lexer::{Token, TokenKind, lex};

/// What a file without a `module` line is told; the formatter, which
/// lays out the spec's fragments too, looks past it.
pub(crate) const MISSING_MODULE: &str = "expected `module <name>` first";

/// A parsed module and the problems found on the way.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    /// The syntax tree, complete as far as it could be read.
    pub file: File,
    /// Lexing and parsing problems, in source order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Parses one module's text.
pub fn parse(source: &str) -> Parsed {
    let lexed = lex(source);
    let mut parser = Parser {
        source,
        tokens: lexed.tokens,
        position: 0,
        newline_modes: Vec::new(),
        diagnostics: lexed.diagnostics,
    };
    let mut file = parser.file();
    file.comments = lexed.comments;
    let mut diagnostics = parser.diagnostics;
    diagnostics.sort_by_key(|diagnostic| diagnostic.span);
    Parsed { file, diagnostics }
}

pub(crate) struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    position: usize,
    /// For each open bracket, whether newlines inside it separate.
    newline_modes: Vec<bool>,
    diagnostics: Vec<Diagnostic>,
}

impl Parser<'_> {
    fn newlines_separate(&self) -> bool {
        self.newline_modes.last().copied().unwrap_or(true)
    }

    /// The index of the next token that matters here.
    fn current(&self) -> usize {
        self.significant_after(self.position)
    }

    fn significant_after(&self, mut index: usize) -> usize {
        if !self.newlines_separate() {
            while self.tokens[index].kind == TokenKind::Newline {
                index += 1;
            }
        }
        index
    }

    fn peek(&self) -> Token {
        self.tokens[self.current()]
    }

    fn peek_kind(&self) -> TokenKind {
        self.peek().kind
    }

    /// The kind of the `ahead`-th token after the next one that matters.
    fn nth_kind(&self, ahead: usize) -> TokenKind {
        let mut index = self.current();
        for _ in 0..ahead {
            if self.tokens[index].kind == TokenKind::End {
                break;
            }
            index = self.significant_after(index + 1);
        }
        self.tokens[index].kind
    }

    fn nth_text(&self, ahead: usize) -> &str {
        let mut index = self.current();
        for _ in 0..ahead {
            if self.tokens[index].kind == TokenKind::End {
                break;
            }
            index = self.significant_after(index + 1);
        }
        self.tokens[index].span.text(self.source)
    }

    fn bump(&mut self) -> Token {
        let index = self.current();
        let token = self.tokens[index];
        if token.kind != TokenKind::End {
            self.position = index + 1;
        }
        token
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek_kind() == kind
    }

    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        self.at(kind).then(|| self.bump())
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Option<Token> {
        if let Some(token) = self.eat(kind) {
            return Some(token);
        }
        self.error_here(&format!("expected {what}"));
        None
    }

    /// Whether the next token is the word `word`, keyword or not.
    fn at_word(&self, word: &str) -> bool {
        self.at(TokenKind::Name) && self.text(self.peek()) == word
    }

    fn eat_word(&mut self, word: &str) -> Option<Token> {
        self.at_word(word).then(|| self.bump())
    }

    fn skip_newlines(&mut self) {
        while self.tokens[self.position].kind == TokenKind::Newline {
            self.position += 1;
        }
    }

    fn text(&self, token: Token) -> &str {
        token.span.text(self.source)
    }

    /// Where the last consumed token ended.
    fn previous_end(&self) -> u32 {
        self.position
            .checked_sub(1)
            .map_or(0, |index| self.tokens[index].span.end)
    }

    /// A span from `start` to the end of the last consumed token.
    fn span_from(&self, start: u32) -> Span {
        Span {
            start,
            end: self.previous_end().max(start),
        }
    }

    fn error_here(&mut self, message: &str) {
        let token = self.peek();
        let found = match token.kind {
            TokenKind::End => "the end of the file".to_owned(),
            TokenKind::Newline => "a new line".to_owned(),
            _ => format!("`{}`", self.text(token)),
        };
        self.diagnostics.push(Diagnostic::error(
            token.span,
            format!("{message}, found {found}"),
        ));
    }

    fn error_at(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::error(span, message));
    }

    /// Runs `parse` between `open` and `close`, with newlines inside
    /// separating or not. Reports a missing closing bracket.
    fn delimited<T>(
        &mut self,
        open: TokenKind,
        close: TokenKind,
        newlines_separate: bool,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.expect(open, opening_name(open));
        self.newline_modes.push(newlines_separate);
        let result = parse(self);
        self.skip_separators_before(close);
        if self.expect(close, closing_name(close)).is_none() {
            self.recover_to(close);
        }
        self.newline_modes.pop();
        result
    }

    fn skip_separators_before(&mut self, close: TokenKind) {
        let index = self.significant_after(self.position);
        let mut probe = index;
        while matches!(
            self.tokens[probe].kind,
            TokenKind::Newline | TokenKind::Semicolon
        ) {
            probe += 1;
        }
        if self.tokens[probe].kind == close {
            self.position = probe;
        }
    }

    /// Items separated by newlines or `;` (or, where `commas` is set, by
    /// commas too) until `close` or the end of the file. A malformed item
    /// is reported and skipped.
    fn separated<T>(
        &mut self,
        close: TokenKind,
        commas: bool,
        mut item: impl FnMut(&mut Self) -> Option<T>,
    ) -> Vec<T> {
        let mut items = Vec::new();
        loop {
            self.skip_separators(commas);
            if self.at(close) || self.at(TokenKind::End) {
                return items;
            }
            let before = self.position;
            match item(self) {
                Some(value) => items.push(value),
                None => self.recover_in_block(before),
            }
            if !self.at_separator(commas) && !self.at(close) && !self.at(TokenKind::End) {
                self.error_here("expected a new line or `;` after this");
                self.recover_in_block(self.position);
            }
        }
    }

    fn at_separator(&self, commas: bool) -> bool {
        let kind = self.tokens[self.position].kind;
        matches!(kind, TokenKind::Newline | TokenKind::Semicolon)
            || (commas && self.at(TokenKind::Comma))
    }

    fn skip_separators(&mut self, commas: bool) {
        while self.at_separator(commas) {
            self.position += 1;
        }
    }

    /// Skips to the next separator or closing brace at this depth, always
    /// moving past at least one token so parsing makes progress.
    fn recover_in_block(&mut self, before: usize) {
        if self.position == before && !self.at(TokenKind::End) && !self.at(TokenKind::RightBrace) {
            self.bump();
        }
        let mut depth = 0usize;
        loop {
            let kind = self.tokens[self.position].kind;
            let ends_item = matches!(
                kind,
                TokenKind::Newline | TokenKind::Semicolon | TokenKind::RightBrace
            );
            if kind == TokenKind::End || (depth == 0 && ends_item) {
                return;
            }
            depth = adjust_depth(depth, kind);
            self.position += 1;
        }
    }

    /// Skips to just past `close` at this depth.
    fn recover_to(&mut self, close: TokenKind) {
        let mut depth = 0usize;
        loop {
            let kind = self.tokens[self.position].kind;
            if kind == TokenKind::End {
                return;
            }
            self.position += 1;
            if kind == close && depth == 0 {
                return;
            }
            depth = adjust_depth(depth, kind);
        }
    }

    /// A name: a plain or backticked identifier.
    fn name(&mut self, what: &str) -> Option<Name> {
        let token = self.peek();
        match token.kind {
            TokenKind::Name => {
                self.bump();
                Some(Name {
                    text: self.text(token).to_owned(),
                    span: token.span,
                })
            }
            TokenKind::EscapedName => {
                self.bump();
                let text = self.text(token).trim_matches('`').to_owned();
                Some(Name {
                    text,
                    span: token.span,
                })
            }
            _ => {
                self.error_here(&format!("expected {what}"));
                None
            }
        }
    }

    /// A name being declared or bound, which may not be a reserved word
    /// unless written in backticks.
    fn declared_name(&mut self, what: &str) -> Option<Name> {
        let is_plain = self.at(TokenKind::Name);
        let name = self.name(what)?;
        if is_plain && crate::syntax::lexer::is_reserved(&name.text) {
            let message = format!(
                "`{}` is a reserved word; write it in backticks to use it as a name",
                name.text
            );
            self.error_at(name.span, message);
        }
        Some(name)
    }

    fn at_name(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Name | TokenKind::EscapedName)
    }
}

/// The bracket depth after `kind`, from `depth`.
fn adjust_depth(depth: usize, kind: TokenKind) -> usize {
    match kind {
        TokenKind::LeftBrace | TokenKind::LeftParen | TokenKind::LeftBracket => depth + 1,
        TokenKind::RightBrace | TokenKind::RightParen | TokenKind::RightBracket => {
            depth.saturating_sub(1)
        }
        _ => depth,
    }
}

fn opening_name(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::LeftBrace => "`{`",
        TokenKind::LeftParen => "`(`",
        TokenKind::LeftBracket => "`[`",
        TokenKind::Less => "`<`",
        _ => "an opening bracket",
    }
}

fn closing_name(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::RightBrace => "`}`",
        TokenKind::RightParen => "`)`",
        TokenKind::RightBracket => "`]`",
        TokenKind::Greater => "`>`",
        _ => "a closing bracket",
    }
}

#[cfg(test)]
mod tests;
