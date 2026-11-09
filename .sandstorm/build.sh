#!/usr/bin/env bash
set -euo pipefail
# Native code is built and staged before invoking spktool pack/dev.
cd /opt/app
test -x build/grain-root/bin/isolate-preview || {
    echo 'Run bash scripts/build-preview.sh and python3 scripts/stage-preview.py first.' >&2
    exit 1
}
