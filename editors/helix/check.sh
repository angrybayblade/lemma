#!/usr/bin/env bash
# Follows the Helix section of editors/README.md in a throwaway home and
# checks what Helix makes of it:
#
#   1. writes the section's languages.toml, with this checkout as the
#      grammar's path, and runs its shell blocks as written;
#   2. asserts that `hx --health lemma` finds the language server, the
#      parser and the highlight queries;
#   3. asserts that every capture in the translated highlights.scm is a
#      scope Helix documents for themes;
#   4. builds the grammar again through the section's [[grammar]] entry and
#      `hx --grammar build`;
#   5. with tmux installed, opens editors/fixture/lemma.lm in Helix inside a
#      detached tmux session, with a theme giving each checked scope its own
#      colour, and asserts that a keyword, declared names, a rule variable
#      and a string have their colours and that the server's diagnostic
#      marks the gutter.
#
# Usage: editors/helix/check.sh <lemma binary>
#
# HELIX_EXECUTABLE names the hx to check; otherwise it is `hx` on PATH.
# Exits with 77 when there is no Helix. editors/check.sh runs it.

set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: editors/helix/check.sh <lemma binary>" >&2
  exit 2
fi

helix_directory=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
editors_directory=$(dirname "$helix_directory")
repository=$(dirname "$editors_directory")
readme="$editors_directory/README.md"
fixture="$editors_directory/fixture/lemma.lm"
lemma=$(realpath "$1")

hx=${HELIX_EXECUTABLE:-hx}
if ! command -v "$hx" >/dev/null; then
  echo "Skipping the Helix check: Helix is not installed; set HELIX_EXECUTABLE to use one not on PATH"
  exit 77
fi
hx=$(command -v "$hx")

# Scopes for syntax highlighting, from "Scopes" in Helix 25.07's theme
# documentation (book/src/themes.md). Themes match a capture by its longest
# prefix among these, so a capture outside the list gets no colour of its own.
documented_scopes="
attribute
type type.builtin type.parameter type.enum type.enum.variant
constructor
constant constant.builtin constant.builtin.boolean constant.character
constant.character.escape constant.numeric constant.numeric.integer
constant.numeric.float
string string.regexp string.special string.special.path string.special.url
string.special.symbol
comment comment.line comment.line.documentation comment.block
comment.block.documentation comment.unused
variable variable.builtin variable.parameter variable.other
variable.other.member variable.other.member.private
label
punctuation punctuation.delimiter punctuation.bracket punctuation.special
keyword keyword.control keyword.control.conditional keyword.control.repeat
keyword.control.import keyword.control.return keyword.control.exception
keyword.operator keyword.directive keyword.function keyword.storage
keyword.storage.type keyword.storage.modifier
operator
function function.builtin function.method function.method.private
function.macro function.special
tag tag.builtin
namespace
special
"

failures=()
fail() {
  failures+=("$1")
}

scratch=$(mktemp -d)
cleanup() {
  TMUX_TMPDIR="$scratch" tmux -L lemma-check kill-server 2>/dev/null || true
  rm -rf "$scratch"
}
trap cleanup EXIT

# `hx` and `lemma` on PATH, as the README's setup expects them.
bin="$scratch/bin"
mkdir -p "$bin"
ln -s "$hx" "$bin/hx"
ln -s "$lemma" "$bin/lemma"

# Runs a command with a throwaway home, so ~/.config/helix is the check's.
in_home() {
  local home=$1
  shift
  env -u XDG_CONFIG_HOME -u XDG_DATA_HOME -u XDG_CACHE_HOME -u XDG_STATE_HOME \
    -u HELIX_RUNTIME -u TMUX HOME="$home" PATH="$bin:$PATH" COLORTERM=truecolor "$@"
}

# The `n`th fenced block of language $1 in the README's Helix section, or
# every such block when `n` is 0, with the placeholder path replaced by this
# checkout.
readme_block() {
  awk -v language="$1" -v wanted="$2" '
    /^## / { inside = ($0 == "## Helix") }
    !inside { next }
    block && /^```/ { block = 0; next }
    block { if (wanted == 0 || count == wanted) print; next }
    $0 == "```" language { block = 1; count++ }
  ' "$readme" | sed "s#/path/to/lemma#$repository#g"
}

strip_colours() {
  sed 's/\x1b\[[0-9;:]*m//g'
}

