; Scopes, definitions and references for Lemma's `.lm` language.

; Scopes

[
  (source_file)
  (service_declaration)
  (function_declaration)
  (behavior_declaration)
  (checker_declaration)
  (relation_declaration)
  (rule_declaration)
  (require_declaration)
  (allow_declaration)
  (rule_clause)
  (block)
  (for_statement)
] @local.scope

; Definitions

(let_declaration
  name: (identifier) @local.definition.var)

(parameter
  name: (identifier) @local.definition.parameter)

(for_statement
  pattern: (identifier) @local.definition.var)

(tuple_pattern
  (identifier) @local.definition.var)

(import_name
  name: (identifier) @local.definition.import
  !alias)

(import_name
  alias: (identifier) @local.definition.import)

(namespace_import
  alias: (identifier) @local.definition.namespace)

(repo_declaration
  name: (identifier) @local.definition.namespace)

[
  (type_declaration
    name: (identifier) @local.definition.type)
  (entity_declaration
    name: (identifier) @local.definition.type)
  (relation_declaration
    name: (identifier) @local.definition.type)
  (service_declaration
    name: (identifier) @local.definition.type)
]

[
  (function_declaration
    name: (identifier) @local.definition.function)
  (checker_declaration
    name: (identifier) @local.definition.function)
  (graph_declaration
    name: (identifier) @local.definition.function)
  (rule_declaration
    name: (identifier) @local.definition.function)
]

; A rule variable is a lowercase bare name in a term, scoped to its clause
; (or to the `require` or `allow` whose `when:` binds it). Its binding
; occurrences are those inside a predicate.
((predicate
  (term
    root: (identifier) @local.definition.var))
  (#match? @local.definition.var "^[a-z_]"))

((predicate
  (named_term
    value: (term
      root: (identifier) @local.definition.var)))
  (#match? @local.definition.var "^[a-z_]"))

; References: names read as values. Member names, field names and argument
; names are not references.

[
  (let_declaration
    value: (identifier) @local.reference)
  (repo_declaration
    source: (identifier) @local.reference)
  (graph_declaration
    value: (identifier) @local.reference)
  (graph_row
    column: (identifier) @local.reference)
  (field
    value: (identifier) @local.reference)
  (named_argument
    value: (identifier) @local.reference)
  (arguments
    (identifier) @local.reference)
  (list
    (identifier) @local.reference)
  (parenthesized_expression
    (identifier) @local.reference)
  (member_expression
    object: (identifier) @local.reference)
  (snapshot_expression
    object: (identifier) @local.reference)
  (index_expression
    object: (identifier) @local.reference)
  (index_expression
    index: (identifier) @local.reference)
  (call_expression
    function: (identifier) @local.reference)
  (binary_expression
    left: (identifier) @local.reference)
  (binary_expression
    right: (identifier) @local.reference)
  (unary_expression
    operand: (identifier) @local.reference)
  (need_expression
    evidence: (identifier) @local.reference)
  (expect_statement
    condition: (identifier) @local.reference)
  (return_statement
    value: (identifier) @local.reference)
  (if_statement
    condition: (identifier) @local.reference)
  (for_statement
    domain: (identifier) @local.reference)
  (named_type
    name: (identifier) @local.reference)
  (interpolation
    name: (identifier) @local.reference)
]

((term
  root: (identifier) @local.reference)
  (#match? @local.reference "^[a-z_]"))
