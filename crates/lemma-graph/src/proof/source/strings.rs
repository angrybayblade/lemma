//! String literals, decoded from the grammar's fragments, escapes and
//! interpolations.

use tree_sitter::Node;

use super::super::ir::{Expression as Expr, Part};
use super::{Language, Reader, children, field, only};

impl Reader<'_> {
    /// A string literal without interpolation, such as a record key.
    pub(super) fn string(&self, node: Node<'_>) -> Result<String, String> {
        let mut text = String::new();
        for part in self.text_parts(node)? {
            let Part::Text(piece) = part else {
                return self.unsupported(node);
            };
            text.push_str(&piece);
        }
        Ok(text)
    }

    /// A string literal, or a formatted string where values are interpolated.
    pub(super) fn text_expression(&self, node: Node<'_>) -> Result<Expr, String> {
        let parts = self.text_parts(node)?;
        if parts.iter().all(|part| matches!(part, Part::Text(_))) {
            return self.string(node).map(Expr::Text);
        }
        Ok(Expr::Template(parts))
    }

    /// The pieces of a string literal, decoded from the grammar's fragments,
    /// escapes and interpolations rather than from its source text.
    pub(super) fn text_parts(&self, node: Node<'_>) -> Result<Vec<Part>, String> {
        if !matches!(
            node.kind(),
            "string"
                | "concatenated_string"
                | "template_string"
                | "interpreted_string_literal"
                | "raw_string_literal"
        ) {
            return self.unsupported(node);
        }
        if node.kind() == "concatenated_string" {
            let mut parts = Vec::new();
            for piece in children(node) {
                parts.extend(self.text_parts(piece)?);
            }
            return Ok(parts);
        }
        if self.language == Language::Python {
            return self.python_string(node);
        }
        let mut parts = Vec::new();
        for piece in children(node) {
            let part = match piece.kind() {
                "string_fragment" | "interpreted_string_literal_content" => {
                    Part::Text(self.text(piece).to_owned())
                }
                // Go discards carriage returns inside raw strings.
                "raw_string_literal_content" if !self.text(piece).contains('\r') => {
                    Part::Text(self.text(piece).to_owned())
                }
                "escape_sequence" => Part::Text(
                    decode_escapes(self.text(piece), self.language)
                        .ok_or_else(|| self.refusal(piece))?,
                ),
                "template_substitution" => Part::Value(self.expression(only(piece)?)?),
                _ => return self.unsupported(piece),
            };
            parts.push(part);
        }
        Ok(parts)
    }

    pub(super) fn python_string(&self, node: Node<'_>) -> Result<Vec<Part>, String> {
        let start = node
            .named_child(0)
            .filter(|start| start.kind() == "string_start")
            .ok_or("missing string start")?;
        let prefix = self
            .text(start)
            .trim_end_matches(['\'', '"'])
            .to_ascii_lowercase();
        // Bytes are not text.
        if prefix.contains('b') {
            return self.unsupported(node);
        }
        let is_raw = prefix.contains('r');
        let is_formatted = prefix.contains('f');
        let mut parts = Vec::new();
        for piece in children(node) {
            match piece.kind() {
                "string_start" | "string_end" => {}
                "string_content" => {
                    parts.push(Part::Text(self.python_content(
                        piece,
                        is_raw,
                        is_formatted,
                    )?));
                }
                // Conversions, format specifications and `{x=}` spell values
                // in ways each needs its own reading.
                "interpolation"
                    if piece.child_by_field_name("type_conversion").is_none()
                        && piece.child_by_field_name("format_specifier").is_none()
                        && !self
                            .text(piece)
                            .trim_end_matches('}')
                            .trim_end()
                            .ends_with('=') =>
                {
                    parts.push(Part::Value(self.expression(field(piece, "expression")?)?));
                }
                _ => return self.unsupported(piece),
            }
        }
        Ok(parts)
    }

    pub(super) fn python_content(
        &self,
        piece: Node<'_>,
        is_raw: bool,
        is_formatted: bool,
    ) -> Result<String, String> {
        let text = if is_raw {
            self.text(piece).to_owned()
        } else {
            decode_escapes(self.text(piece), Language::Python).ok_or_else(|| self.refusal(piece))?
        };
        if !is_formatted {
            return Ok(text);
        }
        Ok(text.replace("{{", "{").replace("}}", "}"))
    }
}

/// Decodes the escapes in a piece of string literal, or `None` for one this
/// reader does not decode, rather than guessing what the language does.
fn decode_escapes(text: &str, language: Language) -> Option<String> {
    let mut decoded = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        let escape = characters.next()?;
        let simple = match escape {
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            '\\' | '\'' | '"' => Some(escape),
            'b' => Some('\u{8}'),
            'f' => Some('\u{c}'),
            'v' => Some('\u{b}'),
            'a' if language != Language::TypeScript => Some('\u{7}'),
            '0' if language == Language::TypeScript
                && !characters.peek().is_some_and(char::is_ascii_digit) =>
            {
                Some('\0')
            }
            _ => None,
        };
        if let Some(simple) = simple {
            decoded.push(simple);
            continue;
        }
        let digits = match (escape, language) {
            ('x', _) => 2,
            ('u', Language::TypeScript) if characters.peek() == Some(&'{') => {
                characters.next();
                let hex: String = characters.by_ref().take_while(|c| *c != '}').collect();
                decoded.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                continue;
            }
            ('u', _) => 4,
            ('U', Language::Python | Language::Go) => 8,
            // A backslash before a line break continues the line.
            ('\n', Language::TypeScript | Language::Python) => continue,
            _ => return None,
        };
        let hex: String = characters.by_ref().take(digits).collect();
        if hex.len() != digits {
            return None;
        }
        decoded.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
    }
    Some(decoded)
}
