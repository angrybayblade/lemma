#!/usr/bin/env bash
# Checks the editor support for Lemma's `.lm` language.
#
# Usage: editors/check.sh [grammar] [vscode] [neovim] [helix] [zed]
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
#   neovim   builds the parser, then runs editors/neovim/check.lua in a
#            headless Neovim 0.11 or later with the setup from README.md:
#            highlight captures at a few positions, and the language server
#            attaching and publishing diagnostics.
#   helix    runs editors/helix/check.sh, which follows the README's Helix
#            setup in a throwaway home, checks `hx --health lemma` and the
#            translated captures, and with tmux checks what Helix draws.
#   zed      checks that the Zed extension's generated highlights.scm is up
#            to date, runs its queries against the same files as the grammar
#            section, and builds and lints its Rust crate for each WebAssembly
#            target installed (wasm32-wasip2, which Zed uses, and
#            wasm32-wasip1). No Zed runs.
#
# Needs node, npm and a C compiler. Each package's pinned tools are installed
# on the first run. With CI set, a committed parser that is out of date with
# grammar.js fails the check.
#
# The editor sections run the language server built from this checkout:
# $LM if set, or else target/release/lm, which they build with cargo.
# A section whose editor is not installed, or that needs a display it cannot
# get, is skipped with a message saying why; set the editor's variable below
# to use one that is not on PATH.
#
#   VSCODE_EXECUTABLE  a VS Code to use instead of downloading one
#   NVIM_EXECUTABLE    a Neovim to use instead of `nvim`
#   HELIX_EXECUTABLE   a Helix to use instead of `hx`

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

# Sets `lm` to the binary whose `lm lsp` the editor sections run, building
# it on first use. Returns 1, after saying why, when there is none.
lm=
find_lm() {
  if [ -n "$lm" ]; then
    return 0
  fi
  if [ -n "${LM:-}" ]; then
    lm=$LM
    return 0
  fi
  if ! command -v cargo >/dev/null; then
    echo "No lm binary: set LM, or install cargo to build one."
    return 1
  fi
  echo "Building the language server: cargo build --release -p lemma-cli --bin lm"
  cargo build --release --quiet --manifest-path "$repository/Cargo.toml" -p lemma-cli --bin lm
  lm="${CARGO_TARGET_DIR:-$repository/target}/release/lm"
}

# A directory holding only `lm`, linked to the server, for editors whose
# documented setup runs `lm` from PATH.
lm_bin_directory() {
  local directory="$scratch/bin"
  if [ ! -e "$directory/lm" ]; then
    mkdir -p "$directory"
    ln -s "$(realpath "$lm")" "$directory/lm"
  fi
  echo "$directory"
}

