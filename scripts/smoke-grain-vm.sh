#!/usr/bin/env bash
set -euo pipefail
cd /opt/app
package_root=${SPKI_PACKAGE_ROOT:-/opt/app/build/grain-root}
probe_dir=$(mktemp -d /tmp/spki-grain.XXXXXX)
ln -s /opt/sandstorm/latest/sandstorm "$probe_dir/sandstorm-supervisor"
supervisor_pid=
cleanup() {
    if [[ -n "$supervisor_pid" ]]; then
        sudo kill "$supervisor_pid" 2>/dev/null || true
        wait "$supervisor_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT
start() {
    sudo "$probe_dir/sandstorm-supervisor" --uid "$(id -u)" \
        --pkg "$package_root" --var "$probe_dir/grain" "$@" \
        previewprobe previewprobe /bin/isolate-preview grain /bin/isolate-host /app/worker.js \
        > "$probe_dir/supervisor.log" 2>&1 &
    supervisor_pid=$!
    for attempt in $(seq 1 100); do
        [[ -S "$probe_dir/grain/socket" ]] && break
        kill -0 "$supervisor_pid" 2>/dev/null || { cat "$probe_dir/supervisor.log" >&2; exit 1; }
        sleep 0.1
    done
}
fetch() {
    if ! timeout 30 /opt/app/target/debug/grain-smoke "$probe_dir/grain/socket" "$1"; then
        cat "$probe_dir/supervisor.log" >&2
        sudo cat "$probe_dir/grain/log" >&2 2>/dev/null || true
        return 1
    fi
}
start --new
[[ $(fetch '') == '{"count":0}' ]]
[[ $(fetch increment) == '{"count":1}' ]]
python3 /opt/app/scripts/check-cli.py "$probe_dir/grain/socket"
python3 /opt/app/scripts/check-logs.py "$probe_dir/grain/socket"
python3 /opt/app/scripts/check-http.py "$probe_dir/grain/socket"
python3 /opt/app/scripts/check-replacements.py "$probe_dir/grain/socket"
cleanup
supervisor_pid=
# The supervisor may leave its old socket on disk; remove only this test's socket.
sudo rm -f "$probe_dir/grain/socket"
start
[[ $(fetch '') == '{"count":1}' ]]
[[ $(fetch version) == '{"version":2}' ]]
printf 'PASS: ordinary Sandstorm grain, worker fetch, storage write, restart persistence. Logs: %s\n' "$probe_dir"
# Exercise upgrade recovery while retaining this temporary grain's saved source and storage.
cleanup
supervisor_pid=
sudo rm -f "$probe_dir/grain/socket"
sudo python3 /opt/app/scripts/check-recovery.py "$probe_dir/grain" prepare
start
sudo python3 /opt/app/scripts/check-recovery.py "$probe_dir/grain"
cleanup
supervisor_pid=
sudo rm -f "$probe_dir/grain/socket"
start
[[ $(fetch version) == '{"version":2}' ]]
[[ $(fetch '') == '{"count":1}' ]]
printf 'PASS: recovered revision survives another restart\n'
