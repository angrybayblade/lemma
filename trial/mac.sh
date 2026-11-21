#!/bin/sh
# The coverage half of the trial, for a Mac with System Integrity Protection on
# (the CI runners have it off, which hides what protected programs strip).
# Needs no root. For a few public test suites: install their dependencies
# unwatched, then run the tests under the injected library as a Lemma checker
# would run them (the command itself, no shell around it), and say whether
# every process was watched.
# Usage: sh trial/mac.sh [WORK_DIR]   (needs git, clang, python3; go and node+npm used if present)
set -u
here=$(cd "$(dirname "$0")" && pwd)
work=${1:-/tmp/lemma-mac-trial}
mkdir -p "$work"
sh "$here/build.sh" > "$work/build.log" 2>&1 || { echo "build failed: see $work/build.log"; exit 1; }
csrutil status
clone() {  # DIR URL REF
  [ -d "$1" ] || git clone -q --depth 50 "$2" "$1"
  git -C "$1" checkout -q "$3" 2>/dev/null
}
watch() {  # LABEL SRC COMMAND...
  label=$1 src=$2; shift 2
  rm -rf "$work/$label.calls"
  ( cd "$src" && sh "$here/with-c.sh" "$work/$label.calls" "$@" > "$work/$label.output.txt" 2>&1 )
  status=$?
  echo "$label: exit $status, $(tail -1 "$work/$label.output.txt" | cut -c1-100)"
  python3 "$here/coverage.py" "$work/$label.calls" "$label" "$status"
}
rm -rf "$work/probe.dir"
watch probe "$work" "$here/probe" "$work/probe.dir"
if command -v go > /dev/null; then
  clone "$work/gin" https://github.com/gin-gonic/gin master
  ( cd "$work/gin" && go mod download && go vet ./... > /dev/null 2>&1 )
  watch gin "$work/gin" go test ./...
fi
clone "$work/flask" https://github.com/pallets/flask main
( cd "$work/flask" && python3 -m venv .venv && .venv/bin/pip -q install -e . pytest > /dev/null 2>&1 )
watch flask "$work/flask" "$work/flask/.venv/bin/python" -m pytest -q tests/test_basic.py
if command -v npm > /dev/null; then
  clone "$work/zustand" https://github.com/pmndrs/zustand main
  ( cd "$work/zustand" && npm install --silent --no-audit --no-fund > /dev/null 2>&1 )
  watch zustand-npx "$work/zustand" npx vitest run
  watch zustand-node "$work/zustand" node node_modules/vitest/vitest.mjs run
fi
echo "Done. Paste the lines above back; details are in $work."
