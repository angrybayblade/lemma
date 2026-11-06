/**
 * @file Tree-sitter grammar for Lemma's `.lm` language.
 *
 * The syntax contract is `.specs/13-lm-grammar.md`, section 1 (lexical
 * conventions) and section 14 (EBNF and precedence), as the parser in
 * `crates/lemma-lang/src/syntax` reads it. Rule names follow the EBNF where
 * they can, so the two are easy to compare.
 *
 * Where this grammar accepts more than that parser, it does so on purpose, so
 * an editor can still highlight a file the compiler will reject:
 *
 * - The `module` line is optional, because most examples in the spec are
 *   fragments without one.
 * - A newline right after a unary operator or a member's `.` is whitespace,
 *   since those rules also occur inside brackets (see `sameLine` below).
 *
 * editors/README.md lists every difference from that parser.
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
    // Never produced. Where a rule accepts it, the next token must be on the
    // same line; see `sameLine` below and `src/scanner.c`.
    $._same_line,
    // Never used by a rule. The scanner produces it for a newline that breaks
    // a line before a `{`, such as a body's, so error recovery skips it.
    $._misplaced_newline,
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
    // The `module` line, when there is one, ends with a separator like any
    // other item.
    source_file: $ => seq(
      optional($._separators),
      choice(
        seq(optional(seq($.module_declaration, $._separators)), ...lines($._separators, $._item)),
        $.module_declaration,
      ),
    ),

    _separators: $ => repeat1(choice($._newline, ';')),

    // In field and typed-field blocks a comma separates too:
    // `{ include: ["src"], exclude: ["vendor"] }`.
    _field_separators: $ => repeat1(choice($._newline, ';', ',')),

    module_declaration: $ => seq('module', sameLine($), field('name', $.qualified_name)),

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
      sameLine($),
      choice(
        field('module', $.qualified_name),
        seq($._import_list, $._from_module),
        seq($.namespace_import, $._from_module),
      ),
    ),

    reexport_declaration: $ => seq(
      'export',
      sameLine($),
      $._import_list,
      $._from_module,
    ),

    _from_module: $ => seq(
      sameLine($),
      'from',
      sameLine($),
      field('module', $.qualified_name),
    ),

    // No separator can occur in this comma list, so newlines inside the
    // braces are whitespace, as they are inside `( )` and `[ ]`.
    _import_list: $ => seq('{', optionalCommaList($.import_name), '}'),

    import_name: $ => seq(
      field('name', $.identifier),
      optional(seq('as', field('alias', $.identifier))),
    ),

    namespace_import: $ => seq('*', sameLine($), 'as', sameLine($), field('alias', $.identifier)),

    // Workspace and repositories

    workspace_declaration: $ => seq(
      'workspace',
      sameLine($),
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
      sameLine($),
      ':',
      field('value', $._expression),
    ),

    repo_declaration: $ => seq(
      'repo',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      '=',
      field('source', $._expression),
    ),

    // Declarations

    let_declaration: $ => seq(exported($), $._binding),

    _binding: $ => seq(
      'let',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      optional(seq(':', field('type', $._type), sameLine($))),
      '=',
      field('value', $._expression),
    ),

    type_declaration: $ => seq(
      exported($),
      'type',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      '=',
      field('value', $._type),
    ),

    entity_declaration: $ => seq(
      exported($),
      'entity',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      bracedFields($, $.typed_field),
    ),

    relation_declaration: $ => seq(
      exported($),
      'relation',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      field('parameters', $.parameters),
    ),

    service_declaration: $ => seq(
      exported($),
      'service',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
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
      sameLine($),
      ':',
      field('value', $._expression),
    ),

    operation_declaration: $ => seq(
      exported($),
      optional(seq('client', sameLine($))),
      'operation',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      bracedFields($, $.field),
    ),

    graph_declaration: $ => seq(
      exported($),
      'graph',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      ':',
      field('relation', $._type),
      sameLine($),
      choice(
        braced($, $.graph_row),
        seq('=', field('value', $._expression)),
      ),
    ),

    // A row is one value for a one-column relation, `a -> b` for the tuple
    // `(a, b)`, or a tuple. Every column is a `column` field, so a consumer
    // can read each form the same way. A row starting with `(` is a tuple,
    // as the Rust parser reads it, so `(a)` is a one-column tuple and
    // `(a) -> b` is an error: the tuple's `(` is a token of its own, which
    // wins over the `(` of a parenthesised expression where both could start.
    graph_row: $ => choice(
      seq(
        field('column', $._expression),
        optional(seq('->', field('column', $._expression))),
      ),
      seq(
        alias(token(prec(1, '(')), '('),
        optionalCommaList(field('column', $._expression)),
        ')',
      ),
    ),

    // Rules

    // A rule borrows a relation's columns, `rule r : Relation`, or names its
    // own, `rule r(a: Text, b: Int)`.
    rule_declaration: $ => seq(
      exported($),
      'rule',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      choice(
        seq(':', field('relation', $._type)),
        field('parameters', $.parameters),
      ),
      sameLine($),
      braced($, $.rule_clause),
    ),

    rule_clause: $ => seq(
      field('head', $.rule_head),
      sameLine($),
      ':-',
      field('body', $.rule_body),
    ),

    rule_head: $ => seq('(', optionalCommaList($._term), ')'),

    rule_body: $ => commaSeparated($._literal),

    _literal: $ => choice($.predicate, $.comparison),

    predicate: $ => seq(
      optional(seq(field('negation', '!'), sameLine($))),
      field('name', $.qualified_name),
      sameLine($),
      optional(seq(field('snapshot', $.snapshot), sameLine($))),
      '(',
      choice(
        optionalCommaList($._term),
        seq(commaSeparated($.named_term), optional(',')),
      ),
      ')',
    ),

    named_term: $ => seq(field('name', $.identifier), ':', field('value', $._term)),

    comparison: $ => seq(
      field('left', $._term),
      sameLine($),
      field('operator', choice('==', '!=', '<', '<=', '>', '>=')),
      field('right', $._term),
    ),

    _term: $ => choice(
      $.term,
      $.string,
      $.integer,
      $.negative_integer,
      $.true,
      $.false,
      $.null,
    ),

    // Rule terms have no unary minus, so a negative constant is read here.
    negative_integer: $ => seq('-', $.integer),

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
      exported($),
      'require',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      braced($, choice($.when_clause, $.field)),
    ),

    allow_declaration: $ => seq(
      exported($),
      'allow',
      sameLine($),
      field('requirement', $.qualified_name),
      sameLine($),
      braced($, choice($.when_clause, $.field)),
    ),

    // In `require` and `allow`, the value of `when` is a rule body, not an
    // expression.
    when_clause: $ => seq('when', sameLine($), ':', field('body', $.rule_body)),

    behavior_declaration: $ => seq(
      exported($),
      'behavior',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      optional(seq(field('parameters', $.parameters), sameLine($))),
      field('body', $.block),
    ),

    function_declaration: $ => seq(
      exported($),
      'fn',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      field('parameters', $.parameters),
      sameLine($),
      ':',
      field('return_type', $._type),
      sameLine($),
      field('body', $.block),
    ),

    checker_declaration: $ => seq(
      exported($),
      'checker',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      field('parameters', $.parameters),
      sameLine($),
      ':',
      field('return_type', $._type),
      sameLine($),
      bracedFields($, $.field),
    ),

    check_declaration: $ => seq(
      exported($),
      'check',
      sameLine($),
      field('name', $.identifier),
      sameLine($),
      braced($, choice($.run, $.skip)),
    ),

    run: $ => seq(
      'run',
      sameLine($),
      field('requirement', choice($.qualified_name, $.wildcard)),
      optional(field('arguments', $.arguments)),
      optional(field('overrides', $.record)),
    ),

    skip: $ => seq('skip', sameLine($), field('requirement', $.qualified_name)),

    // `lemma.http.*`: every requirement in a module.
    wildcard: $ => seq(field('module', $.qualified_name), '.', sameLine($), '*'),

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
      sameLine($),
      field('condition', $._expression),
      optional(seq('because', sameLine($), field('reason', $._expression))),
    ),

    return_statement: $ => seq('return', sameLine($), field('value', $._expression)),

    // The block opens on the line of its condition, and the `else` or
    // `else if` that follows it on the same line as `else`.
    if_statement: $ => seq(
      'if',
      sameLine($),
      field('condition', $._expression),
      sameLine($),
      field('consequence', $.block),
      optional(seq(
        'else',
        sameLine($),
        field('alternative', choice($.block, $.if_statement)),
      )),
    ),

    for_statement: $ => seq(
      'for',
      sameLine($),
      field('pattern', choice($.identifier, $.tuple_pattern)),
      sameLine($),
      'in',
      sameLine($),
      field('domain', $._expression),
      sameLine($),
      field('body', $.block),
    ),

    tuple_pattern: $ => seq('(', optionalCommaList($.identifier), ')'),

    // Fields, parameters and types

    field: $ => seq(field('name', $.identifier), sameLine($), ':', field('value', $._expression)),

    parameters: $ => seq(
      '(',
      optionalCommaList($.parameter),
      ')',
    ),

    parameter: $ => seq(field('name', $.identifier), ':', field('type', $._type)),

    typed_field: $ => seq(field('name', $.identifier), sameLine($), ':', field('type', $._type)),

    _type: $ => choice($.named_type, $.record_type),

    named_type: $ => seq(
      field('name', $.identifier),
      optional(field('arguments', $.type_arguments)),
    ),

    type_arguments: $ => seq('<', optionalCommaList($._type), '>'),

    record_type: $ => bracedFields($, $.typed_field),

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
      optionalCommaList(field('index', $._expression)),
      ']',
    ),

    call_expression: $ => seq(
      field('function', $._postfix),
      field('arguments', $.arguments),
    ),

    arguments: $ => seq(
      '(',
      optionalCommaList(choice($._expression, $.named_argument)),
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

    list: $ => seq('[', optionalCommaList($._expression), ']'),

    record: $ => bracedFields($, $.field),

    // Lexical

    qualified_name: $ => seq($.identifier, repeat(seq('.', sameLine($), $.identifier))),

    snapshot: _ => choice('@base', '@head'),

    // Letters are ASCII. A backticked name may hold anything but a backtick or
    // a newline, such as a field called `in`.
    identifier: _ => /[A-Za-z_][A-Za-z0-9_]*|`[^`\n]+`/,

    // 64-bit signed integers. A minus sign is its own token: `-1` in an
    // expression is a unary minus, and in a rule term a `negative_integer`.
    integer: _ => /[0-9]+/,

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
    // plain text, so dispatch keys such as "GET /users/{}" parse cleanly. The
    // lone brace has no precedence: inside a string no other `{` can stand,
    // and while recovering from an error, when every token can, a `{` should
    // read as the brace that opens a body, so the parser can resume there.
    string_content: _ => choice(
      token.immediate(prec(1, /[^"\\{}\n]+/)),
      token.immediate(/[{}]/),
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
 * Marks a place where a newline would end the line too early: after a
 * declaration's keyword or name, before the `{` of a body or block, and
 * wherever else the Rust parser reads a newline as a separator that cannot
 * stand there, such as `fn f(): Int` with its `{` on the next line. The
 * scanner turns a newline at such a place into a separator, so Tree-sitter
 * reports the same error, and its recovery can end the half-written line
 * there rather than swallow the next one.
 *
 * Only places outside brackets are marked. Inside `( )` and `[ ]` a newline
 * is whitespace, so the rules that also occur there, such as unary
 * operators, member access and parameters, have no marks.
 *
 * @param {GrammarSymbols<string>} $
 * @returns {ChoiceRule}
 */
