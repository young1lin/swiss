#!/usr/bin/env bash
# One-shot syntax check for every POSIX skill script (run via WSL bash from Windows).
cd <repo> || exit 1
rc=0
while IFS= read -r f; do
    if bash -n "$f"; then
        echo "OK: $f"
    else
        echo "FAIL: $f"; rc=1
    fi
done < <(find .agents/skills -name '*.sh' -path '*/scripts/*' | sort)
exit $rc
