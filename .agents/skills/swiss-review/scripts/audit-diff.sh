#!/usr/bin/env bash
# Cross-platform sibling of audit-diff.ps1 - a lens, not a gate.
#
# Greps ADDED lines of the outgoing diff for the patterns that breach this repo's
# load-bearing rules. The semantic half stays with the reviewer. Untracked files are not
# covered; exit code is always 0 - findings are printed, not enforced.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
BASE="HEAD"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --base) BASE="$2"; shift 2 ;;
        *) BASE="$1"; shift ;;
    esac
done

count=0
hit() {
    count=$((count + 1))
    echo "[$count] $file :: $line"
    echo "      -> $1"
}

check() { # $1=file  $2=added-line
    file="$1"; line="$2"
    case "$file" in *admin_assets*) hit 'panel tree is a byte-for-byte Node copy - hand edits forbidden; use the recopy procedure (swiss-node-reference scripts/recopy-panel)';; esac
    case "$line" in *"Command::new(\"powershell"*|*"Command::new('powershell"*) hit 'no subprocess where a syscall exists - a PowerShell spawn costs ~65 MB transient working set';; esac
    case "$line" in *multi_thread*) hit 'runtime is current_thread by design - multi_thread needs a measured reason in the commit message';; esac
    if { case "$file" in *.rs|*.toml) true;; *) false;; esac; } && case "$file" in *platform*) false;; *) true;; esac; then
        if printf '%s' "$line" | grep -Eq '(^|[^A-Za-z])unsafe([^A-Za-z]|$)'; then
            hit 'unsafe belongs at the Windows FFI boundary (platform/), nowhere else'
        fi
    fi
    case "$file" in *adapters*|*proc.rs|*http.rs|*rest.rs)
        case "$line" in *serde_json::Value*) hit 'forwarding paths pass payloads as &RawValue - no serde_json::Value materialisation';; esac;; esac
    if case "$file" in *secure*|*adapters*) true;; *) false;; esac; then
        case "$line" in *".unwrap()"*) case "$line" in *"cfg(test)"*|*"mod tests"*) ;; *) hit 'no .unwrap() on config/network/db/filesystem paths - one failing MCP must not take down the others';; esac;; esac
    fi
    case "$file" in *swiss-core/src/secure/envelope*) hit 'the sealed envelope format is FROZEN (docs/05) - envelope construction changes are stop-and-discuss';; esac
    if case "$file" in *Cargo.toml) true;; *) false;; esac; then
        if printf '%s' "$line" | grep -Eq '^[a-zA-Z0-9_-]+[[:space:]]*=[[:space:]]*"[^"]*"[[:space:]]*$'; then
            hit 'new dependency? default-features = false first, justify weight in the commit message (swiss-dependency-review)'
        fi
    fi
    case "$file" in
        *master.key|*gateway.config.json|*managed.json|*tunnels.json|*jobs.json|*.env|*.log|*.cast)
            hit 'never-commit file in the diff - state files and recordings carry real secrets';; esac
}

# Iterate added lines with their file; the loop runs in the main shell so the counter lives.
while IFS=$'\t' read -r file line; do
    [ -n "$file" ] && check "$file" "$line"
done < <(git -C "$ROOT" diff --unified=0 "$BASE" -- 2>/dev/null | awk '
    /^\+\+\+ b\// { file = substr($0, 7); next }
    /^\+\+\+/ { next }
    /^\+/ { gsub(/\t/, " "); print file "\t" substr($0, 2) }
')

echo ''
if [ "$count" -eq 0 ]; then
    echo "no mechanical red flags in the diff against $BASE (untracked files not scanned)"
else
    echo "$count mechanical finding(s) against $BASE - judge each against the rule; this scan never blocks"
fi
exit 0
