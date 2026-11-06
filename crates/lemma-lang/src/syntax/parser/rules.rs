//! Rules, rule bodies and terms: the Datalog part of the language.

use super::Parser;
use crate::syntax::ast::{
    Atom, AtomArguments, Body, Comparison, ConstantPath, Field, Literal, NamedTerm, PathStep,
    RuleClause, RuleColumns, RuleDeclaration, Snapshot, Term, TermKind,
};
use crate::syntax::lexer::{TokenKind, decode_text};

impl Parser<'_> {
    pub(super) fn rule(&mut self) -> Option<RuleDeclaration> {
        let start = self.bump().span.start;
        let name = self.declared_name("the rule's name")?;
        let columns = if self.eat(TokenKind::Colon).is_some() {
            RuleColumns::Relation(self.type_expression()?)
        } else {
            RuleColumns::Inline(self.parameters())
        };
        let clauses = self.delimited(
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            true,
            |parser| parser.separated(TokenKind::RightBrace, false, Parser::clause),
        );
        Some(RuleDeclaration {
            name,
            columns,
            clauses,
            span: self.span_from(start),
        })
    }

    fn clause(&mut self) -> Option<RuleClause> {
        let start = self.peek().span.start;
        let head = self.delimited(TokenKind::LeftParen, TokenKind::RightParen, false, |p| {
            p.comma_separated(TokenKind::RightParen, Parser::term)
        });
        self.expect(TokenKind::Derives, "`:-` after the clause's head")?;
        self.skip_newlines();
        let body = self.body()?;
        Some(RuleClause {
            head,
            body,
            span: self.span_from(start),
        })
    }

    /// The fields of a `require` or `allow`, where `when` holds a body.
    pub(super) fn fields_with_body(&mut self) -> (Option<Body>, Vec<Field>) {
        let members = self.delimited(
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            true,
            |parser| parser.separated(TokenKind::RightBrace, false, Parser::field_or_body),
        );
        let mut when = None;
        let mut fields = Vec::new();
        for member in members {
            match member {
                Ok(body) => when = Some(body),
                Err(field) => fields.push(field),
            }
        }
        (when, fields)
    }

    /// `when: body`, or else a field.
    fn field_or_body(&mut self) -> Option<Result<Body, Field>> {
        if self.at_word("when") && self.nth_kind(1) == TokenKind::Colon {
            self.bump();
            self.bump();
            self.skip_newlines();
            return self.body().map(Ok);
        }
        self.field().map(Err)
    }

    /// Literals joined by commas; a newline after a comma continues.
    pub(super) fn body(&mut self) -> Option<Body> {
        let start = self.peek().span.start;
        let mut literals = vec![self.literal()?];
        while self.eat(TokenKind::Comma).is_some() {
            self.skip_newlines();
            literals.push(self.literal()?);
        }
        Some(Body {
            literals,
            span: self.span_from(start),
        })
    }

    fn literal(&mut self) -> Option<Literal> {
        let start = self.peek().span.start;
        if self.eat(TokenKind::Bang).is_some() {
            return self.atom(true, start).map(Literal::Atom);
        }
        if self.starts_atom() {
            return self.atom(false, start).map(Literal::Atom);
        }
        let left = self.term()?;
        let Some(operator) = self.comparison_operator() else {
            self.error_here("expected a comparison such as `==` or `!=`, or a predicate call");
            return None;
        };
        self.skip_newlines();
        let right = self.term()?;
        Some(Literal::Comparison(Comparison {
            left,
            operator,
            right,
            span: self.span_from(start),
        }))
    }

    /// Whether the next tokens are `name(.name)*` then `(` or a snapshot.
    fn starts_atom(&self) -> bool {
        let mut ahead = 0;
        loop {
            if !matches!(
                self.nth_kind(ahead),
                TokenKind::Name | TokenKind::EscapedName
            ) {
                return false;
            }
            match self.nth_kind(ahead + 1) {
                TokenKind::LeftParen | TokenKind::Snapshot => return true,
                TokenKind::Dot => ahead += 2,
                _ => return false,
            }
        }
    }

    fn atom(&mut self, is_negated: bool, start: u32) -> Option<Atom> {
        let predicate = self.module_name("a graph, rule or relation")?;
        let snapshot = self.snapshot();
        let arguments = self.delimited(
            TokenKind::LeftParen,
            TokenKind::RightParen,
            false,
            Parser::atom_arguments,
        );
        Some(Atom {
            is_negated,
            predicate,
            snapshot,
            arguments,
            span: self.span_from(start),
        })
    }

    pub(super) fn snapshot(&mut self) -> Option<Snapshot> {
        let token = self.eat(TokenKind::Snapshot)?;
        Some(if self.text(token) == "@base" {
            Snapshot::Base
        } else {
            Snapshot::Head
        })
    }

    fn atom_arguments(&mut self) -> AtomArguments {
        let is_named = self.at_name() && self.nth_kind(1) == TokenKind::Colon;
        if !is_named {
            return AtomArguments::Positional(
                self.comma_separated(TokenKind::RightParen, Parser::term),
            );
        }
        AtomArguments::Named(self.comma_separated(TokenKind::RightParen, |parser| {
            let name = parser.declared_name("a column name")?;
            parser.expect(TokenKind::Colon, "`:` after the column's name")?;
            let term = parser.term()?;
            Some(NamedTerm { name, term })
        }))
    }

    /// A term; spelling decides whether a name is a variable or a constant.
    pub(super) fn term(&mut self) -> Option<Term> {
        let token = self.peek();
        let start = token.span.start;
        let kind = match token.kind {
            TokenKind::Text => {
                self.bump();
                TermKind::Text(decode_text(self.text(token)))
            }
            TokenKind::Integer | TokenKind::Minus => TermKind::Integer(self.integer_term()?),
            TokenKind::Name | TokenKind::EscapedName => self.named_term()?,
            _ => {
                self.error_here("expected a variable, `_`, or a constant");
                return None;
            }
        };
        Some(Term {
            kind,
            span: self.span_from(start),
        })
    }

    fn integer_term(&mut self) -> Option<i64> {
        let is_negative = self.eat(TokenKind::Minus).is_some();
        let token = self.expect(TokenKind::Integer, "an integer")?;
        let text = self.text(token);
        let written = if is_negative {
            format!("-{text}")
        } else {
            text.to_owned()
        };
        if let Ok(value) = written.parse() {
            return Some(value);
        }
        self.error_at(token.span, "this integer does not fit in 64 bits");
        Some(0)
    }

    fn named_term(&mut self) -> Option<TermKind> {
        let name = self.name("a term")?;
        match name.text.as_str() {
            "_" => return Some(TermKind::Wildcard),
            "true" => return Some(TermKind::Boolean(true)),
            "false" => return Some(TermKind::Boolean(false)),
            "null" => return Some(TermKind::Null),
            "self" => {
                return Some(TermKind::Constant(ConstantPath {
                    root: None,
                    steps: self.path_steps()?,
                }));
            }
            reserved if crate::syntax::lexer::is_reserved(reserved) => {
                let message = format!("`{reserved}` is a reserved word, not a variable");
                self.error_at(name.span, message);
            }
            _ => {}
        }
        if is_variable(&name.text) {
            if self.at(TokenKind::Dot) && self.at_name_after_dot() {
                self.bump();
                let field = self.name("a field name")?;
                return Some(TermKind::Field {
                    variable: name,
                    field,
                });
            }
            return Some(TermKind::Variable(name.text));
        }
        Some(TermKind::Constant(ConstantPath {
            root: Some(name),
            steps: self.path_steps()?,
        }))
    }

    fn at_name_after_dot(&self) -> bool {
        matches!(self.nth_kind(1), TokenKind::Name | TokenKind::EscapedName)
    }

    fn path_steps(&mut self) -> Option<Vec<PathStep>> {
        let mut steps = Vec::new();
        loop {
            if self.at(TokenKind::Dot) && self.at_name_after_dot() {
                self.bump();
                steps.push(PathStep::Member(self.name("a member name")?));
                continue;
            }
            if self.at(TokenKind::LeftBracket) {
                let key = self.delimited(
                    TokenKind::LeftBracket,
                    TokenKind::RightBracket,
                    false,
                    Parser::key_text,
                )?;
                steps.push(PathStep::Key(key));
                continue;
            }
            return Some(steps);
        }
    }

    fn key_text(&mut self) -> Option<crate::syntax::ast::Name> {
        let token = self.expect(TokenKind::Text, "a string key")?;
        Some(crate::syntax::ast::Name {
            text: decode_text(self.text(token)),
            span: token.span,
        })
    }
}

/// Whether a bare name in a pattern is a variable: it starts with a
/// lowercase letter, or with `_` followed by more.
pub fn is_variable(name: &str) -> bool {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) if first.is_ascii_lowercase() => true,
        Some('_') => characters.next().is_some(),
        _ => false,
    }
}
