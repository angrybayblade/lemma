//! The spaces between two tokens on one line.
//!
//! Most pairs get one space. Brackets, `.`, `,`, `;`, snapshots, prefix
//! operators, `:` and type arguments are written tight. A token's role,
//! worked out once for the whole module, settles the cases its kind alone
//! cannot: a `-` that negates, a `<` that opens type arguments, a `*` that
//! selects every name.

use super::facts::Facts;
use crate::syntax::lexer::{Token, TokenKind, is_reserved};

/// What a token does, where its kind leaves it open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// Nothing special.
    Plain,
    /// A reserved word starting a construct, such as `in` or `return`, which
    /// is followed by a space even before a bracket.
    Keyword,
    /// `!`, or a `-` that negates.
    Prefix,
    /// The `<` opening type arguments.
    GenericOpen,
    /// The `>` closing type arguments.
    GenericClose,
    /// The `:` between a graph's or rule's name and its relation.
    RelationColon,
    /// The `*` of `import * as` or of `run module.*`.
    Wildcard,
    /// The `{` of `import { ... }` or `export { ... } from`, inside which
    /// newlines do not separate.
    NameList,
}

/// A token's kind and role: all the spacing rules look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Classified {
    /// The token's kind.
    pub kind: TokenKind,
    /// Its role.
    pub role: Role,
}

/// Each token's role, in order.
pub(super) fn classify(source: &str, tokens: &[Token], facts: &Facts) -> Vec<Classified> {
    let mut classifier = Classifier {
        source,
        facts,
        classified: Vec::with_capacity(tokens.len()),
        separating: Vec::new(),
        previous: None,
    };
    for (index, token) in tokens.iter().enumerate() {
        let role = classifier.role(tokens, index);
        classifier.classified.push(Classified {
            kind: token.kind,
            role,
        });
        classifier.track_brackets(token.kind, role);
        if token.kind != TokenKind::Newline {
            classifier.previous = Some(index);
        }
    }
    classifier.classified
}

struct Classifier<'a> {
    source: &'a str,
    facts: &'a Facts,
    classified: Vec<Classified>,
    /// For each open bracket, whether newlines inside it separate.
    separating: Vec<bool>,
    /// The last token before this one that is not a newline.
    previous: Option<usize>,
}

impl Classifier<'_> {
    fn role(&self, tokens: &[Token], index: usize) -> Role {
        let token = tokens[index];
        let previous = self.previous.map(|previous| tokens[previous]);
        let previous_end = previous.map(|previous| previous.span.end);
        let follows = |end: Option<u32>, ends: &std::collections::BTreeSet<u32>| {
            end.is_some_and(|end| ends.contains(&end))
        };
        match token.kind {
            TokenKind::Name if self.is_keyword(token, previous) => Role::Keyword,
            TokenKind::Bang => Role::Prefix,
            TokenKind::Minus if self.is_prefix_minus(tokens, index) => Role::Prefix,
            TokenKind::Less if follows(previous_end, &self.facts.generic_opens) => {
                Role::GenericOpen
            }
            TokenKind::Greater if self.facts.generic_closes.contains(&token.span.end) => {
                Role::GenericClose
            }
            TokenKind::Colon if follows(previous_end, &self.facts.relation_colons) => {
                Role::RelationColon
            }
            TokenKind::Star if previous.is_some_and(|p| self.is_dot_or_import(p)) => Role::Wildcard,
            TokenKind::LeftBrace if previous.is_some_and(|p| self.starts_name_list(p)) => {
                Role::NameList
            }
            _ => Role::Plain,
        }
    }

    /// A reserved word other than a value (`true`, `self`, ...), unless it
    /// is a member after `.`, as in `python.module(...)`.
    fn is_keyword(&self, token: Token, previous: Option<Token>) -> bool {
        let text = token.span.text(self.source);
        let is_value = matches!(text, "true" | "false" | "null" | "self");
        let is_member = previous.is_some_and(|previous| previous.kind == TokenKind::Dot);
        is_reserved(text) && !is_value && !is_member
    }

    /// Whether a `-` negates: it starts a line where newlines separate, or
    /// follows something that cannot end a value.
    fn is_prefix_minus(&self, tokens: &[Token], index: usize) -> bool {
        let starts_line = index
            .checked_sub(1)
            .is_some_and(|before| tokens[before].kind == TokenKind::Newline);
        if starts_line && self.separating.last().copied().unwrap_or(true) {
            return true;
        }
        let Some(previous) = self.previous else {
            return true;
        };
        !ends_value(self.classified[previous])
    }

    fn is_dot_or_import(&self, previous: Token) -> bool {
        previous.kind == TokenKind::Dot || self.is_word(previous, "import")
    }

    fn starts_name_list(&self, previous: Token) -> bool {
        self.is_word(previous, "import") || self.is_word(previous, "export")
    }

    fn is_word(&self, token: Token, word: &str) -> bool {
        token.kind == TokenKind::Name && token.span.text(self.source) == word
    }

    fn track_brackets(&mut self, kind: TokenKind, role: Role) {
        match kind {
            TokenKind::LeftBrace => self.separating.push(role != Role::NameList),
            TokenKind::LeftParen | TokenKind::LeftBracket => self.separating.push(false),
            TokenKind::RightBrace | TokenKind::RightParen | TokenKind::RightBracket => {
                self.separating.pop();
            }
            _ => {}
        }
    }
}

