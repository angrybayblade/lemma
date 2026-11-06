//! Lines, indentation and blank lines.
//!
//! The text is cut into lines at newline tokens, so a line break the author
//! wrote is never added or removed. Each line is indented by the brackets
//! open at its start, plus one level when it continues the line before it;
//! each comment-only line takes the indentation of the code it precedes.

use std::collections::BTreeSet;

use super::spacing::{self, Classified, Role};
use crate::span::Span;
use crate::syntax::ast::{Declaration, File, GraphBody, Item};
use crate::syntax::lexer::{Comment, Lexed, TokenKind};

/// One level of indentation, as the standard library is written.
const INDENT: &str = "    ";

/// Something on a line, in source order.
#[derive(Debug, Clone, Copy)]
enum Piece {
    /// A token, by index.
    Token(usize),
    /// A comment, by index.
    Comment(usize),
}

/// What lies between two newline tokens. A block comment spanning lines
/// is one piece, so such a line covers several lines of text.
#[derive(Debug, Default)]
struct Line {
    pieces: Vec<Piece>,
}

impl Line {
    fn is_blank(&self) -> bool {
        self.pieces.is_empty()
    }

    fn tokens(&self) -> impl Iterator<Item = usize> + '_ {
        self.pieces.iter().filter_map(|piece| match piece {
            Piece::Token(index) => Some(*index),
            Piece::Comment(_) => None,
        })
    }

    fn first_token(&self) -> Option<usize> {
        self.tokens().next()
    }

    fn last_token(&self) -> Option<usize> {
        self.tokens().last()
    }
}

/// The lines a top-level item covers.
struct Extent {
    first_line: usize,
    last_line: usize,
    /// Whether it is set off from its neighbours by blank lines: the
    /// `module` line, and a declaration whose braced body spans lines.
    wants_blank_lines: bool,
}

/// A bracket open at some point, and how lines inside it are indented.
struct Open {
    /// The indentation of lines inside it.
    content_indent: usize,
    /// Whether newlines inside it separate, so that lines can continue.
    is_separating: bool,
}

/// Lays out a module that parsed without errors.
pub(super) fn render(
    source: &str,
    lexed: &Lexed,
    classified: &[Classified],
    file: &File,
) -> String {
    let layout = Layout {
        source,
        lexed,
        classified,
    };
    let lines = layout.lines();
    let indents = layout.indents(&lines);
    let required = layout.required_blank_lines(file, &lines);
    layout.write(&lines, &indents, &required)
}

struct Layout<'a> {
    source: &'a str,
    lexed: &'a Lexed,
    classified: &'a [Classified],
}

