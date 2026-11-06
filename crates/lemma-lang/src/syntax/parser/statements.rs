//! Blocks and the statements of behaviors and functions.

use super::Parser;
use crate::syntax::ast::{Binding, Block, Else, IfStatement, Name, Statement};
use crate::syntax::lexer::TokenKind;

impl Parser<'_> {
    pub(super) fn block(&mut self) -> Block {
        let start = self.peek().span.start;
        let statements = self.delimited(
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            true,
            |parser| parser.separated(TokenKind::RightBrace, false, Parser::statement),
        );
        Block {
            statements,
            span: self.span_from(start),
        }
    }

    fn statement(&mut self) -> Option<Statement> {
        let word = if self.at(TokenKind::Name) {
            self.text(self.peek()).to_owned()
        } else {
            String::new()
        };
        if self.nth_kind(1) == TokenKind::Colon && self.at_name() {
            return self.field().map(Statement::Field);
        }
        match word.as_str() {
            "let" => self.binding().map(Statement::Binding),
            "expect" => Some(self.expect_statement()),
            "return" => {
                let start = self.bump().span.start;
                let value = self.expression();
                Some(Statement::Return {
                    value,
                    span: self.span_from(start),
                })
            }
            "if" => Some(Statement::If(self.if_statement())),
            "for" => self.for_statement(),
            _ => {
                self.error_here("expected a statement: `let`, `expect`, `if`, `for` or `return`");
                None
            }
        }
    }

    pub(super) fn binding(&mut self) -> Option<Binding> {
        let start = self.bump().span.start;
        let name = self.declared_name("the binding's name")?;
        let declared_type = if self.eat(TokenKind::Colon).is_some() {
            self.skip_newlines();
            Some(self.type_expression()?)
        } else {
            None
        };
        self.expect(TokenKind::Equals, "`=`")?;
        self.skip_newlines();
        let value = self.expression();
        Some(Binding {
            name,
            declared_type,
            value,
            span: self.span_from(start),
        })
    }

    fn expect_statement(&mut self) -> Statement {
        let start = self.bump().span.start;
        let condition = self.expression();
        if self.word_on_next_line("because") {
            self.skip_newlines();
        }
        let because = self.eat_word("because").map(|_| self.expression());
        Statement::Expect {
            condition,
            because,
            span: self.span_from(start),
        }
    }

    /// Whether the next line starts with `word`: `because` continues an
    /// `expect`, and `else` an `if`.
    fn word_on_next_line(&self, word: &str) -> bool {
        let mut index = self.position;
        while self.tokens[index].kind == TokenKind::Newline {
            index += 1;
        }
        let token = self.tokens[index];
        token.kind == TokenKind::Name && self.text(token) == word
    }

    fn if_statement(&mut self) -> IfStatement {
        let start = self.bump().span.start;
        let condition = self.expression();
        let then = self.block();
        if self.word_on_next_line("else") {
            self.skip_newlines();
        }
        let otherwise = if self.eat_word("else").is_some() {
            if self.at_word("if") {
                Some(Else::If(Box::new(self.if_statement())))
            } else {
                Some(Else::Block(self.block()))
            }
        } else {
            None
        };
        IfStatement {
            condition,
            then,
            otherwise,
            span: self.span_from(start),
        }
    }

    fn for_statement(&mut self) -> Option<Statement> {
        let start = self.bump().span.start;
        let pattern = self.pattern()?;
        self.expect_word("in")?;
        let domain = self.expression();
        let body = self.block();
        Some(Statement::For {
            pattern,
            domain,
            body,
            span: self.span_from(start),
        })
    }

    fn pattern(&mut self) -> Option<Vec<Name>> {
        if !self.at(TokenKind::LeftParen) {
            return Some(vec![self.declared_name("a name for each item")?]);
        }
        let names = self.delimited(TokenKind::LeftParen, TokenKind::RightParen, false, |p| {
            p.comma_separated(TokenKind::RightParen, |p| p.declared_name("a name"))
        });
        Some(names)
    }
}
