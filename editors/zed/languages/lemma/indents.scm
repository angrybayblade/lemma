; Indents for Zed. A node captured @indent indents the lines after its first
; one, up to the token captured @end. Indentation has no meaning in `.lm`;
; this follows the canonical layout in spec 13.

; Members of any bracketed body are indented, and its closer is not.
(_
  "{"
  "}" @end) @indent

(_
  "["
  "]" @end) @indent

(_
  "("
  ")" @end) @indent

; Continuation lines: a rule body after `:-` or `when:`, the right side of a
; binary operator or `->`, and a `because` line.
[
  (rule_clause)
  (when_clause)
  (binary_expression)
  (graph_row)
  (expect_statement)
] @indent
