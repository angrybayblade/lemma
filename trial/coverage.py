"""Which processes of a run the injected library (candidate C) watched.

Reads the calls.<pid> files `with-c.sh` leaves. A process is watched when
its file holds a `watched` line for its current program. A child started
with posix_spawn that has no file, or a successful execve with no `watched`
line after it, ran unwatched: its file reads are unseen, so a recorder built
on C would have to refuse the run.

Usage: python3 coverage.py CALLS_DIR [LABEL] [EXIT_STATUS]
"""
import collections
import os
import re
import sys

calls, label = sys.argv[1], (sys.argv[2] if len(sys.argv) > 2 else "")
status = int(sys.argv[3]) if len(sys.argv) > 3 else 0
if status >= 128:
    # Killed by a signal: on the runners, arm64e system binaries were killed
    # once the library loaded into them. The run itself was changed.
    print(f"{label}: killed by signal {status - 128} under the library -> C broke this run")
    sys.exit(0)
files = {}
for name in os.listdir(calls):
    if name.startswith("calls."):
        with open(os.path.join(calls, name), errors="replace") as f:
            files[int(name.split(".")[1])] = f.read().splitlines()

started, unwatched = 0, collections.Counter()
for pid, lines in files.items():
    for at, line in enumerate(lines):
        spawned = re.match(r"posix_spawnp? (\S+) child (-?\d+) errno 0", line)
        if spawned:
            started += 1
            child = int(spawned.group(2))
            if not any(l.startswith("watched") for l in files.get(child, [])):
                unwatched[os.path.basename(spawned.group(1))] += 1
        executed = re.match(r"execve (\S+)", line)
        if executed:
            started += 1
            later = lines[at + 1:]
            # A failed execve returns, and the old program goes on writing.
            ran = not later or later[0].startswith("watched")
            if not later:
                unwatched[os.path.basename(executed.group(1))] += 1
            elif not ran:
                started -= 1
        if re.match(r"fork child \d+", line):
            started += 1
            child = int(line.split()[2])
            if child not in files:
                unwatched["(fork)"] += 1

lost = sum(unwatched.values())
if not files:
    # No record at all: the library never loaded, so nothing was seen.
    print(f"{label}: no process recorded -> C cannot vouch for this run (the library did not load)")
    sys.exit(0)
print(f"{label}: {len(files)} processes recorded, {started} starts, {lost} unwatched"
      + (f": {dict(unwatched.most_common(8))}" if lost else "")
      + f" -> {'C would vouch for this run' if lost == 0 else 'C would refuse this run'}")
