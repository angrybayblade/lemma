//! Expressions, by precedence climbing, and written types.

use super::Parser;
use crate::syntax::ast::{
    Argument, BinaryOperator, Expression, ExpressionKind, TypeExpression, UnaryOperator,
};
use crate::syntax::lexer::{TokenKind, check_escapes, decode_text};

/// Precedence levels, lowest first. Equality and comparison do not chain.
const LEVELS: [&[(TokenKind, BinaryOperator)]; 6] = [
    &[(TokenKind::OrOr, BinaryOperator::Or)],
    &[(TokenKind::AndAnd, BinaryOperator::And)],
    &[
        (TokenKind::EqualEqual, BinaryOperator::Equal),
        (TokenKind::BangEqual, BinaryOperator::NotEqual),
    ],
    &[
        (TokenKind::Less, BinaryOperator::Less),
        (TokenKind::LessEqual, BinaryOperator::LessOrEqual),
        (TokenKind::Greater, BinaryOperator::Greater),
        (TokenKind::GreaterEqual, BinaryOperator::GreaterOrEqual),
    ],
    &[
        (TokenKind::Plus, BinaryOperator::Add),
        (TokenKind::Minus, BinaryOperator::Subtract),
    ],
    &[
        (TokenKind::Star, BinaryOperator::Multiply),
        (TokenKind::Slash, BinaryOperator::Divide),
        (TokenKind::Percent, BinaryOperator::Remainder),
    ],
];

/// Levels whose operators do not chain: `a == b == c` is an error.
const NON_ASSOCIATIVE: [usize; 2] = [2, 3];

