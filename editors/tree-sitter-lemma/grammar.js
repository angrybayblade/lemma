/**
 * @file Tree-sitter grammar for Lemma's `.lm` language.
 *
 * The syntax contract is `.specs/13-lm-grammar.md`, section 1 (lexical
 * conventions) and section 14 (EBNF and precedence). Rule names follow the
 * EBNF where they can, so the two are easy to compare.
 *
 * Where this grammar accepts more than the EBNF, it does so on purpose, so an
 * editor can still highlight a file the compiler will reject:
 *
 * - The `module` line is optional, because most examples in the spec are
 *   fragments without one.
 * - An `operation` may appear at the top level, as in the spec's identity
 *   example; the compiler reports it as misplaced.
 * - A reserved word may follow `.` as a member name, as in the spec's own
 *   `python.module(...)`.
 * - A newline is only a separator where the grammar can end an item there
 *   (see `src/scanner.c`). Elsewhere it is whitespace.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

module.exports = grammar({
  name: 'lemma',

  // Contextual words such as `service` or `check` are keywords only in parse
  // states that accept them, so `config.service` and `level: check` stay
  // ordinary names. Tree-sitter does this for every keyword it extracts from
  // the word token.
  word: $ => $.identifier,

  // Reserved words are never names, even where only a name could appear, so
  // `let a =` followed by `let b = 2` recovers as two bindings.
  reserved: {
    global: _ => [
      'module', 'import', 'export', 'from', 'as', 'let', 'fn', 'if', 'else',
      'for', 'in', 'return', 'expect', 'because', 'need', 'true', 'false',
      'null', 'self',
    ],
    // After a `.` a name cannot be mistaken for a keyword, and the spec's own
    // examples call `python.module(...)`, so members may be reserved words.
    member: _ => [],
  },

  externals: $ => [
    // A newline that ends an item, statement, field or graph row.
    $._newline,
    // The `{` that opens a well-formed `{name}` or `{name.field}` in a string.
    $._interpolation_start,
    // Never used by a rule. The scanner sees it as valid only while
    // Tree-sitter is recovering from an error.
    $._error_sentinel,
  ],

  extras: $ => [/\s/, $.comment],

  supertypes: $ => [$._expression, $._statement, $._literal],

  conflicts: $ => [
    // `a.b` at the start of a rule literal is a predicate name if `(` or a
    // snapshot follows, and a term otherwise.
    [$.qualified_name, $.term],
    // `run a.b.*`: the module name ends where `.*` begins.
    [$.qualified_name],
  ],

  rules: {
    source_file: $ => seq(
      optional(seq($.module_declaration, optional($._separators))),
      ...lines($, $._item),
    ),

    _separator: $ => choice($._newline, ';'),
    _separators: $ => repeat1($._separator),

    module_declaration: $ => seq('module', field('name', $.qualified_name)),

    _item: $ => choice(
      $.import_declaration,
      $.reexport_declaration,
      $.workspace_declaration,
      $.repo_declaration,
      $.let_declaration,
      $.type_declaration,
      $.entity_declaration,
      $.relation_declaration,
      $.service_declaration,
      $.operation_declaration,
      $.graph_declaration,
      $.rule_declaration,
      $.require_declaration,
      $.allow_declaration,
      $.behavior_declaration,
      $.function_declaration,
      $.checker_declaration,
      $.check_declaration,
    ),

    // Imports

    import_declaration: $ => seq(
      'import',
      choice(
        field('module', $.qualified_name),
        seq($._import_list, 'from', field('module', $.qualified_name)),
        seq($.namespace_import, 'from', field('module', $.qualified_name)),
      ),
    ),

    reexport_declaration: $ => seq(
      'export',
      $._import_list,
      'from',
      field('module', $.qualified_name),
    ),

    // No separator can occur in this comma list, so newlines inside the
    // braces are whitespace, as they are inside `( )` and `[ ]`.
    _import_list: $ => seq('{', commaSeparated($.import_name), optional(','), '}'),

    import_name: $ => seq(
      field('name', $.identifier),
      optional(seq('as', field('alias', $.identifier))),
    ),

    namespace_import: $ => seq('*', 'as', field('alias', $.identifier)),

    // Workspace and repositories

    workspace_declaration: $ => seq(
      'workspace',
      braced($, choice(
        $.repo_declaration,
        $.field,
        alias($._repo_named_field, $.field),
      )),
    ),

    // A workspace member starting with `repo` is a repository unless a `:`
    // follows, so a field may still be called `repo`.
    _repo_named_field: $ => seq(
      field('name', alias('repo', $.identifier)),
      ':',
      field('value', $._expression),
    ),

    repo_declaration: $ => seq(
      'repo',
      field('name', $.identifier),
      '=',
      field('source', $._expression),
    ),

    // Declarations

    let_declaration: $ => seq(optional('export'), $._binding),

    _binding: $ => seq(
      'let',
      field('name', $.identifier),
      optional(seq(':', field('type', $._type))),
      '=',
      field('value', $._expression),
    ),

    type_declaration: $ => seq(
      optional('export'),
      'type',
      field('name', $.identifier),
      '=',
      field('value', $._type),
    ),

    entity_declaration: $ => seq(
      optional('export'),
      'entity',
      field('name', $.identifier),
      braced($, $.typed_field),
    ),

    relation_declaration: $ => seq(
      optional('export'),
      'relation',
      field('name', $.identifier),
      field('parameters', $.parameters),
    ),

    service_declaration: $ => seq(
      optional('export'),
      'service',
      field('name', $.identifier),
      braced($, choice(
        $.service_declaration,
        $.operation_declaration,
        alias($._binding, $.let_declaration),
        $.field,
        alias($._keyword_named_field, $.field),
      )),
    ),

    // A service member starting with `client`, `service` or `operation` is a
    // declaration unless a `:` follows, so `client: http.generated_client(...)`
    // is still a field.
    _keyword_named_field: $ => seq(
      field('name', choice(
        alias('client', $.identifier),
        alias('service', $.identifier),
        alias('operation', $.identifier),
      )),
      ':',
      field('value', $._expression),
    ),

    operation_declaration: $ => seq(
      optional('export'),
      optional('client'),
      'operation',
      field('name', $.identifier),
      braced($, $.field),
    ),

    graph_declaration: $ => seq(
      optional('export'),
      'graph',
      field('name', $.identifier),
      ':',
      field('relation', $._type),
      choice(
        braced($, $.graph_row),
        seq('=', field('value', $._expression)),
      ),
    ),

    // `a -> b` is the tuple `(a, b)`. Every column is a `column` field, so a
    // consumer can read either form the same way.
    graph_row: $ => choice(
      seq(field('column', $._expression), '->', field('column', $._expression)),
      seq(
        '(',
        field('column', $._expression),
        repeat1(seq(',', field('column', $._expression))),
        ')',
      ),
    ),

    // Rules

    rule_declaration: $ => seq(
      optional('export'),
      'rule',
      field('name', $.identifier),
      ':',
      field('relation', $._type),
      braced($, $.rule_clause),
    ),

    rule_clause: $ => seq(
      field('head', $.rule_head),
      ':-',
      field('body', $.rule_body),
    ),

    rule_head: $ => seq('(', commaSeparated($._term), ')'),

    rule_body: $ => commaSeparated($._literal),

    _literal: $ => choice($.predicate, $.comparison),

    predicate: $ => seq(
      optional(field('negation', '!')),
      field('name', $.qualified_name),
      optional(field('snapshot', $.snapshot)),
      '(',
      optional(choice(
        commaSeparated($._term),
        commaSeparated($.named_term),
      )),
      ')',
    ),

    named_term: $ => seq(field('name', $.identifier), ':', field('value', $._term)),

    comparison: $ => seq(
      field('left', $._term),
      field('operator', choice('==', '!=', '<', '<=', '>', '>=')),
      field('right', $._term),
    ),

    _term: $ => choice($.term, $.string, $.integer, $.true, $.false, $.null),

    // Whether a term is a variable depends only on its spelling: a bare name
    // starting with a lowercase letter or `_` is a variable (and `x.field`
    // reads a field of it); a path starting with an uppercase name or `self`
    // is a constant. The highlight and locals queries make that distinction.
    term: $ => seq(
      field('root', choice($.identifier, $.self)),
      repeat(choice(
        seq('.', field('member', reserved('member', $.identifier))),
        seq('[', field('key', $.string), ']'),
      )),
    ),

    // Requirements

    require_declaration: $ => seq(
      optional('export'),
      'require',
      field('name', $.identifier),
      braced($, choice($.when_clause, $.field)),
    ),

    allow_declaration: $ => seq(
      optional('export'),
      'allow',
      field('requirement', $.qualified_name),
      braced($, choice($.when_clause, $.field)),
    ),

    // In `require` and `allow`, the value of `when` is a rule body, not an
    // expression.
    when_clause: $ => seq('when', ':', field('body', $.rule_body)),

    behavior_declaration: $ => seq(
      optional('export'),
      'behavior',
      field('name', $.identifier),
      optional(field('parameters', $.parameters)),
      field('body', $.block),
    ),

    function_declaration: $ => seq(
      optional('export'),
      'fn',
      field('name', $.identifier),
      field('parameters', $.parameters),
      ':',
      field('return_type', $._type),
      field('body', $.block),
    ),

    checker_declaration: $ => seq(
      optional('export'),
      'checker',
      field('name', $.identifier),
      field('parameters', $.parameters),
      ':',
      field('return_type', $._type),
      braced($, $.field),
    ),

    check_declaration: $ => seq(
      optional('export'),
      'check',
      field('name', $.identifier),
      braced($, choice($.run, $.skip)),
    ),

    run: $ => seq(
      'run',
      field('requirement', choice($.qualified_name, $.wildcard)),
      optional(field('arguments', $.arguments)),
      optional(field('overrides', $.record)),
    ),

    skip: $ => seq('skip', field('requirement', $.qualified_name)),

    // `lemma.http.*`: every requirement in a module.
    wildcard: $ => seq(field('module', $.qualified_name), '.', '*'),

    // Statements

    block: $ => braced($, $._statement),

    _statement: $ => choice(
      alias($._binding, $.let_declaration),
      // Only at the start of a behavior; the compiler checks the placement.
      $.field,
      $.expect_statement,
      $.return_statement,
      $.if_statement,
      $.for_statement,
    ),

    expect_statement: $ => seq(
      'expect',
      field('condition', $._expression),
      optional(seq('because', field('reason', $.string))),
    ),

    return_statement: $ => seq('return', field('value', $._expression)),

    if_statement: $ => seq(
      'if',
      field('condition', $._expression),
      field('consequence', $.block),
      optional(seq('else', field('alternative', choice($.block, $.if_statement)))),
    ),

    for_statement: $ => seq(
      'for',
      field('pattern', choice($.identifier, $.tuple_pattern)),
      'in',
      field('domain', $._expression),
      field('body', $.block),
    ),

    tuple_pattern: $ => seq(
      '(',
      $.identifier,
      repeat1(seq(',', $.identifier)),
      ')',
    ),

    // Fields, parameters and types

    field: $ => seq(field('name', $.identifier), ':', field('value', $._expression)),

    parameters: $ => seq(
      '(',
      optional(seq(commaSeparated($.parameter), optional(','))),
      ')',
    ),

    parameter: $ => seq(field('name', $.identifier), ':', field('type', $._type)),

    typed_field: $ => seq(field('name', $.identifier), ':', field('type', $._type)),

    _type: $ => choice($.named_type, $.record_type),

    named_type: $ => seq(
      field('name', $.identifier),
      optional(field('arguments', $.type_arguments)),
    ),

    type_arguments: $ => seq('<', commaSeparated($._type), '>'),

    record_type: $ => braced($, $.typed_field),

    // Expressions. One level per row of the precedence table, lowest first,
    // as in the EBNF. Equality and comparison take operands from the level
    // below, so `a == b == c` does not parse, as the spec requires.

    _expression: $ => $._disjunction,

    _disjunction: $ => choice($._conjunction, alias($.disjunction, $.binary_expression)),
    disjunction: $ => binary($._disjunction, '||', $._conjunction),

    _conjunction: $ => choice($._equality, alias($.conjunction, $.binary_expression)),
    conjunction: $ => binary($._conjunction, '&&', $._equality),

    _equality: $ => choice($._comparison, alias($.equality, $.binary_expression)),
    equality: $ => binary($._comparison, choice('==', '!='), $._comparison),

    _comparison: $ => choice($._additive, alias($.ordering, $.binary_expression)),
    ordering: $ => binary($._additive, choice('<', '<=', '>', '>='), $._additive),

    _additive: $ => choice($._multiplicative, alias($.additive, $.binary_expression)),
    additive: $ => binary($._additive, choice('+', '-'), $._multiplicative),

    _multiplicative: $ => choice($._unary, alias($.multiplicative, $.binary_expression)),
    multiplicative: $ => binary($._multiplicative, choice('*', '/', '%'), $._unary),

    _unary: $ => choice($._postfix, $.unary_expression, $.need_expression),

    unary_expression: $ => seq(
      field('operator', choice('!', '-')),
      field('operand', $._unary),
    ),

    // `need e` evaluates evidence, so it gets its own node.
    need_expression: $ => seq('need', field('evidence', $._unary)),

    _postfix: $ => choice(
      $._primary,
      $.member_expression,
      $.snapshot_expression,
      $.index_expression,
      $.call_expression,
    ),

    member_expression: $ => seq(
      field('object', $._postfix),
      '.',
      field('member', reserved('member', $.identifier)),
    ),

    snapshot_expression: $ => seq(
      field('object', $._postfix),
      field('snapshot', $.snapshot),
    ),

    index_expression: $ => seq(
      field('object', $._postfix),
      '[',
      commaSeparated(field('index', $._expression)),
      ']',
    ),

    call_expression: $ => seq(
      field('function', $._postfix),
      field('arguments', $.arguments),
    ),

    arguments: $ => seq(
      '(',
      optional(seq(commaSeparated(choice($._expression, $.named_argument)), optional(','))),
      ')',
    ),

    named_argument: $ => seq(
      field('name', $.identifier),
      ':',
      field('value', $._expression),
    ),

    _primary: $ => choice(
      $.integer,
      $.string,
      $.true,
      $.false,
      $.null,
      $.self,
      $.identifier,
      $.list,
      $.record,
      $.parenthesized_expression,
    ),

    parenthesized_expression: $ => seq('(', $._expression, ')'),

    list: $ => seq(
      '[',
      optional(seq(commaSeparated($._expression), optional(','))),
      ']',
    ),

    record: $ => braced($, $.field),

    // Lexical

    qualified_name: $ => seq($.identifier, repeat(seq('.', $.identifier))),

    snapshot: _ => choice('@base', '@head'),

    // Letters are ASCII. A backticked name may hold anything but a backtick or
    // a newline, such as a field called `in`.
    identifier: _ => /[A-Za-z_][A-Za-z0-9_]*|`[^`\n]+`/,

    // 64-bit signed integers. The sign belongs to the literal so a rule term
    // can be negative; in an expression `-x` is a unary minus.
    integer: _ => /-?[0-9]+/,

    true: _ => 'true',
    false: _ => 'false',
    null: _ => 'null',
    self: _ => 'self',

    string: $ => seq(
      '"',
      repeat(choice(
        $.string_content,
        $.escape_sequence,
        $.interpolation,
      )),
      '"',
    ),

    // A `{` or `}` that does not open an interpolation or write a brace is
    // plain text, so dispatch keys such as "GET /users/{}" parse cleanly.
    string_content: _ => choice(
      token.immediate(prec(1, /[^"\\{}\n]+/)),
      token.immediate(prec(1, /[{}]/)),
    ),

    // JSON escapes, plus `{{` and `}}` for literal braces.
    escape_sequence: _ => choice(
      token.immediate(prec(2, /\\(["\\/bfnrt]|u[0-9a-fA-F]{4})/)),
      token.immediate(prec(2, '{{')),
      token.immediate(prec(2, '}}')),
    ),

    // `{name}` or `{name.field}`. Only `message:` and `because` strings
    // interpolate; editors highlight the form in every string.
    interpolation: $ => seq(
      alias($._interpolation_start, '{'),
      field('name', alias(token.immediate(/[A-Za-z_][A-Za-z0-9_]*/), $.identifier)),
      optional(seq(
        token.immediate('.'),
        field('member', alias(token.immediate(/[A-Za-z_][A-Za-z0-9_]*/), $.identifier)),
      )),
      token.immediate('}'),
    ),

    comment: _ => token(choice(
      seq('//', /[^\n]*/),
      seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/'),
    )),
  },
});

/**
 * Zero or more `rule`, each ended by one or more separators, except that the
 * last may stand without one: a closing `}` or the end of the file ends it.
 *
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} rule
 * @returns {RuleOrLiteral[]}
 */
function lines($, rule) {
  return [repeat(seq(rule, $._separators)), optional(rule)];
}

/**
 * A `{ ... }` body of separator-delimited members.
 *
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} member
 * @returns {SeqRule}
 */
function braced($, member) {
  return seq('{', ...lines($, member), '}');
}

/**
 * One or more `rule`, separated by commas.
 *
 * @param {RuleOrLiteral} rule
 * @returns {SeqRule}
 */
function commaSeparated(rule) {
  return seq(rule, repeat(seq(',', rule)));
}

/**
 * A binary operation whose operands come from the given precedence levels.
 *
 * @param {RuleOrLiteral} left
 * @param {RuleOrLiteral} operator
 * @param {RuleOrLiteral} right
 * @returns {SeqRule}
 */
function binary(left, operator, right) {
  return seq(
    field('left', left),
    field('operator', operator),
    field('right', right),
  );
}
