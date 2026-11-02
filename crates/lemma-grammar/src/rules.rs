//! Parsing rule text: Lemma's Datalog syntax, shared by YAML grammars and,
//! later, `.ls` files.
//!
//! ```text
//! removed_field(op, f) :- returns@base(op, f), !returns@head(op, f).
//! over_budget(c) :- rate(c, r), r > 120.
//! ```
//!
//! Identifiers in atoms are variables, `_` matches anything, `"text"` is a
//! symbol and digits are numbers. `@base` or `@head` after a relation
//! selects a snapshot; without one the head snapshot is meant. The final
//! period is optional.

use lemma_logic::Comparison;

/// A term as written, before symbols are interned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawTerm {
    /// A variable.
    Variable(String),
    /// `_`.
    Wildcard,
    /// A quoted string.
    Text(String),
    /// An integer.
    Number(i64),
}

/// A relation applied to terms, with an optional snapshot qualifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawAtom {
    /// The relation's name.
    pub relation: String,
    /// `base` or `head`, if written.
    pub snapshot: Option<String>,
    /// The terms.
    pub terms: Vec<RawTerm>,
}

/// One condition in a body, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawLiteral {
    /// The atom holds.
    Positive(RawAtom),
    /// The atom does not hold.
    Negative(RawAtom),
    /// A comparison.
    Compare(RawTerm, Comparison, RawTerm),
}

/// A rule as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRule {
    /// What it derives.
    pub head: RawAtom,
    /// Its conditions.
    pub body: Vec<RawLiteral>,
}

/// Where and why rule text failed to parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message} at character {position}")]
pub struct ParseError {
    /// What was expected.
    pub message: String,
    /// Byte offset in the text.
    pub position: usize,
}

/// Parses a whole rule: `head :- body`.
pub fn parse_rule(text: &str) -> Result<RawRule, ParseError> {
    let mut parser = Parser::new(text);
    let head = parser.atom()?;
    parser.expect(":-")?;
    let body = parser.body()?;
    Ok(RawRule { head, body })
}

/// Parses a body alone, as in a requirement's `when`.
pub fn parse_body(text: &str) -> Result<Vec<RawLiteral>, ParseError> {
    Parser::new(text).body()
}

