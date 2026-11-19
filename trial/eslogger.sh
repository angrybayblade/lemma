#!/bin/sh
# Runs a command under eslogger (candidate A) and leaves the events in OUT/events.json.
# Usage: eslogger.sh OUT COMMAND...
set -u
out=$1; shift
mkdir -p "$out"
events="access chdir clone copyfile create exchangedata exec exit fork fsgetpath getattrlist getextattr link listextattr lookup open readdir readlink rename searchfs stat truncate unlink write"
echo "== starting eslogger"
# eslogger mutes every process in its own process group, so it gets a group of its own.
sudo -n /usr/bin/perl -e 'setpgrp(0, 0) or die "setpgrp: $!"; exec @ARGV or die "exec: $!"' /usr/bin/eslogger $events > "$out/events.json" 2> "$out/eslogger-err.txt" &
sleep 4
if ! pgrep -x eslogger > /dev/null; then
  echo "eslogger did not start:"; cat "$out/eslogger-err.txt"; exit 1
fi
echo "== eslogger up: $(wc -l < "$out/events.json") events so far; stderr: $(cat "$out/eslogger-err.txt")"
start=$(date +%s)
echo "== running: $*"
echo "== script pgid $(ps -o pgid= -p $$) eslogger pgid $(ps -o pgid= -p $(pgrep -x eslogger | head -1))"
"$@" > "$out/command.txt" 2>&1 &
command=$!
waited=0
while kill -0 $command 2>/dev/null && [ $waited -lt 120 ]; do sleep 1; waited=$((waited + 1)); done
if kill -0 $command 2>/dev/null; then
  echo "== command still running after 120s; last output:"; tail -5 "$out/command.txt"
  ps -axo pid,ppid,stat,comm | awk -v p=$command '$1==p || $2==p'
  pkill -9 -P $command; kill -9 $command
fi
wait $command; status=$?
end=$(date +%s)
echo "== command ended: status $status after $((end - start))s"
sleep 3
echo "== stopping eslogger"
sudo -n pkill -INT -x eslogger
waited=0
while pgrep -x eslogger > /dev/null && [ $waited -lt 30 ]; do sleep 1; waited=$((waited + 1)); done
pgrep -x eslogger > /dev/null && { echo "== eslogger ignored SIGINT; killing"; sudo -n pkill -9 -x eslogger; }
sudo -n chown "$(id -u)" "$out/events.json"
echo "status $status seconds $((end - start)) events $(wc -l < "$out/events.json") bytes $(wc -c < "$out/events.json")" | tee "$out/summary.txt"
