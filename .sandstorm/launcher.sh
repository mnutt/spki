#!/usr/bin/env bash
set -euo pipefail
# The pkgdef invokes this native command directly; no HTTP bridge is needed.
exec /bin/isolate-preview grain /bin/isolate-host /app/worker.js
