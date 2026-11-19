#!/bin/sh
# Runs a command with the injected library (candidate C). The variables are set by the shell for the
# program it starts, not through /usr/bin/env, which is protected and removes DYLD_* variables.
# Usage: with-c.sh CALLS_DIR COMMAND...
here=$(cd "$(dirname "$0")" && pwd)
calls=$1; shift
mkdir -p "$calls"
DYLD_INSERT_LIBRARIES="$here/watch.dylib" WATCH_DIR="$calls" exec "$@"
