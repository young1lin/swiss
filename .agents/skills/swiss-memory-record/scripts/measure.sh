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

# Cross-platform sibling of measure.ps1 - the docs/01 memory loop (records, not gates).
#
# Boots the isolated 19998 instance via live-check.sh, reads /health + /api/memory +
# swiss status, prints a ready-to-append docs/01 row next to the previous recorded row,
# then stops the instance. The skill body owns what the numbers mean and the append rule.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
BOOT="$ROOT/.agents/skills/swiss-live-verify/scripts/live-check.sh"
DOCS01="$ROOT/docs/01-goals-and-memory-budget.md"
PORT=19998
TEST_HOME="${SWISS_TEST_HOME:-$HOME/.swiss-test-home}"
EXE="$ROOT/target-test/release/swiss"
TOKEN='acceptance-token-for-1998'   # pinned by live-check.sh before boot

SKIP_BUILD=0; KEEP=0
for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=1 ;;
        --keep) KEEP=1 ;;
        *) echo "unknown flag: $arg (use --skip-build | --keep)" >&2; exit 1 ;;
    esac
done

if [ "$SKIP_BUILD" -eq 1 ]; then bash "$BOOT" --skip-build; else bash "$BOOT"; fi

health="$(curl -s "http://127.0.0.1:$PORT/health")"
mem="$(curl -s -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$PORT/api/memory")"

field() { printf '%s' "$mem" | sed -n "s/.*\"$1\":\([0-9.]*\).*/\1/p"; }
date_str="$(date +%F)"
hash="$(printf '%s' "$health" | sed -n 's/.*"hash":"\([0-9a-f]*\)".*/\1/p')"
row="| $date_str | $hash | gatewayMb=$(field gatewayMb) privateCommit~=$(field heapUsedMb)/$(field heapTotalMb) childrenMb=$(field childrenMb) (proc children: $(field processCount)) | working set per /api/memory; workload: real-state snapshot on 19998 |"

# swiss status against the test home (env scoped to this invocation only).
SWISS_HOME="$TEST_HOME" SWISS_PORT="$PORT" "$EXE" status 2>&1 | grep -Ei 'memory|build|running|MB' >"$TEST_HOME/status.rows" || true

echo ''
echo '=== ready-to-append docs/01 row (Measured result section; never overwrite a dated row) ==='
echo "$row"
prev="$(grep -n 'MB' "$DOCS01" | tail -1 || true)"
if [ -n "$prev" ]; then echo "previous record (docs/01 $prev)"; fi
echo ''
echo '--- swiss status (memory rows) ---'
cat "$TEST_HOME/status.rows" 2>/dev/null || true

if [ "$KEEP" -ne 1 ]; then
    echo ''
    echo '=== stopping 19998'
    bash "$BOOT" --stop
fi
exit 0
