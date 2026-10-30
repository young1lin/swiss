#!/usr/bin/env python3
# Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.

"""Package the native swiss executable as a locally signed macOS application."""

import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import shutil
import stat
import struct
import subprocess
import sys
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent.parent


def make_bundle(binary: Path, output: Path, version: str) -> Path:
    with binary.open("rb") as source:
        header = source.read(8)
    if len(header) != 8 or struct.unpack("<II", header) not in (
        (0xFEEDFACF, 0x0100000C), (0xFEEDFACF, 0x01000007)
    ):
        raise ValueError("expected a 64-bit macOS executable, not a Windows or Linux binary")
    app = output / "swiss.app"
    if app.exists():
        raise FileExistsError(f"refusing to replace an existing app: {app}")
    macos = app / "Contents" / "MacOS"
    resources = app / "Contents" / "Resources"
    macos.mkdir(parents=True)
    resources.mkdir()
    executable = macos / "swiss"
    shutil.copyfile(binary, executable)
    executable.chmod(0o755)
    for name in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.md"):
        shutil.copyfile(ROOT / name, resources / name)
    shutil.copyfile(ROOT / "assets" / "icon-macos.icns", resources / "swiss.icns")
    info = {
        "CFBundleExecutable": "swiss", "CFBundleIdentifier": "dev.swiss.toolbox",
        "CFBundleName": "swiss", "CFBundleDisplayName": "swiss",
        "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version,
        "CFBundleVersion": version, "CFBundleIconFile": "swiss.icns",
        "LSMinimumSystemVersion": "14.0", "NSHighResolutionCapable": True,
        "LSApplicationCategoryType": "public.app-category.developer-tools",
        "NSAppTransportSecurity": {"NSAllowsLocalNetworking": True},
    }
    with (app / "Contents" / "Info.plist").open("wb") as target:
        plistlib.dump(info, target)
    (app / "Contents" / "PkgInfo").write_bytes(b"APPL????")
    return app


def archive_bundle(app: Path) -> Path:
    archive = app.with_suffix(".app.zip")
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as target:
        for path in sorted(app.rglob("*")):
            if not path.is_file():
                continue
            entry = zipfile.ZipInfo(path.relative_to(app.parent).as_posix())
            entry.create_system = 3
            mode = 0o755 if path == app / "Contents" / "MacOS" / "swiss" else 0o644
            entry.external_attr = (stat.S_IFREG | mode) << 16
            entry.compress_type = zipfile.ZIP_DEFLATED
            target.writestr(entry, path.read_bytes())
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_suffix(archive.suffix + ".sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    return archive


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    parser.add_argument("--version", default=version)
    parser.add_argument("--rcodesign", help="cross-platform signer; native macOS uses codesign")
    args = parser.parse_args()
    if not args.rcodesign and sys.platform != "darwin":
        parser.error("cross packaging requires --rcodesign; native codesign runs only on macOS")
    app = make_bundle(args.binary.resolve(), args.output.resolve(), args.version)
    if args.rcodesign:
        subprocess.run([args.rcodesign, "sign", "--config-file", os.devnull, str(app)], check=True)
        # rcodesign 0.29's verify command rejects the intentionally absent CMS of an ad-hoc
        # signature. Inspect its structure here; strict native verification remains below.
        subprocess.run([args.rcodesign, "print-signature-info", "--config-file", os.devnull, str(app / "Contents" / "MacOS" / "swiss")], check=True)
        print("Ad-hoc signature created; native Gatekeeper assessment requires macOS.")
    else:
        subprocess.run(["codesign", "--force", "--sign", "-", str(app)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
    print(app)
    print(archive_bundle(app))


if __name__ == "__main__":
    main()
