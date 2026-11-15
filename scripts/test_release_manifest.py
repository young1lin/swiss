# Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.

"""SPEC §release.ci: update.json from exes, full archives and patches."""

from pathlib import Path
import io
import tarfile
import tempfile
import unittest
import zipfile

from release_manifest import (
    build_manifest,
    dumps,
    extract_named,
    over_sixty_percent,
    previous_tags,
    publish,
)


def _fill(directory: Path, version: str = "0.2.3") -> None:
    target = directory / "x86_64-pc-windows-msvc"
    target.mkdir(parents=True)
    (target / "exe").write_bytes(b"new-exe-bytes")
    (target / "exe.name").write_text("swiss.exe\n", encoding="utf-8")
    (target / "full.zst").write_bytes(b"f" * 100)
    (target / "full.asset").write_text(
        f"swiss-{version}-x86_64-pc-windows-msvc.zst\n", encoding="utf-8"
    )
    kept = target / "patches" / "0.2.2"
    kept.mkdir(parents=True)
    (kept / "from.exe").write_bytes(b"old-exe")
    (kept / "patch.zst").write_bytes(b"p" * 60)
    (kept / "asset").write_text(
        f"swiss-0.2.2-to-{version}-x86_64-pc-windows-msvc.patch.zst\n", encoding="utf-8"
    )
    (kept / "window").write_text("27\n", encoding="utf-8")
    dropped = target / "patches" / "0.2.1"
    dropped.mkdir()
    (dropped / "from.exe").write_bytes(b"older-exe")
    (dropped / "patch.zst").write_bytes(b"q" * 61)
    (dropped / "asset").write_text(
        f"swiss-0.2.1-to-{version}-x86_64-pc-windows-msvc.patch.zst\n", encoding="utf-8"
    )
    (dropped / "window").write_text("27\n", encoding="utf-8")


class ReleaseManifestTests(unittest.TestCase):
    def test_manifest_keeps_a_patch_at_sixty_percent_and_drops_a_larger_one(self):
        self.assertFalse(over_sixty_percent(60, 100))
        self.assertTrue(over_sixty_percent(61, 100))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _fill(root)
            document = build_manifest(
                root, "0.2.3", "v0.2.3", "https://github.com/young1lin/swiss/releases/tag/v0.2.3"
            )
            patches = document["targets"]["x86_64-pc-windows-msvc"]["patches"]
            self.assertEqual([item["from"] for item in patches], ["0.2.2"])
            self.assertEqual(patches[0]["windowLog"], 27)
            self.assertEqual(patches[0]["size"], 60)
            self.assertEqual(document["targets"]["x86_64-pc-windows-msvc"]["exe"], "swiss.exe")
            self.assertEqual(document["targets"]["x86_64-pc-windows-msvc"]["size"], len(b"new-exe-bytes"))
            published = root / "out"
            publish(document, root, published)
            self.assertTrue((published / "swiss-0.2.3-x86_64-pc-windows-msvc.zst").is_file())
            self.assertTrue(
                (published / "swiss-0.2.2-to-0.2.3-x86_64-pc-windows-msvc.patch.zst").is_file()
            )
            self.assertFalse(
                (published / "swiss-0.2.1-to-0.2.3-x86_64-pc-windows-msvc.patch.zst").exists()
            )

    def test_a_missing_full_asset_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "x86_64-unknown-linux-gnu"
            target.mkdir()
            (target / "exe").write_bytes(b"exe")
            (target / "exe.name").write_text("swiss\n", encoding="utf-8")
            with self.assertRaises(SystemExit) as raised:
                build_manifest(root, "0.2.3", "v0.2.3", "https://github.com/young1lin/swiss/releases/tag/v0.2.3")
            self.assertIn("missing full asset", str(raised.exception))

    def test_key_order_is_stable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _fill(root)
            notes = "https://github.com/young1lin/swiss/releases/tag/v0.2.3"
            first = build_manifest(root, "0.2.3", "v0.2.3", notes)
            second = build_manifest(root, "0.2.3", "v0.2.3", notes)
            text = dumps(first)
            self.assertEqual(text, dumps(second))
            self.assertEqual(list(first.keys()), ["schema", "version", "tag", "notes", "targets"])
            target = first["targets"]["x86_64-pc-windows-msvc"]
            self.assertEqual(list(target.keys()), ["exe", "sha256", "size", "full", "patches"])
            self.assertEqual(list(target["full"].keys()), ["asset", "sha256", "size"])
            self.assertEqual(
                list(target["patches"][0].keys()),
                ["from", "fromSha256", "asset", "sha256", "size", "windowLog"],
            )
            self.assertTrue(text.endswith("\n"))
            self.assertEqual(target["sha256"], target["sha256"].lower())

    def test_previous_tags_are_the_three_newest_below_current(self):
        names = ["v0.2.2", "v0.10.0", "v0.2.3", "nightly", "v0.2.1", "v0.2.0", "v0.1.9", "v0.2.2"]
        self.assertEqual(previous_tags(names, "v0.2.3"), ["v0.2.2", "v0.2.1", "v0.2.0"])
        self.assertEqual(previous_tags(["v0.3.0"], "v0.2.3"), [])

    def test_extract_named_skips_a_directory_that_shares_the_basename(self):
        # The arm runner's tar ordered skills/swiss before swiss; the directory's basename
        # matched first, extractfile answered None, and the release step died naming the
        # file it had just found. Only a regular file may match, whatever the order.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tar_path = root / "swiss.tar.gz"
            buffer = io.BytesIO()
            with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
                folder = tarfile.TarInfo("skills/swiss")
                folder.type = tarfile.DIRTYPE
                archive.addfile(folder)
                payload = b"elf-exe"
                info = tarfile.TarInfo("swiss")
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
            tar_path.write_bytes(buffer.getvalue())
            extracted = root / "swiss"
            extract_named(str(tar_path), "swiss", str(extracted))
            self.assertEqual(extracted.read_bytes(), b"elf-exe")
            bundle = root / "swiss.zip"
            with zipfile.ZipFile(bundle, "w") as archive:
                archive.writestr("skills/swiss/", b"")
                archive.writestr("swiss", b"zip-exe")
            zipped = root / "out"
            extract_named(str(bundle), "swiss", str(zipped))
            self.assertEqual(zipped.read_bytes(), b"zip-exe")

    def test_extract_named_reads_zip_and_tar_members(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle = root / "swiss.zip"
            with zipfile.ZipFile(bundle, "w") as archive:
                archive.writestr("skills\\swiss\\SKILL.md", b"skill")
                archive.writestr("swiss.exe", b"MZ-exe")
            dest = root / "out.exe"
            extract_named(str(bundle), "swiss.exe", str(dest))
            self.assertEqual(dest.read_bytes(), b"MZ-exe")
            tar_path = root / "swiss.tar.gz"
            buffer = io.BytesIO()
            with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
                payload = b"elf-exe"
                info = tarfile.TarInfo("swiss")
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
            tar_path.write_bytes(buffer.getvalue())
            extracted = root / "swiss"
            extract_named(str(tar_path), "swiss", str(extracted))
            self.assertEqual(extracted.read_bytes(), b"elf-exe")
            with self.assertRaises(SystemExit):
                extract_named(str(bundle), "missing", str(root / "nope"))


if __name__ == "__main__":
    unittest.main()
