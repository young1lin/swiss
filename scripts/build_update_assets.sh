#!/usr/bin/env bash
# Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.
#
# Compress each non-bundle release exe and, when it is small enough, a zstd
# patch from each of the previous three releases. Writes update.json into the
# dist directory and copies the assets the manifest kept. The macOS .app zip
# is left as a manual download (SPEC §release.ci).

set -euo pipefail

DIST="${1:?dist directory}"
VERSION="${2:?version}"
TAG="${3:?tag}"
NOTES="https://github.com/young1lin/swiss/releases/tag/${TAG}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$(mktemp -d)"
trap 'rm -rf "$SRC"' EXIT
export PYTHONPATH="${ROOT}/scripts${PYTHONPATH:+:$PYTHONPATH}"

extract() {
  python3 -c 'from release_manifest import extract_named; import sys; extract_named(sys.argv[1], sys.argv[2], sys.argv[3])' "$1" "$2" "$3"
}

while read -r target exe ext; do
  [[ -z "$target" ]] && continue
  archive="${DIST}/swiss-${VERSION}-${target}.${ext}"
  if [[ ! -f "$archive" ]]; then
    echo "missing archive ${archive}" >&2
    exit 1
  fi
  dir="${SRC}/${target}"
  mkdir -p "$dir"
  extract "$archive" "$exe" "${dir}/exe"
  printf '%s\n' "$exe" > "${dir}/exe.name"
  zstd -19 --long=27 -f "${dir}/exe" -o "${dir}/full.zst"
  printf '%s\n' "swiss-${VERSION}-${target}.zst" > "${dir}/full.asset"
done <<'EOF'
x86_64-pc-windows-msvc swiss.exe zip
x86_64-unknown-linux-gnu swiss tar.gz
aarch64-unknown-linux-gnu swiss tar.gz
aarch64-apple-darwin swiss tar.gz
x86_64-apple-darwin swiss tar.gz
EOF

mapfile -t ALL < <(gh release list --limit 100 --json tagName,isDraft,isPrerelease \
  --jq '.[] | select(.isDraft|not) | select(.isPrerelease|not) | .tagName')
ALL_TAGS="$(printf '%s\n' "${ALL[@]+"${ALL[@]}"}")"
export ALL_TAGS TAG
mapfile -t PREV < <(python3 -c 'import os; from release_manifest import previous_tags; print("\n".join(previous_tags([line for line in os.environ.get("ALL_TAGS", "").splitlines() if line], os.environ["TAG"])))')

while read -r target exe ext; do
  [[ -z "$target" ]] && continue
  for prev in "${PREV[@]+"${PREV[@]}"}"; do
    [[ -z "$prev" ]] && continue
    from_ver="${prev#v}"
    asset="swiss-${from_ver}-${target}.${ext}"
    olddir="$(mktemp -d)"
    if ! gh release download "$prev" --pattern "$asset" --dir "$olddir" --clobber; then
      echo "skip ${asset} on ${prev}: not published"
      rm -rf "$olddir"
      continue
    fi
    if [[ ! -f "${olddir}/${asset}" ]]; then
      echo "skip ${asset} on ${prev}: download was empty"
      rm -rf "$olddir"
      continue
    fi
    patchdir="${SRC}/${target}/patches/${from_ver}"
    mkdir -p "$patchdir"
    extract "${olddir}/${asset}" "$exe" "${patchdir}/from.exe"
    rm -rf "$olddir"
    zstd -19 --long=27 --patch-from="${patchdir}/from.exe" -f "${SRC}/${target}/exe" -o "${patchdir}/patch.zst"
    decoded="$(mktemp)"
    zstd -d --long=27 --patch-from="${patchdir}/from.exe" -o "$decoded" "${patchdir}/patch.zst"
    cmp "$decoded" "${SRC}/${target}/exe"
    rm -f "$decoded"
    printf '%s\n' "swiss-${from_ver}-to-${VERSION}-${target}.patch.zst" > "${patchdir}/asset"
    printf '%s\n' "27" > "${patchdir}/window"
  done
done <<'EOF'
x86_64-pc-windows-msvc swiss.exe zip
x86_64-unknown-linux-gnu swiss tar.gz
aarch64-unknown-linux-gnu swiss tar.gz
aarch64-apple-darwin swiss tar.gz
x86_64-apple-darwin swiss tar.gz
EOF

python3 "${ROOT}/scripts/release_manifest.py" \
  --dir "$SRC" \
  --version "$VERSION" \
  --tag "$TAG" \
  --notes "$NOTES" \
  --output "${DIST}/update.json" \
  --publish "$DIST"
