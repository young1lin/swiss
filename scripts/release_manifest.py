#!/usr/bin/env python3
# Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.

"""Write update.json from a directory of exes, full zstd files and patches.

The release job (SPEC §release.ci) fills the directory. This script hashes the
bytes, drops a patch larger than 60% of its target's full archive, and writes
the manifest in a fixed key order.

Layout, relative to --dir:

  <target>/exe
  <target>/exe.name
  <target>/full.zst
  <target>/full.asset
  <target>/patches/<from-version>/from.exe
  <target>/patches/<from-version>/patch.zst
  <target>/patches/<from-version>/asset
  <target>/patches/<from-version>/window
"""

import argparse
import hashlib
import json
import shutil
import tarfile
import zipfile
from pathlib import Path
import sys


def parse_release(tag: str) -> tuple[int, int, int] | None:
    """vMAJOR.MINOR.PATCH, or None. A pre-release suffix does not match."""
    if not tag.startswith("v"):
        return None
    parts = tag[1:].split(".")
    if len(parts) != 3 or not all(part.isdigit() for part in parts):
        return None
    return (int(parts[0]), int(parts[1]), int(parts[2]))


def previous_tags(names: list[str], current: str, limit: int = 3) -> list[str]:
    """The newest `limit` published tags strictly below `current`."""
    current_version = parse_release(current)
    if current_version is None:
        raise SystemExit(f"current tag is not vMAJOR.MINOR.PATCH: {current}")
    found: list[tuple[tuple[int, int, int], str]] = []
    for name in names:
        parsed = parse_release(name)
        if parsed is None or parsed >= current_version:
            continue
        found.append((parsed, name))
    found.sort(reverse=True)
    out: list[str] = []
    seen: set[str] = set()
    for _, name in found:
        if name in seen:
            continue
        seen.add(name)
        out.append(name)
        if len(out) == limit:
            break
    return out


def member_name(raw: str) -> str:
    return Path(raw.replace("\\", "/")).name


def extract_named(archive: str, name: str, dest: str) -> None:
    """Write the archive member whose file name is `name` to `dest`."""
    dest_path = Path(dest)
    dest_path.parent.mkdir(parents=True, exist_ok=True)
    if archive.endswith(".zip"):
        with zipfile.ZipFile(archive) as bundle:
            member = next(
                (item for item in bundle.namelist() if not item.endswith("/") and member_name(item) == name),
                None,
            )
            if member is None:
                raise SystemExit(f"{archive} has no {name}")
            dest_path.write_bytes(bundle.read(member))
        return
    if archive.endswith(".tar.gz") or archive.endswith(".tgz"):
        with tarfile.open(archive, "r:gz") as bundle:
            # A directory can share the basename (skills/swiss next to swiss), and the
            # archiver's order decides which matches first: only a regular file is the exe.
            member = next(
                (item for item in bundle.getmembers() if item.isfile() and member_name(item.name) == name),
                None,
            )
            if member is None:
                raise SystemExit(f"{archive} has no {name}")
            extracted = bundle.extractfile(member)
            if extracted is None:
                raise SystemExit(f"{archive} has no {name}")
            dest_path.write_bytes(extracted.read())
        return
    raise SystemExit(f"unknown archive type: {archive}")


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _text(path: Path) -> str:
    return path.read_text(encoding="utf-8").strip()


def over_sixty_percent(patch_size: int, full_size: int) -> bool:
    """True when the patch is strictly larger than 60% of the full archive."""
    return patch_size * 10 > full_size * 6


def build_manifest(directory: Path, version: str, tag: str, notes: str) -> dict:
    if parse_release(tag) != _version_tuple(version):
        raise SystemExit(f"tag {tag} does not match version {version}")
    targets: dict[str, dict] = {}
    for target_dir in sorted(path for path in directory.iterdir() if path.is_dir()):
        full = target_dir / "full.zst"
        exe = target_dir / "exe"
        if not full.is_file():
            raise SystemExit(f"missing full asset for {target_dir.name}")
        if not exe.is_file():
            raise SystemExit(f"missing exe for {target_dir.name}")
        full_asset = _text(target_dir / "full.asset")
        full_size = full.stat().st_size
        patches: list[dict] = []
        patch_root = target_dir / "patches"
        if patch_root.is_dir():
            for patch_dir in sorted(path for path in patch_root.iterdir() if path.is_dir()):
                patch_file = patch_dir / "patch.zst"
                from_exe = patch_dir / "from.exe"
                if not patch_file.is_file() or not from_exe.is_file():
                    raise SystemExit(f"incomplete patch {target_dir.name}/{patch_dir.name}")
                patch_size = patch_file.stat().st_size
                if over_sixty_percent(patch_size, full_size):
                    continue
                patches.append({
                    "from": patch_dir.name,
                    "fromSha256": _sha256(from_exe),
                    "asset": _text(patch_dir / "asset"),
                    "sha256": _sha256(patch_file),
                    "size": patch_size,
                    "windowLog": int(_text(patch_dir / "window")),
                })
        patches.sort(key=lambda item: _version_tuple(item["from"]), reverse=True)
        targets[target_dir.name] = {
            "exe": _text(target_dir / "exe.name"),
            "sha256": _sha256(exe),
            "size": exe.stat().st_size,
            "full": {"asset": full_asset, "sha256": _sha256(full), "size": full_size},
            "patches": patches,
        }
    return {
        "schema": 1,
        "version": version,
        "tag": tag,
        "notes": notes,
        "targets": targets,
    }


def _version_tuple(version: str) -> tuple[int, int, int]:
    parsed = parse_release(version if version.startswith("v") else f"v{version}")
    if parsed is None:
        raise SystemExit(f"not a MAJOR.MINOR.PATCH version: {version}")
    return parsed


def dumps(document: dict) -> str:
    return json.dumps(document, indent=2) + "\n"


def publish(document: dict, directory: Path, dest: Path) -> None:
    """Copy each full archive and each patch the manifest kept."""
    dest.mkdir(parents=True, exist_ok=True)
    for target, info in document["targets"].items():
        target_dir = directory / target
        shutil.copyfile(target_dir / "full.zst", dest / info["full"]["asset"])
        kept = {patch["asset"] for patch in info["patches"]}
        patch_root = target_dir / "patches"
        if not patch_root.is_dir():
            continue
        for patch_dir in patch_root.iterdir():
            asset = _text(patch_dir / "asset")
            if asset in kept:
                shutil.copyfile(patch_dir / "patch.zst", dest / asset)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--notes", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--publish", type=Path)
    args = parser.parse_args()
    document = build_manifest(args.dir, args.version, args.tag, args.notes)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(dumps(document), encoding="utf-8")
    if args.publish is not None:
        publish(document, args.dir, args.publish)
    print(args.output)


if __name__ == "__main__":
    sys.exit(main() or 0)
