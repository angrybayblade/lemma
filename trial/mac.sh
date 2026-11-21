#!/bin/sh
# The coverage half of the trial, for a Mac with System Integrity Protection on
# (the CI runners have it off, which hides what protected programs strip).
# Needs no root. Runs a few real test suites under the injected library and
# says, for each, whether every process was watched.
# Usage: sh trial/mac.sh [WORK_DIR]   (needs git, clang; go, python3, node+npm used if present)
set -u
here=$(cd "$(dirname "$0")" && pwd)
work=${1:-/tmp/lemma-mac-trial}
mkdir -p "$work"
sh "$here/build.sh" > "$work/build.log" 2>&1 || { echo "build failed: see $work/build.log"; exit 1; }
csrutil status
run() {  # LABEL REPO_URL REF COMMAND...
  label=$1 url=$2 ref=$3; shift 3
  dir="$work/$label"
  [ -d "$dir/src" ] || git clone -q --depth 50 "$url" "$dir/src"
  git -C "$dir/src" checkout -q "$ref" 2>/dev/null
  rm -rf "$dir/calls"
  ( cd "$dir/src" && sh "$here/with-c.sh" "$dir/calls" "$@" > "$dir/output.txt" 2>&1 )
  python3 "$here/coverage.py" "$dir/calls" "$label"
}
# The probe first: its env-script child is the known gap.
rm -rf "$work/probe"; mkdir -p "$work/probe"
sh "$here/with-c.sh" "$work/probe/calls" "$here/probe" "$work/probe/dir" > "$work/probe/output.txt" 2>&1
python3 "$here/coverage.py" "$work/probe/calls" probe
command -v go > /dev/null && run gin https://github.com/gin-gonic/gin master go test ./...
command -v python3 > /dev/null && run flask https://github.com/pallets/flask main sh -c 'python3 -m venv .venv && .venv/bin/pip -q install -e . pytest && .venv/bin/python -m pytest -q tests/test_basic.py'
command -v npm > /dev/null && run zustand https://github.com/pmndrs/zustand main sh -c 'npm install --silent --no-audit --no-fund && npx vitest run'
command -v npm > /dev/null && run zustand-direct https://github.com/pmndrs/zustand main node node_modules/vitest/vitest.mjs run
echo "Done. Paste the lines above back; details are in $work."