impl Parser<'_> {
    pub(crate) fn expression(&mut self) -> Expression {
        self.binary(0)
    }

    fn binary(&mut self, level: usize) -> Expression {
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1);
        let mut chained = 0;
        while let Some(operator) = self.operator_at(level) {
            chained += 1;
            if chained > 1 && NON_ASSOCIATIVE.contains(&level) {
                self.error_here("comparisons do not chain; join them with `&&`");
            }
            self.bump();
            self.skip_newlines();
            let right = self.binary(level + 1);
            let span = left.span.to(right.span);
            left = Expression {
                kind: ExpressionKind::Binary {
                    operator,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            };
        }
        left
    }

    fn operator_at(&self, level: usize) -> Option<BinaryOperator> {
        let kind = self.peek_kind();
        LEVELS[level]
            .iter()
            .find(|(token, _)| *token == kind)
            .map(|(_, operator)| *operator)
    }

    /// `>`, `>=` and the rest, as a rule comparison.
    pub(super) fn comparison_operator(&mut self) -> Option<BinaryOperator> {
        let kind = self.peek_kind();
        let operator = LEVELS[2]
            .iter()
            .chain(LEVELS[3].iter())
            .find(|(token, _)| *token == kind)
            .map(|(_, operator)| *operator)?;
        self.bump();
        Some(operator)
    }

    fn unary(&mut self) -> Expression {
        let start = self.peek().span.start;
        let operator = if self.eat(TokenKind::Bang).is_some() {
            Some(UnaryOperator::Not)
        } else if self.eat(TokenKind::Minus).is_some() {
            Some(UnaryOperator::Negate)
        } else if self.eat_word("need").is_some() {
            Some(UnaryOperator::Need)
        } else {
            None
        };
        let Some(operator) = operator else {
            return self.postfix();
        };
        let operand = self.unary();
        Expression {
            kind: ExpressionKind::Unary {
                operator,
                operand: Box::new(operand),
            },
            span: self.span_from(start),
        }
    }

    fn postfix(&mut self) -> Expression {
        let mut expression = self.primary();
        loop {
            match self.postfix_step(expression) {
                Ok(extended) => expression = extended,
                Err(finished) => return finished,
            }
        }
    }

    /// Extends `expression` by one member access, snapshot, index or call;
    /// `Err` gives it back unchanged when none follows.
    fn postfix_step(&mut self, expression: Expression) -> Result<Expression, Expression> {
        let start = expression.span.start;
        let kind = match self.peek_kind() {
            TokenKind::Dot => {
                self.bump();
                let Some(name) = self.name("a member name after `.`") else {
                    return Err(expression);
                };
                ExpressionKind::Member {
                    target: Box::new(expression),
                    name,
                }
            }
            TokenKind::Snapshot => {
                let Some(snapshot) = self.snapshot() else {
                    return Err(expression);
                };
                ExpressionKind::Snapshot {
                    target: Box::new(expression),
                    snapshot,
                }
            }
            TokenKind::LeftBracket => ExpressionKind::Index {
                target: Box::new(expression),
                keys: self.bracketed_list(),
            },
            TokenKind::LeftParen => ExpressionKind::Call {
                callee: Box::new(expression),
                arguments: self.call_arguments(),
            },
            _ => return Err(expression),
        };
        Ok(Expression {
            kind,
            span: self.span_from(start),
        })
    }

    /// `[a, b, ...]`.
    fn bracketed_list(&mut self) -> Vec<Expression> {
        self.delimited(
            TokenKind::LeftBracket,
            TokenKind::RightBracket,
            false,
            |parser| parser.expressions_until(TokenKind::RightBracket),
        )
    }

    /// Expressions separated by commas until `close`.
    pub(super) fn expressions_until(&mut self, close: TokenKind) -> Vec<Expression> {
        self.comma_separated(close, |parser| Some(parser.expression()))
    }

    /// `(name: value, value, ...)`.
    pub(super) fn call_arguments(&mut self) -> Vec<Argument> {
        self.delimited(
            TokenKind::LeftParen,
            TokenKind::RightParen,
            false,
            |parser| parser.comma_separated(TokenKind::RightParen, |p| Some(p.argument())),
        )
    }

    fn argument(&mut self) -> Argument {
        let name = if self.at_name() && self.nth_kind(1) == TokenKind::Colon {
            let name = self.name("an argument name");
            self.bump();
            name
        } else {
            None
        };
        Argument {
            name,
            value: self.expression(),
        }
    }

    fn primary(&mut self) -> Expression {
        let token = self.peek();
        let kind = match token.kind {
            TokenKind::Integer => {
                self.bump();
                self.integer_value(token)
            }
            TokenKind::Text => {
                self.bump();
                let raw = self.text(token).to_owned();
                self.diagnostics.extend(check_escapes(&raw, token.span));
                ExpressionKind::Text(decode_text(&raw))
            }
            TokenKind::Name | TokenKind::EscapedName => self.word(),
            TokenKind::LeftBracket => ExpressionKind::List(self.bracketed_list()),
            TokenKind::LeftBrace => ExpressionKind::Record(self.field_block()),
            TokenKind::LeftParen => {
                let inner = self.delimited(
                    TokenKind::LeftParen,
                    TokenKind::RightParen,
                    false,
                    Parser::expression,
                );
                inner.kind
            }
            _ => {
                self.error_here("expected a value");
                ExpressionKind::Missing
            }
        };
        Expression {
            kind,
            span: self.span_from(token.span.start),
        }
    }

    fn integer_value(&mut self, token: crate::syntax::lexer::Token) -> ExpressionKind {
        if let Ok(value) = self.text(token).parse() {
            return ExpressionKind::Integer(value);
        }
        self.error_at(token.span, "this integer does not fit in 64 bits");
        ExpressionKind::Integer(0)
    }

    fn word(&mut self) -> ExpressionKind {
        let is_escaped = self.at(TokenKind::EscapedName);
        let Some(name) = self.name("a value") else {
            return ExpressionKind::Missing;
        };
        if is_escaped {
            return ExpressionKind::Name(name.text);
        }
        match name.text.as_str() {
            "true" => ExpressionKind::Boolean(true),
            "false" => ExpressionKind::Boolean(false),
            "null" => ExpressionKind::Null,
            "self" => ExpressionKind::SelfModule,
            _ => ExpressionKind::Name(name.text),
        }
    }

    /// `Name`, `Name<Arguments>` or `{ field: Type ... }`.
    pub(crate) fn type_expression(&mut self) -> Option<TypeExpression> {
        let start = self.peek().span.start;
        if self.at(TokenKind::LeftBrace) {
            let fields = self.typed_field_block();
            return Some(TypeExpression::Record {
                fields,
                span: self.span_from(start),
            });
        }
        let name = self.name("a type")?;
        let arguments = if self.at(TokenKind::Less) {
            self.delimited(TokenKind::Less, TokenKind::Greater, false, |parser| {
                parser.comma_separated(TokenKind::Greater, Parser::type_expression)
            })
        } else {
            Vec::new()
        };
        Some(TypeExpression::Named {
            name,
            arguments,
            span: self.span_from(start),
        })
    }
}
