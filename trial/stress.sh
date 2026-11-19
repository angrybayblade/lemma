#!/bin/sh
# A file-heavy load for measuring eslogger's rate and drops: walk and stat system trees for ~20 seconds.
( /usr/bin/find /usr /System/Library/Frameworks /Library -xdev -exec /usr/bin/stat -q {} + > /dev/null 2>&1 ) &
walker=$!
sleep 20
kill $walker 2>/dev/null
pkill -P $walker 2>/dev/null
exit 0
