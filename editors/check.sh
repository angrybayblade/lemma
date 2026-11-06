#!/usr/bin/env bash
# Checks the editor support for Lemma's `.lm` language:
#
#   1. regenerates the Tree-sitter parser from grammar.js;
#   2. runs the Tree-sitter corpus and highlight tests;
#   3. parses every ```lm block in .specs/13-lm-grammar.md and every `.lm`
#      file in the repository, such as the standard library and the Rust
#      parser's corpus, and fails on any ERROR or MISSING node;
#   4. runs every query against those files, so a query naming a node type
#      the grammar no longer has fails;
#   5. type-checks, lints and format-checks the VS Code client, and checks its
#      TextMate grammar.
#
# Needs node, npm and a C compiler. Each package's pinned tools are installed
# on the first run. With CI set, a committed parser that is out of date with
# grammar.js fails the check.

set -euo pipefail

editors_directory=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repository=$(dirname "$editors_directory")
grammar_directory="$editors_directory/tree-sitter-lemma"
vscode_directory="$editors_directory/vscode"
spec="$repository/.specs/13-lm-grammar.md"

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

step() {
  printf '\n== %s\n' "$1"
}

install_packages() {
  if [ ! -d "$1/node_modules" ]; then
    (cd "$1" && npm ci --no-audit --no-fund)
  fi
}

step "Installing pinned tools"
install_packages "$grammar_directory"
install_packages "$vscode_directory"
if [ ! -x "$grammar_directory/node_modules/tree-sitter-cli/tree-sitter" ]; then
  # npm can be configured to skip install scripts; this one downloads the CLI.
  (cd "$grammar_directory/node_modules/tree-sitter-cli" && node install.js)
fi
tree_sitter="$grammar_directory/node_modules/.bin/tree-sitter"

# The CLI warns on every command when no parser directory is configured.
mkdir "$scratch/parsers"
printf '{"parser-directories": ["%s"]}\n' "$scratch/parsers" >"$scratch/config.json"

cd "$grammar_directory"

step "Generating the parser"
generated=(src/parser.c src/grammar.json src/node-types.json src/tree_sitter/*.h)
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
blocks=("$scratch"/spec/*.lm)
if [ ! -e "${blocks[0]}" ]; then
  echo "No \`\`\`lm blocks found in $spec" >&2
  exit 1
fi
# --quiet prints a line only for a file with an ERROR or MISSING node, and
# the exit status says whether there was one.
"$tree_sitter" parse --quiet --config-path "$scratch/config.json" "${blocks[@]}"
echo "${#blocks[@]} blocks parse without errors"

step "Parsing every .lm file in the repository"
sources=()
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
for query in queries/*.scm; do
  "$tree_sitter" query --config-path "$scratch/config.json" "$query" "${blocks[@]}" "${sources[@]}" >/dev/null
  echo "$query"
done

step "Checking the VS Code extension"
(cd "$vscode_directory" && npm run --silent check)

printf '\nAll editor checks passed.\n'
