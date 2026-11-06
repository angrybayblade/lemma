/*
 * External scanner for Lemma's `.lm` grammar.
 *
 * It produces two tokens the generated lexer cannot:
 *
 * NEWLINE: `.specs/13-lm-grammar.md` section 1 makes a newline end an item,
 * statement, record field or graph row, except inside `( )` and `[ ]`, after
 * a binary operator, `,`, `:`, `=`, `->`, `:-` or an opening bracket, and
 * before a line starting with `because`. In every one of those exceptions the
 * grammar cannot end an item, so the parser does not ask for a NEWLINE there.
 * The scanner therefore emits one whenever the parser accepts it and the next
 * line does not start with `because`; otherwise the newline is skipped as
 * whitespace.
 *
 * That is slightly more lenient than the spec's rule, which is stated per
 * token: `fn f(): Int` with its `{` on the next line breaks the line where no
 * separator can stand, so a parser applying that rule literally rejects it,
 * while here the newline is whitespace. Wherever the grammar can end an item,
 * the two agree.
 *
 * INTERPOLATION_START: the `{` of `{name}` or `{name.field}` inside a string.
 * Telling it apart from a plain `{` needs a look past the closing `}`, so a
 * string such as "GET /users/{}" or "{not interpolated" stays plain text.
 *
 * The scanner keeps no state between tokens.
 */

#include "tree_sitter/parser.h"

#include <stdbool.h>
#include <stdint.h>

enum TokenType {
  NEWLINE,
  INTERPOLATION_START,
  ERROR_SENTINEL,
};

void *tree_sitter_lemma_external_scanner_create(void) { return NULL; }

void tree_sitter_lemma_external_scanner_destroy(void *payload) { (void)payload; }

unsigned tree_sitter_lemma_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_lemma_external_scanner_deserialize(void *payload, const char *buffer,
                                                    unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

static bool is_name_start(int32_t c) {
  return c == '_' || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
}

static bool is_name_char(int32_t c) { return is_name_start(c) || (c >= '0' && c <= '9'); }

static bool is_horizontal_space(int32_t c) {
  return c == ' ' || c == '\t' || c == '\r' || c == '\f' || c == '\v';
}

static bool scan_name(TSLexer *lexer) {
  if (!is_name_start(lexer->lookahead)) return false;
  while (is_name_char(lexer->lookahead)) advance(lexer);
  return true;
}

/* The token is the `{` alone; the rest is only looked at. */
static bool scan_interpolation_start(TSLexer *lexer) {
  advance(lexer);
  lexer->mark_end(lexer);
  if (!scan_name(lexer)) return false;
  if (lexer->lookahead == '.') {
    advance(lexer);
    if (!scan_name(lexer)) return false;
  }
  if (lexer->lookahead != '}') return false;
  lexer->result_symbol = INTERPOLATION_START;
  return true;
}

/*
 * Moves past whitespace, newlines and comments to the start of the next
 * token. A comment line between an `expect` and its `because` line must not
 * end the statement.
 */
static void skip_to_next_token(TSLexer *lexer) {
  for (;;) {
    if (lexer->eof(lexer)) return;
    int32_t c = lexer->lookahead;
    if (is_horizontal_space(c) || c == '\n') {
      advance(lexer);
      continue;
    }
    if (c != '/') return;
    advance(lexer);
    if (lexer->lookahead == '/') {
      while (!lexer->eof(lexer) && lexer->lookahead != '\n') advance(lexer);
      continue;
    }
    if (lexer->lookahead != '*') return;
    advance(lexer);
    for (;;) {
      if (lexer->eof(lexer)) return;
      if (lexer->lookahead == '*') {
        advance(lexer);
        if (lexer->lookahead == '/') {
          advance(lexer);
          break;
        }
        continue;
      }
      advance(lexer);
    }
  }
}

static bool at_because(TSLexer *lexer) {
  const char *word = "because";
  for (const char *letter = word; *letter; letter++) {
    if (lexer->lookahead != *letter) return false;
    advance(lexer);
  }
  return !is_name_char(lexer->lookahead);
}

static bool scan_newline(TSLexer *lexer) {
  while (is_horizontal_space(lexer->lookahead)) skip(lexer);
  if (lexer->lookahead != '\n') return false;
  advance(lexer);
  lexer->mark_end(lexer);
  skip_to_next_token(lexer);
  if (at_because(lexer)) return false;
  lexer->result_symbol = NEWLINE;
  return true;
}

bool tree_sitter_lemma_external_scanner_scan(void *payload, TSLexer *lexer,
                                             const bool *valid_symbols) {
  (void)payload;
  /*
   * During error recovery every token is valid. Newlines still help the
   * parser find the next item, but an interpolation outside a string would
   * swallow the `{` of a record.
   */
  bool recovering = valid_symbols[ERROR_SENTINEL];

  if (valid_symbols[INTERPOLATION_START] && !recovering && lexer->lookahead == '{') {
    return scan_interpolation_start(lexer);
  }
  if (valid_symbols[NEWLINE]) {
    return scan_newline(lexer);
  }
  return false;
}