function sameLine($) {
  return optional($._same_line);
}

/**
 * The optional `export` that starts a declaration, on its line.
 *
 * @param {GrammarSymbols<string>} $
 * @returns {ChoiceRule}
 */
function exported($) {
  return optional(seq('export', sameLine($)));
}

/**
 * Zero or more `rule`, each ended by `separators`, except that the last may
 * stand without one: a closing `}` or the end of the file ends it.
 *
 * @param {RuleOrLiteral} separators
 * @param {RuleOrLiteral} rule
 * @returns {RuleOrLiteral[]}
 */
function lines(separators, rule) {
  return [repeat(seq(rule, separators)), optional(rule)];
}

/**
 * A `{ ... }` body of members separated by newlines or `;`. Separators may
 * also lead, so a body can open with a blank line or a `;`.
 *
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} member
 * @returns {SeqRule}
 */
function braced($, member) {
  return seq('{', optional($._separators), ...lines($._separators, member), '}');
}

/**
 * A `{ ... }` body of fields or typed fields, separated by newlines, `;` or
 * commas.
 *
 * @param {GrammarSymbols<string>} $
 * @param {RuleOrLiteral} member
 * @returns {SeqRule}
 */
function bracedFields($, member) {
  return seq(
    '{',
    optional($._field_separators),
    ...lines($._field_separators, member),
    '}',
  );
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
 * Zero or more `rule`, separated by commas, with an optional trailing comma.
 *
 * @param {RuleOrLiteral} rule
 * @returns {ChoiceRule}
 */
function optionalCommaList(rule) {
  return optional(seq(commaSeparated(rule), optional(',')));
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
