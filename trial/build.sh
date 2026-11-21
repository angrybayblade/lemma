#!/bin/sh
# Builds the probe for both architectures and the injected library (candidate C) as a universal
# binary. The library needs an arm64e slice: dyld kills an arm64e process (/bin/sh) it cannot load into.
set -eux
cd "$(dirname "$0")"
clang -O1 -arch arm64 -o probe probe.c
clang -O1 -arch x86_64 -o probe-x86 probe.c
clang -O1 -dynamiclib -arch arm64 -arch arm64e -arch x86_64 -o watch.dylib watch.c
