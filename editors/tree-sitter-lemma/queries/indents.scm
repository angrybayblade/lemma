; Indents for Lemma's `.lm` language, in nvim-treesitter's convention.
; Indentation has no meaning in `.lm`; these only keep a file tidy.

; Members of any bracketed body are indented, and its closer is not.
(_
  "{"
  "}" @indent.end) @indent.begin

(_
  "["
  "]" @indent.end) @indent.begin

(_
  "("
  ")" @indent.end) @indent.begin

[
  "}"
  "]"
  ")"
] @indent.branch

; Continuation lines: a rule body after `:-` or `when:`, the right side of a
; binary operator or `->`, and a `because` line.
[
  (rule_clause)
  (when_clause)
  (binary_expression)
  (graph_row)
  (expect_statement)
] @indent.begin

(comment) @indent.auto

(string) @indent.ignore
