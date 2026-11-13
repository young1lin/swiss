#!/bin/sh
# Rebuild tests/fixtures/update from the zstd CLI. The committed bytes are what the
# Rust decoder must accept (ADR-041).
set -eu
cd "$(dirname "$0")"
python3 - <<'PY'
from pathlib import Path
old = bytes(i % 251 for i in range(4096))
new = bytes((i * 3) % 251 for i in range(4096))
Path("old.bin").write_bytes(old)
Path("new.bin").write_bytes(new)
PY
zstd -19 --long=27 --patch-from=old.bin new.bin -o p.patch.zst