struct Parser<'a> {
    text: &'a str,
    position: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, position: 0 }
    }

    fn body(&mut self) -> Result<Vec<RawLiteral>, ParseError> {
        let mut literals = vec![self.literal()?];
        while self.eat(",") {
            literals.push(self.literal()?);
        }
        self.eat(".");
        self.skip_space();
        if self.position < self.text.len() {
            return Err(self.error("expected `,` or the end of the rule"));
        }
        Ok(literals)
    }

    fn literal(&mut self) -> Result<RawLiteral, ParseError> {
        if self.eat("!") {
            return Ok(RawLiteral::Negative(self.atom()?));
        }
        let start = self.position;
        let left = self.term()?;
        // A name followed by `(` or `@snapshot(` starts an atom.
        if matches!(left, RawTerm::Variable(_)) && (self.peek("(") || self.peek("@")) {
            self.position = start;
            return Ok(RawLiteral::Positive(self.atom()?));
        }
        let op = self.comparison()?;
        let right = self.term()?;
        Ok(RawLiteral::Compare(left, op, right))
    }

    fn atom(&mut self) -> Result<RawAtom, ParseError> {
        let relation = self.identifier()?;
        let snapshot = if self.eat("@") {
            Some(self.identifier()?)
        } else {
            None
        };
        self.expect("(")?;
        let mut terms = Vec::new();
        if !self.eat(")") {
            terms.push(self.term()?);
            while self.eat(",") {
                terms.push(self.term()?);
            }
            self.expect(")")?;
        }
        Ok(RawAtom {
            relation,
            snapshot,
            terms,
        })
    }

    fn term(&mut self) -> Result<RawTerm, ParseError> {
        self.skip_space();
        let rest = &self.text[self.position..];
        if rest.starts_with('"') {
            return self.text_literal();
        }
        if rest.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
            return self.number();
        }
        let name = self.identifier()?;
        Ok(if name == "_" {
            RawTerm::Wildcard
        } else {
            RawTerm::Variable(name)
        })
    }

    fn text_literal(&mut self) -> Result<RawTerm, ParseError> {
        let start = self.position + 1;
        let Some(length) = self.text[start..].find('"') else {
            return Err(self.error("unterminated string"));
        };
        self.position = start + length + 1;
        Ok(RawTerm::Text(self.text[start..start + length].to_owned()))
    }

    fn number(&mut self) -> Result<RawTerm, ParseError> {
        let start = self.position;
        let rest = &self.text[start..];
        let length = rest
            .char_indices()
            .take_while(|(index, c)| c.is_ascii_digit() || (*index == 0 && *c == '-'))
            .count();
        self.position += length;
        rest[..length]
            .parse()
            .map(RawTerm::Number)
            .map_err(|_| self.error("expected a number"))
    }

    fn comparison(&mut self) -> Result<Comparison, ParseError> {
        let operators = [
            ("!=", Comparison::NotEqual),
            ("<=", Comparison::LessOrEqual),
            (">=", Comparison::GreaterOrEqual),
            ("=", Comparison::Equal),
            ("<", Comparison::Less),
            (">", Comparison::Greater),
        ];
        for (symbol, op) in operators {
            if self.eat(symbol) {
                return Ok(op);
            }
        }
        Err(self.error("expected an atom or a comparison"))
    }

    fn identifier(&mut self) -> Result<String, ParseError> {
        self.skip_space();
        let rest = &self.text[self.position..];
        let length = rest
            .char_indices()
            .take_while(|(index, c)| {
                c.is_ascii_alphabetic() || *c == '_' || (*index > 0 && c.is_ascii_digit())
            })
            .count();
        if length == 0 {
            return Err(self.error("expected a name"));
        }
        self.position += length;
        Ok(rest[..length].to_owned())
    }

    fn expect(&mut self, token: &str) -> Result<(), ParseError> {
        if self.eat(token) {
            return Ok(());
        }
        Err(self.error(&format!("expected `{token}`")))
    }

    fn eat(&mut self, token: &str) -> bool {
        self.skip_space();
        if self.text[self.position..].starts_with(token) {
            self.position += token.len();
            return true;
        }
        false
    }

    fn peek(&mut self, token: &str) -> bool {
        self.skip_space();
        self.text[self.position..].starts_with(token)
    }

    fn skip_space(&mut self) {
        let rest = &self.text[self.position..];
        self.position += rest.len() - rest.trim_start().len();
    }

    fn error(&self, message: &str) -> ParseError {
        ParseError {
            message: message.to_owned(),
            position: self.position,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(name: &str) -> RawTerm {
        RawTerm::Variable(name.to_owned())
    }

    #[test]
    fn rules_with_negation_and_snapshots_parse() {
        let rule = parse_rule("removed_field(op, f) :- returns@base(op, f), !returns@head(op, f).")
            .unwrap();

        assert_eq!(rule.head.relation, "removed_field");
        assert_eq!(rule.body.len(), 2);
        let RawLiteral::Negative(atom) = &rule.body[1] else {
            panic!("expected a negation");
        };
        assert_eq!(atom.snapshot.as_deref(), Some("head"));
        assert_eq!(atom.terms, [variable("op"), variable("f")]);
    }

    #[test]
    fn comparisons_constants_and_wildcards_parse() {
        let body = parse_body(r#"rate(c, r, _), r > 120, c != "dashboard""#).unwrap();

        assert_eq!(
            body[1],
            RawLiteral::Compare(variable("r"), Comparison::Greater, RawTerm::Number(120))
        );
        assert_eq!(
            body[2],
            RawLiteral::Compare(
                variable("c"),
                Comparison::NotEqual,
                RawTerm::Text("dashboard".to_owned())
            )
        );
        let RawLiteral::Positive(atom) = &body[0] else {
            panic!("expected an atom");
        };
        assert_eq!(atom.terms[2], RawTerm::Wildcard);
    }

    #[test]
    fn errors_point_at_the_problem() {
        let error = parse_rule("broken(c) :- reads(c,").unwrap_err();

        assert_eq!(error.position, 21);
        assert!(error.message.contains("name"));
    }

    #[test]
    fn a_missing_arrow_is_reported() {
        let error = parse_rule("broken(c) reads(c)").unwrap_err();

        assert!(error.message.contains(":-"));
    }
}