# Whether version $1 is at least version $2, comparing dotted numbers.
version_at_least() {
  [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n 1)" = "$2" ]
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

# Sets `spec_blocks` to spec 13's ```lm blocks, written out as files, and
# `repository_sources` to every `.lm` file in the repository, tracked or new.
spec_blocks=()
repository_sources=()
collect_lm_files() {
  if [ "${#spec_blocks[@]}" -gt 0 ]; then
    return 0
  fi
  mkdir -p "$scratch/spec"
  awk -v directory="$scratch/spec" '
    /^```lm[[:space:]]*$/ { inside = 1; count++; file = sprintf("%s/block-%02d.lm", directory, count); next }
    /^```/ && inside { inside = 0; close(file); next }
    inside { print > file }
  ' "$spec"
  spec_blocks=("$scratch"/spec/*.lm)
  if [ ! -e "${spec_blocks[0]}" ]; then
    echo "No \`\`\`lm blocks found in $spec" >&2
    exit 1
  fi
  local source
  while IFS= read -r -d '' source; do
    repository_sources+=("$repository/$source")
  done < <(git -C "$repository" ls-files -z --cached --others --exclude-standard -- '*.lm')
  if [ "${#repository_sources[@]}" -eq 0 ]; then
    echo "No .lm files found in $repository" >&2
    exit 1
  fi
}

check_grammar() {
  install_tree_sitter
  collect_lm_files
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
  # --quiet prints a line only for a file with an ERROR or MISSING node, and
  # the exit status says whether there was one.
  "$tree_sitter" parse --quiet --config-path "$scratch/config.json" "${spec_blocks[@]}"
  echo "${#spec_blocks[@]} blocks parse without errors"

  step "Parsing every .lm file in the repository"
  "$tree_sitter" parse --quiet --config-path "$scratch/config.json" "${repository_sources[@]}"
  echo "${#repository_sources[@]} files parse without errors"

  step "Running the queries"
  run_queries queries/*.scm
}

# Runs each query against spec 13's blocks and the repository's `.lm` files,
# failing on a query the grammar cannot compile. Runs in the grammar's
# directory, where the CLI finds the grammar.
run_queries() {
  local query
  for query in "$@"; do
    "$tree_sitter" query --config-path "$scratch/config.json" "$query" \
      "${spec_blocks[@]}" "${repository_sources[@]}" >/dev/null
    echo "${query#"$editors_directory"/}"
  done
}

check_vscode() {
  install_packages "$vscode_directory"

  step "Checking the VS Code extension"
  (cd "$vscode_directory" && npm run --silent check)

  if ! find_lm; then
    skip vscode "no language server to run"
    return
  fi
  step "Running the VS Code extension in VS Code"
  run_editor vscode env LM="$lm" npm --prefix "$vscode_directory" run --silent test:integration
}

check_neovim() {
  step "Running the Neovim check"
  local nvim=${NVIM_EXECUTABLE:-nvim}
  if ! command -v "$nvim" >/dev/null; then
    skip neovim "Neovim is not installed; set NVIM_EXECUTABLE to use one not on PATH"
    return
  fi
  local version
  version=$("$nvim" --version | sed -n '1s/^NVIM v\([0-9][0-9.]*\).*/\1/p')
  if ! version_at_least "$version" 0.11; then
    skip neovim "Neovim $version is too old; the ABI 15 parser needs 0.11 or later"
    return
  fi
  if ! find_lm; then
    skip neovim "no language server to run"
    return
  fi
  install_tree_sitter

  # A runtime directory laid out as Neovim looks for parsers and queries, and
  # a home of its own so the user's configuration and state stay out of it.
  local runtime="$scratch/neovim/runtime" home="$scratch/neovim/home"
  mkdir -p "$runtime/parser" "$runtime/queries/lemma" "$home"
  # `build` has no --config-path; TREE_SITTER_DIR is where it looks instead.
  TREE_SITTER_DIR="$scratch" "$tree_sitter" build --output "$runtime/parser/lemma.so" "$grammar_directory"
  cp "$grammar_directory"/queries/*.scm "$runtime/queries/lemma/"
  PATH="$(lm_bin_directory):$PATH" \
    XDG_CONFIG_HOME="$home/config" XDG_DATA_HOME="$home/data" \
    XDG_STATE_HOME="$home/state" XDG_CACHE_HOME="$home/cache" \
    "$nvim" --clean --headless -l "$editors_directory/neovim/check.lua" \
    "$runtime" "$editors_directory/fixture/mod.lm" "$editors_directory/README.md"
}

check_helix() {
  step "Running the Helix check"
  if ! command -v "${HELIX_EXECUTABLE:-hx}" >/dev/null; then
    skip helix "Helix is not installed; set HELIX_EXECUTABLE to use one not on PATH"
    return
  fi
  if ! find_lm; then
    skip helix "no language server to run"
    return
  fi
  run_editor helix "$editors_directory/helix/check.sh" "$lm"
}

check_zed() {
  local zed_directory="$editors_directory/zed"
  install_tree_sitter
  collect_lm_files

  step "Checking the Zed extension's queries"
  "$zed_directory/update-highlights.sh" "$scratch/zed-highlights.scm"
  if ! cmp -s "$scratch/zed-highlights.scm" "$zed_directory/languages/lemma/highlights.scm"; then
    echo "editors/zed/languages/lemma/highlights.scm is out of date; run editors/zed/update-highlights.sh" >&2
    exit 1
  fi
  (cd "$grammar_directory" && run_queries "$zed_directory"/languages/lemma/*.scm)
  # The extension can only name a commit that already holds the grammar, so a
  # grammar change is followed by a change to this `rev`.
  local revision
  revision=$(sed -n 's/^rev = "\(.*\)"$/\1/p' "$zed_directory/extension.toml")
  if ! git -C "$repository" diff --quiet "$revision" -- editors/tree-sitter-lemma/src 2>/dev/null; then
    echo "Note: editors/zed/extension.toml pins the grammar at $revision, which differs from this checkout's; update its rev once the grammar change is committed."
  fi

  step "Building the Zed extension"
  if ! command -v cargo >/dev/null || ! command -v rustup >/dev/null; then
    skip zed "building the extension needs cargo and rustup"
    return
  fi
  local installed targets=() target
  installed=$(rustup target list --installed)
  for target in wasm32-wasip2 wasm32-wasip1; do
    if grep -qx "$target" <<<"$installed"; then
      targets+=("$target")
    fi
  done
  if [ "${#targets[@]}" -eq 0 ]; then
    skip zed "no WebAssembly target to build for; run rustup target add wasm32-wasip2"
    return
  fi
  local manifest="$zed_directory/Cargo.toml"
  cargo fmt --manifest-path "$manifest" --check
  for target in "${targets[@]}"; do
    cargo clippy --release --quiet --manifest-path "$manifest" --target "$target" -- -D warnings
    cargo build --release --quiet --manifest-path "$manifest" --target "$target"
    echo "The extension builds for $target"
  done
}

sections=("$@")
if [ "${#sections[@]}" -eq 0 ]; then
  sections=(grammar vscode neovim helix zed)
fi
for section in "${sections[@]}"; do
  case "$section" in
    grammar) check_grammar ;;
    vscode) check_vscode ;;
    neovim) check_neovim ;;
    helix) check_helix ;;
    zed) check_zed ;;
    *)
      echo "Unknown section \`$section\`; expected grammar, vscode, neovim, helix or zed" >&2
      exit 2
      ;;
  esac
done

if [ "${#skipped[@]}" -gt 0 ]; then
  printf '\nSkipped:\n'
  printf '  %s\n' "${skipped[@]}"
fi
printf '\nAll editor checks passed.\n'