impl Layout<'_> {
    fn lines(&self) -> Vec<Line> {
        let mut lines = vec![Line::default()];
        let mut comments = self.lexed.comments.iter().enumerate().peekable();
        for (index, token) in self.lexed.tokens.iter().enumerate() {
            let before_token =
                |(_, comment): &(usize, &Comment)| comment.span.start < token.span.start;
            while let Some((comment, _)) = comments.next_if(before_token) {
                push_piece(&mut lines, Piece::Comment(comment));
            }
            match token.kind {
                TokenKind::Newline => lines.push(Line::default()),
                TokenKind::End => {}
                _ => push_piece(&mut lines, Piece::Token(index)),
            }
        }
        lines
    }

    fn kind(&self, token: usize) -> TokenKind {
        self.classified[token].kind
    }

    /// Each line's indentation, in levels.
    fn indents(&self, lines: &[Line]) -> Vec<usize> {
        let mut indents = vec![0; lines.len()];
        let mut open: Vec<Open> = Vec::new();
        let mut previous = None;
        for (number, line) in lines.iter().enumerate() {
            let Some(first) = line.first_token() else {
                continue;
            };
            let indent = self.code_indent(&open, first, previous);
            indents[number] = indent;
            for token in line.tokens() {
                self.track_brackets(&mut open, token, indent);
                previous = Some(token);
            }
        }
        self.indent_comment_lines(lines, &mut indents);
        indents
    }

    /// A code line's indentation: a closing bracket's line lines up with the
    /// line that opened it; any other line goes one level inside the
    /// innermost bracket, and one more when it continues the line before.
    fn code_indent(&self, open: &[Open], first: usize, previous: Option<usize>) -> usize {
        let innermost = open.last();
        if is_closing(self.kind(first)) {
            return innermost.map_or(0, |open| open.content_indent.saturating_sub(1));
        }
        let base = innermost.map_or(0, |open| open.content_indent);
        let is_separating = innermost.is_none_or(|open| open.is_separating);
        if is_separating && self.continues(first, previous) {
            return base + 1;
        }
        base
    }

    /// Whether the line starting with `first` continues the one before,
    /// which ended with `previous`. A comma ends a field where commas
    /// separate fields, which the next line then shows by starting another.
    fn continues(&self, first: usize, previous: Option<usize>) -> bool {
        if self.is_word(first, "because") {
            return true;
        }
        let Some(previous) = previous else {
            return false;
        };
        if self.kind(previous) == TokenKind::Comma && self.starts_field(first) {
            return false;
        }
        spacing::continues_line(self.classified[previous])
    }

    /// Whether the line starting at `first` starts with `name:`.
    fn starts_field(&self, first: usize) -> bool {
        let is_name = matches!(self.kind(first), TokenKind::Name | TokenKind::EscapedName);
        let next = self.classified.get(first + 1).map(|token| token.kind);
        is_name && next == Some(TokenKind::Colon)
    }

    fn is_word(&self, token: usize, word: &str) -> bool {
        let token = self.lexed.tokens[token];
        token.kind == TokenKind::Name && token.span.text(self.source) == word
    }

    fn track_brackets(&self, open: &mut Vec<Open>, token: usize, indent: usize) {
        let classified = self.classified[token];
        let is_separating = match classified.kind {
            TokenKind::LeftBrace => classified.role != Role::NameList,
            TokenKind::LeftParen | TokenKind::LeftBracket => false,
            kind => {
                if is_closing(kind) {
                    open.pop();
                }
                return;
            }
        };
        open.push(Open {
            content_indent: indent + 1,
            is_separating,
        });
    }

    /// Gives each comment-only line the indentation of the next code line,
    /// or of the inside of the block that line closes.
    fn indent_comment_lines(&self, lines: &[Line], indents: &mut [usize]) {
        let mut next_indent = 0;
        for number in (0..lines.len()).rev() {
            match lines[number].first_token() {
                Some(first) => {
                    let closes = is_closing(self.kind(first));
                    next_indent = indents[number] + usize::from(closes);
                }
                None => indents[number] = next_indent,
            }
        }
    }

    /// The lines after which a blank line goes even if the author wrote
    /// none: after the `module` line, and around a top-level declaration
    /// whose braced body spans several lines. A blank line already anywhere
    /// between two items is enough.
    fn required_blank_lines(&self, file: &File, lines: &[Line]) -> BTreeSet<usize> {
        let newlines: Vec<u32> = self
            .lexed
            .tokens
            .iter()
            .filter(|token| token.kind == TokenKind::Newline)
            .map(|token| token.span.start)
            .collect();
        let line_of = |offset: u32| newlines.partition_point(|start| *start < offset);
        let mut extents: Vec<Extent> = Vec::new();
        if let Some(module) = &file.module {
            let line = line_of(module.span.start);
            extents.push(Extent {
                first_line: line,
                last_line: line,
                wants_blank_lines: true,
            });
        }
        for item in &file.items {
            let span = item_span(item);
            let (first_line, last_line) = (line_of(span.start), line_of(span.end));
            extents.push(Extent {
                first_line,
                last_line,
                wants_blank_lines: has_block(item) && first_line != last_line,
            });
        }
        let mut required = BTreeSet::new();
        for pair in extents.windows(2) {
            let (first, second) = (&pair[0], &pair[1]);
            let is_below = second.first_line > first.last_line;
            let has_blank =
                (first.last_line + 1..second.first_line).any(|line| lines[line].is_blank());
            let wants_blank = first.wants_blank_lines || second.wants_blank_lines;
            if is_below && wants_blank && !has_blank {
                required.insert(first.last_line);
            }
        }
        required
    }

    fn write(&self, lines: &[Line], indents: &[usize], required: &BTreeSet<usize>) -> String {
        let mut output = String::with_capacity(self.source.len());
        let mut previous: Option<usize> = None;
        let mut saw_blank = false;
        for (number, line) in lines.iter().enumerate() {
            if line.is_blank() {
                saw_blank = true;
                continue;
            }
            let keeps_blank = |previous: usize| {
                let is_inside_block =
                    self.ends_with_opening(&lines[previous]) || self.starts_with_closing(line);
                (saw_blank && !is_inside_block) || required.contains(&previous)
            };
            if previous.is_some_and(keeps_blank) {
                output.push('\n');
            }
            self.write_line(&mut output, line, indents[number]);
            previous = Some(number);
            saw_blank = false;
        }
        output
    }

    fn ends_with_opening(&self, line: &Line) -> bool {
        line.last_token()
            .is_some_and(|token| is_opening(self.kind(token)))
    }

    fn starts_with_closing(&self, line: &Line) -> bool {
        line.first_token()
            .is_some_and(|token| is_closing(self.kind(token)))
    }

    fn write_line(&self, output: &mut String, line: &Line, indent: usize) {
        let start = output.len();
        for _ in 0..indent {
            output.push_str(INDENT);
        }
        let mut previous: Option<Piece> = None;
        for piece in &line.pieces {
            if previous.is_some_and(|previous| self.space_between(previous, *piece)) {
                output.push(' ');
            }
            output.push_str(self.text(*piece));
            previous = Some(*piece);
        }
        let kept = output[start..].trim_end().len();
        output.truncate(start + kept);
        output.push('\n');
    }

    /// Spacing between two pieces; a comment has a space on each side,
    /// except next to a bracket or comma it hugs.
    fn space_between(&self, previous: Piece, current: Piece) -> bool {
        match (previous, current) {
            (Piece::Token(previous), Piece::Token(current)) => {
                spacing::space_between(self.classified[previous], self.classified[current])
            }
            (Piece::Token(previous), Piece::Comment(_)) => !matches!(
                self.kind(previous),
                TokenKind::LeftParen | TokenKind::LeftBracket
            ),
            (Piece::Comment(_), Piece::Token(current)) => !matches!(
                self.kind(current),
                TokenKind::RightParen
                    | TokenKind::RightBracket
                    | TokenKind::Comma
                    | TokenKind::Semicolon
            ),
            (Piece::Comment(_), Piece::Comment(_)) => true,
        }
    }

    fn text(&self, piece: Piece) -> &str {
        let span = match piece {
            Piece::Token(index) => self.lexed.tokens[index].span,
            Piece::Comment(index) => self.lexed.comments[index].span,
        };
        span.text(self.source)
    }
}

fn push_piece(lines: &mut [Line], piece: Piece) {
    if let Some(line) = lines.last_mut() {
        line.pieces.push(piece);
    }
}

fn is_opening(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::LeftBrace | TokenKind::LeftParen | TokenKind::LeftBracket
    )
}

fn is_closing(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::RightBrace | TokenKind::RightParen | TokenKind::RightBracket
    )
}

/// Whether an item has a braced body: a workspace, a service, a rule and
/// the like, but not a binding, a relation or an import, which may be
/// grouped even when one spans lines.
fn has_block(item: &Item) -> bool {
    let Item::Declaration(declared) = item else {
        return matches!(item, Item::Workspace(_));
    };
    match &declared.declaration {
        Declaration::Graph(graph) => matches!(graph.body, GraphBody::Rows(_)),
        Declaration::Binding(_) | Declaration::Type(_) | Declaration::Relation(_) => false,
        _ => true,
    }
}

fn item_span(item: &Item) -> Span {
    match item {
        Item::Import(import) => import.span,
        Item::Reexport(reexport) => reexport.span,
        Item::Workspace(workspace) => workspace.span,
        Item::Repo(repo) => repo.span,
        Item::Declaration(declared) => declared.span,
    }
}
