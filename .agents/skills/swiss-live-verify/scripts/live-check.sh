#!/usr/bin/env bash
# Copyright 2026 young1lin
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     https://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# Cross-platform sibling of live-check.ps1 (macOS / Linux).
#
# Same contract, same behaviour: stale-port guard, target-test build, isolated test home on
# a REAL-state snapshot, legacy-name token pin, and the served-build proof (/health hash vs
# the exe's --version hash). Never touches 19999. The repo's own scripts/test-instance.ps1
# is Windows-only, so on POSIX this script implements the boot procedure directly.
#
#   live-check.sh               # build into target-test, boot snapshot instance, prove, leave up
#   live-check.sh --skip-build  # reuse the existing target-test exe
#   live-check.sh --fresh       # wipe the test home first (clean-instance scenarios)
#   live-check.sh --stop        # stop whatever owns port 19998, by pid, never by name

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
PORT=19998
# The same literal acceptance-16.ps1 pins: arbitrary, but deliberately the legacy name,
# because the snapshotted production config's tokenEnv still says MCP_GATEWAY_TOKEN.
TOKEN='acceptance-token-for-1998'
TEST_HOME="${SWISS_TEST_HOME:-$HOME/.swiss-test-home}"
# ~/.swiss since the 2026-09-18 rename; ~/.mcp-gateway until the next production start moves it.
PROD_HOME="$HOME/.swiss"
[ -f "$PROD_HOME/gateway.config.json" ] || PROD_HOME="$HOME/.mcp-gateway"
EXE="$ROOT/target-test/release/swiss"
# Sealed state worth snapshotting - same list as test-instance.ps1. master.key is optional
# here: on mac the key lives in the Keychain, on Linux in secret-tool or the machine id,
# all of which resolve for the same machine+user without any key file.
STATE_FILES=(master.key gateway.config.json managed.json tunnels.json jobs.json jobs-state.json env.json)

SKIP_BUILD=0; FRESH=0; DO_STOP=0
for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=1 ;;
        --fresh) FRESH=1 ;;
        --stop) DO_STOP=1 ;;
        *) echo "unknown flag: $arg (use --skip-build | --fresh | --stop)" >&2; exit 1 ;;
    esac
done

# Find the pid(s) owning the port - lsof everywhere, ss as the Linux fallback.
port_pids() {
    if command -v lsof >/dev/null 2>&1; then
        lsof -ti "tcp:$PORT" -sTCP:LISTEN 2>/dev/null || true
    else
        ss -ltnpH "sport = :$PORT" 2>/dev/null | sed -n 's/.*pid=\([0-9]*\).*/\1/p'
    fi
}

if [ "$DO_STOP" -eq 1 ]; then
    pids="$(port_pids)"
    if [ -z "$pids" ]; then echo "nothing is listening on $PORT"; exit 0; fi
    # Hard kill: the serve path writes no pid file, so 'swiss stop' has nothing to act on.
    echo "killing pid(s) $(echo "$pids" | tr '\n' ' ')- the processes that own port $PORT"
    # shellcheck disable=SC2086
    kill $pids 2>/dev/null || true
    for _ in $(seq 1 50); do [ -z "$(port_pids)" ] && break; sleep 0.2; done
    if [ -n "$(port_pids)" ]; then echo "port $PORT is still held" >&2; exit 1; fi
    echo "$PORT stopped; 19999 (production) untouched"
    exit 0
fi

# A stale instance from an earlier session holding the port would serve the WRONG build.
if [ -n "$(port_pids)" ]; then
    echo "port $PORT is already held; run this script with --stop first" >&2
    exit 1
fi

if [ "$SKIP_BUILD" -eq 0 ]; then
    echo '== building target-test exe'
    (cd "$ROOT" && CARGO_TARGET_DIR=target-test cargo build --release)
fi
if [ ! -x "$EXE" ]; then echo "no test binary at $EXE - drop --skip-build or build first" >&2; exit 1; fi

# Snapshot production state into the isolated test home (copied, never linked).
if [ "$FRESH" -eq 1 ] && [ -d "$TEST_HOME" ]; then rm -rf "$TEST_HOME"; fi
mkdir -p "$TEST_HOME"
copied=()
for f in "${STATE_FILES[@]}"; do
    if [ -f "$PROD_HOME/$f" ]; then cp "$PROD_HOME/$f" "$TEST_HOME/$f"; copied+=("$f"); fi
done
echo "test home: $TEST_HOME (snapshotted: ${copied[*]:-nothing to copy})"

# Env vars, never --port: the flag writes itself into the config. Pin the token BEFORE boot
# so the serve process (and its children) inherit the known bearer.
export SWISS_HOME="$TEST_HOME"
export SWISS_PORT="$PORT"
export MCP_GATEWAY_TOKEN="$TOKEN"

"$EXE" serve >"$TEST_HOME/serve.out" 2>"$TEST_HOME/serve.err" &
PID=$!
echo "started pid $PID; waiting for /health (20s budget)"
healthy=0
for _ in $(seq 1 80); do
    kill -0 "$PID" 2>/dev/null || break
    if curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then healthy=1; break; fi
    sleep 0.25
done
if [ "$healthy" -ne 1 ]; then
    echo "FAILED to come up on $PORT" >&2
    kill "$PID" 2>/dev/null || true
    for log in serve.err serve.out; do
        [ -f "$TEST_HOME/$log" ] && { echo "--- last lines of $log ---"; tail -20 "$TEST_HOME/$log"; }
    done
    exit 1
fi

# The proof: what is serving is THIS build. Version line shape: 'swiss <ver> (<hash>, <date>)'.
version="$("$EXE" --version | head -1)"
built="$(printf '%s' "$version" | sed -n 's/^swiss [^ ]* (\([0-9a-f]*\),.*/\1/p')"
if [ -z "$built" ]; then echo "no build hash in version line: $version" >&2; exit 1; fi
health_hash="$(curl -s "http://127.0.0.1:$PORT/health" | sed -n 's/.*"hash":"\([0-9a-f]*\)".*/\1/p')"
info_hash="$(curl -s -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$PORT/api/info" | sed -n 's/.*"hash":"\([0-9a-f]*\)".*/\1/p')"
echo "exe --version : $version"
echo "health.build  : $health_hash"
echo "info.build    : $info_hash"
if [ "$health_hash" != "$built" ] || [ "$info_hash" != "$built" ]; then
    echo "served build ($health_hash) is not this build ($built) - a stale instance answered" >&2
    exit 1
fi
echo "$PORT is serving this build; state writes go to $TEST_HOME - production config untouched"
echo "bearer for /api/* probes: the pinned MCP_GATEWAY_TOKEN above"
exit 0