/// Whether a token can end a value, so that a `-` or `(` after it continues
/// that value: `a - b`, `f(x)`.
fn ends_value(token: Classified) -> bool {
    match token.kind {
        TokenKind::Name => token.role != Role::Keyword,
        TokenKind::EscapedName
        | TokenKind::Integer
        | TokenKind::Text
        | TokenKind::Snapshot
        | TokenKind::RightParen
        | TokenKind::RightBracket
        | TokenKind::RightBrace => true,
        TokenKind::Greater => token.role == Role::GenericClose,
        _ => false,
    }
}

/// Whether `current` is written with a space before it, after `previous`
/// on the same line.
pub(super) fn space_between(previous: Classified, current: Classified) -> bool {
    let is_tight_after = matches!(
        previous.kind,
        TokenKind::LeftParen | TokenKind::LeftBracket | TokenKind::Dot
    ) || matches!(previous.role, Role::Prefix | Role::GenericOpen);
    if is_tight_before(current) || is_tight_after {
        return false;
    }
    match current.kind {
        TokenKind::LeftParen | TokenKind::LeftBracket => !ends_value(previous),
        TokenKind::RightBrace => previous.kind != TokenKind::LeftBrace,
        _ => true,
    }
}

/// Tokens written right after what precedes them.
fn is_tight_before(token: Classified) -> bool {
    let is_tight_kind = matches!(
        token.kind,
        TokenKind::Comma
            | TokenKind::Semicolon
            | TokenKind::RightParen
            | TokenKind::RightBracket
            | TokenKind::Dot
            | TokenKind::Snapshot
    );
    let is_tight_colon = token.kind == TokenKind::Colon && token.role != Role::RelationColon;
    let is_tight_role = matches!(token.role, Role::GenericOpen | Role::GenericClose);
    is_tight_kind || is_tight_colon || is_tight_role
}

/// Whether a line ending in `token` continues on the next one where
/// newlines separate (`.specs/13-lm-grammar.md` §1, Continuation).
pub(super) fn continues_line(token: Classified) -> bool {
    match token.kind {
        TokenKind::Colon
        | TokenKind::Equals
        | TokenKind::Arrow
        | TokenKind::Derives
        | TokenKind::Comma
        | TokenKind::OrOr
        | TokenKind::AndAnd
        | TokenKind::EqualEqual
        | TokenKind::BangEqual
        | TokenKind::Less
        | TokenKind::LessEqual
        | TokenKind::GreaterEqual
        | TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Slash
        | TokenKind::Percent => true,
        TokenKind::Greater => token.role != Role::GenericClose,
        TokenKind::Star => token.role != Role::Wildcard,
        _ => false,
    }
}