# 1. The README's setup, as written.
home="$scratch/home"
mkdir -p "$home/.config/helix"
readme_block toml 1 >"$home/.config/helix/languages.toml"
if ! grep -q '^\[\[language\]\]' "$home/.config/helix/languages.toml"; then
  echo "The README's Helix section has no languages.toml block" >&2
  exit 1
fi
readme_block sh 0 >"$scratch/readme-steps.sh"
echo "Running the README's Helix steps in a throwaway home"
(cd "$scratch" && in_home "$home" bash -euo pipefail "$scratch/readme-steps.sh")

# 2. What Helix's health check finds.
health=$(in_home "$home" hx --health lemma 2>&1 | strip_colours)
printf '%s\n' "$health"
grep -q '✓ lemma: ' <<<"$health" || fail "hx --health lemma does not find the language server"
grep -q '^Tree-sitter parser: ✓' <<<"$health" || fail "hx --health lemma does not find the parser"
grep -q '^Highlight queries: ✓' <<<"$health" || fail "hx --health lemma does not find the highlight queries"

# 3. Every translated capture is a documented scope.
translated="$home/.config/helix/runtime/queries/lemma/highlights.scm"
unknown=$(grep -v '^[[:space:]]*;' "$translated" | grep -oE '@[A-Za-z][A-Za-z0-9_.-]*' | sed 's/^@//' | sort -u |
  comm -23 - <(tr ' ' '\n' <<<"$documented_scopes" | sed '/^$/d' | sort -u) || true)
if [ -n "$unknown" ]; then
  fail "translated captures Helix does not document: $(tr '\n' ' ' <<<"$unknown")"
fi

# 4. The [[grammar]] alternative builds the same grammar.
grammar_home="$scratch/grammar-home"
mkdir -p "$grammar_home/.config/helix/runtime/grammars"
{
  echo 'use-grammars = { only = ["lemma"] }'
  readme_block toml 1
  readme_block toml 2
} >"$grammar_home/.config/helix/languages.toml"
if ! grep -q '^\[\[grammar\]\]' "$grammar_home/.config/helix/languages.toml"; then
  fail "the README's Helix section has no [[grammar]] block"
elif ! in_home "$grammar_home" hx --grammar build; then
  fail "hx --grammar build failed with the README's [[grammar]] entry"
elif [ ! -e "$grammar_home/.config/helix/runtime/grammars/lemma.so" ]; then
  fail "hx --grammar build built no lemma.so"
fi

# 5. Highlights and diagnostics as Helix draws them.
render_fixture() {
  mkdir -p "$home/.config/helix/themes"
  cat >"$home/.config/helix/themes/lemma-check.toml" <<'THEME'
inherits = "default"
"keyword.storage.type" = "#ff0001"
"type" = "#ff0002"
"function" = "#ff0003"
"variable.parameter" = "#ff0004"
"string" = "#ff0005"
"error" = "#ff0006"
THEME
  echo 'theme = "lemma-check"' >"$home/.config/helix/config.toml"

  local tmux=(in_home "$home" env TMUX_TMPDIR="$scratch" tmux -L lemma-check)
  "${tmux[@]}" -f /dev/null new-session -d -x 120 -y 40 "hx $fixture"
  # The diagnostic is the last thing to appear: it needs the server.
  local screen="" attempt
  for attempt in $(seq 1 100); do
    screen=$("${tmux[@]}" capture-pane -p -e -t 0)
    if grep -q '●' <<<"$screen"; then
      break
    fi
    sleep 0.2
  done
  "${tmux[@]}" kill-server

  local cases=(
    "255;0;1 relation a keyword"
    "255;0;2 Owns the declared relation"
    "255;0;3 owned the declared rule"
    "255;0;4 other a rule variable"
    "255;0;5 \" a string"
    "255;0;6 ● the server's diagnostic in the gutter"
  )
  local case colour token what
  for case in "${cases[@]}"; do
    read -r colour token what <<<"$case"
    if ! grep -qF "$(printf '\e[38;2;%sm%s' "$colour" "$token")" <<<"$screen"; then
      fail "Helix does not draw $what (\`$token\`) in its colour"
    fi
  done
  if [ "${#failures[@]}" -gt 0 ]; then
    echo "What Helix drew:"
    strip_colours <<<"$screen"
  fi
}

if command -v tmux >/dev/null; then
  render_fixture
else
  echo "tmux is not installed; not checking what Helix draws"
fi

if [ "${#failures[@]}" -gt 0 ]; then
  printf 'Helix check failed:\n'
  printf '  %s\n' "${failures[@]}"
  exit 1
fi
echo "$("$hx" --version): the README's setup finds the grammar, queries and server, and highlights work"
