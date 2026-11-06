#!/usr/bin/env bash
# Checks the editor support for Lemma's `.lm` language.
#
# Usage: editors/check.sh [grammar] [vscode]
#
# With no arguments it runs every section:
#
#   grammar  regenerates the Tree-sitter parser from grammar.js; runs the
#            corpus and highlight tests; parses every ```lm block in
#            .specs/13-lm-grammar.md and every `.lm` file in the repository,
#            such as the standard library and the Rust parser's corpus,
#            failing on any ERROR or MISSING node; and runs every query
#            against those files, so a query naming a node type the grammar
#            no longer has fails.
#   vscode   type-checks, lints and format-checks the VS Code client and
#            checks its TextMate grammar, then packages the extension,
#            installs the .vsix into a real VS Code and runs its integration
#            test there (editors/vscode/test).
#
# Needs node, npm and a C compiler. Each package's pinned tools are installed
# on the first run. With CI set, a committed parser that is out of date with
# grammar.js fails the check.
#
# The editor sections run the language server built from this checkout:
# $LEMMA if set, or else target/release/lemma, which they build with cargo.
# A section whose editor is not installed, or that needs a display it cannot
# get, is skipped with a message saying why; set the editor's variable below
# to use one that is not on PATH.
#
#   VSCODE_EXECUTABLE  a VS Code to use instead of downloading one

set -euo pipefail

editors_directory=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repository=$(dirname "$editors_directory")
grammar_directory="$editors_directory/tree-sitter-lemma"
vscode_directory="$editors_directory/vscode"
spec="$repository/.specs/13-lm-grammar.md"

# A section's command exits with this status when it cannot run here.
skip_status=77
skipped=()

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

step() {
  printf '\n== %s\n' "$1"
}

skip() {
  echo "Skipped: $2"
  skipped+=("$1: $2")
}

install_packages() {
  if [ ! -d "$1/node_modules" ]; then
    (cd "$1" && npm ci --no-audit --no-fund)
  fi
}

install_tree_sitter() {
  install_packages "$grammar_directory"
  if [ ! -x "$grammar_directory/node_modules/tree-sitter-cli/tree-sitter" ]; then
    # npm can be configured to skip install scripts; this one downloads the CLI.
    (cd "$grammar_directory/node_modules/tree-sitter-cli" && node install.js)
  fi
  tree_sitter="$grammar_directory/node_modules/.bin/tree-sitter"
  # The CLI warns on every command when no parser directory is configured.
  mkdir -p "$scratch/parsers"
  printf '{"parser-directories": ["%s"]}\n' "$scratch/parsers" >"$scratch/config.json"
}

# Sets `lemma` to the language server the editor sections run, building it
# on first use. Returns 1, after saying why, when there is none.
lemma=
find_lemma() {
  if [ -n "$lemma" ]; then
    return 0
  fi
  if [ -n "${LEMMA:-}" ]; then
    lemma=$LEMMA
    return 0
  fi
  if ! command -v cargo >/dev/null; then
    echo "No lemma binary: set LEMMA, or install cargo to build one."
    return 1
  fi
  step "Building the language server"
  cargo build --release --quiet --manifest-path "$repository/Cargo.toml" -p lemma-cli
  lemma="${CARGO_TARGET_DIR:-$repository/target}/release/lemma"
}

# Runs a section's editor command, recording status 77 as a skip.
run_editor() {
  local section=$1
  shift
  local status=0
  "$@" || status=$?
  if [ "$status" -eq "$skip_status" ]; then
    skipped+=("$section: see the message above")
    return 0
  fi
  return "$status"
}

check_grammar() {
  install_tree_sitter
  cd "$grammar_directory"

  step "Generating the parser"
  local generated=(src/parser.c src/grammar.json src/node-types.json src/tree_sitter/*.h)
  local before after
  before=$(cat "${generated[@]}" | cksum)
  "$tree_sitter" generate
  after=$(cat "${generated[@]}" | cksum)
  if [ "$before" != "$after" ]; then
    echo "src/ changed when regenerated from grammar.js; commit the regenerated files."
    if [ -n "${CI:-}" ]; then
      exit 1
    fi
  fi

  step "Running the corpus and highlight tests"
  "$tree_sitter" test --config-path "$scratch/config.json"

  step "Parsing the examples in spec 13"
  mkdir "$scratch/spec"
  awk -v directory="$scratch/spec" '
    /^```lm[[:space:]]*$/ { inside = 1; count++; file = sprintf("%s/block-%02d.lm", directory, count); next }
    /^```/ && inside { inside = 0; close(file); next }
    inside { print > file }
  ' "$spec"
  local blocks=("$scratch"/spec/*.lm)
  if [ ! -e "${blocks[0]}" ]; then
    echo "No \`\`\`lm blocks found in $spec" >&2
    exit 1
  fi
  # --quiet prints a line only for a file with an ERROR or MISSING node, and
  # the exit status says whether there was one.
  "$tree_sitter" parse --quiet --config-path "$scratch/config.json" "${blocks[@]}"
  echo "${#blocks[@]} blocks parse without errors"

  step "Parsing every .lm file in the repository"
  local sources=() source
  while IFS= read -r -d '' source; do
    sources+=("$repository/$source")
  done < <(git -C "$repository" ls-files -z --cached --others --exclude-standard -- '*.lm')
  if [ "${#sources[@]}" -eq 0 ]; then
    echo "No .lm files found in $repository" >&2
    exit 1
  fi
  "$tree_sitter" parse --quiet --config-path "$scratch/config.json" "${sources[@]}"
  echo "${#sources[@]} files parse without errors"

  step "Running the queries"
  local query
  for query in queries/*.scm; do
    "$tree_sitter" query --config-path "$scratch/config.json" "$query" "${blocks[@]}" "${sources[@]}" >/dev/null
    echo "$query"
  done
}

check_vscode() {
  install_packages "$vscode_directory"

  step "Checking the VS Code extension"
  (cd "$vscode_directory" && npm run --silent check)

  if ! find_lemma; then
    skip vscode "no language server to run"
    return
  fi
  step "Running the VS Code extension in VS Code"
  run_editor vscode env LEMMA="$lemma" npm --prefix "$vscode_directory" run --silent test:integration
}

sections=("$@")
if [ "${#sections[@]}" -eq 0 ]; then
  sections=(grammar vscode)
fi
for section in "${sections[@]}"; do
  case "$section" in
    grammar) check_grammar ;;
    vscode) check_vscode ;;
    *)
      echo "Unknown section \`$section\`; expected grammar or vscode" >&2
      exit 2
      ;;
  esac
done

if [ "${#skipped[@]}" -gt 0 ]; then
  printf '\nSkipped:\n'
  printf '  %s\n' "${skipped[@]}"
fi
printf '\nAll editor checks passed.\n'
