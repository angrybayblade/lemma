#!/bin/sh
# Runs a command under eslogger (candidate A) and leaves the events in OUT/events.json.
# Usage: eslogger.sh OUT COMMAND...
set -u
out=$1; shift
mkdir -p "$out"
events="access chdir clone copyfile create exchangedata exec exit fork fsgetpath getattrlist getextattr link listextattr lookup open readdir readlink rename searchfs stat truncate unlink write"
sudo -n /usr/bin/eslogger $events > "$out/events.json" 2> "$out/eslogger-err.txt" &
sleep 4
if ! sudo -n pgrep -x eslogger > /dev/null; then
  echo "eslogger did not start:"; cat "$out/eslogger-err.txt"; exit 1
fi
start=$(date +%s)
"$@" > "$out/command.txt" 2>&1
status=$?
end=$(date +%s)
sleep 3
sudo -n pkill -INT -x eslogger
while sudo -n pgrep -x eslogger > /dev/null; do sleep 1; done
sudo -n chown "$(id -u)" "$out/events.json"
echo "status $status seconds $((end - start)) events $(wc -l < "$out/events.json")" | tee "$out/summary.txt"
