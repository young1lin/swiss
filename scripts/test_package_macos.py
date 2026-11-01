# Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.

"""SPEC §release.ci: bundle entry point, local networking and executable permission contract."""

from pathlib import Path
import plistlib
import struct
import tempfile
import unittest
import zipfile
import subprocess
from unittest.mock import patch

from package_macos import archive_bundle, main, make_bundle


class MacBundleTests(unittest.TestCase):
    def test_missing_and_truncated_inputs_do_not_create_a_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            with self.assertRaises(FileNotFoundError):
                make_bundle(output / "absent", output / "dist", "0.1.4")
            binary = output / "swiss"
            binary.write_bytes(b"\xcf\xfa\xed\xfe")
            with self.assertRaises(ValueError):
                make_bundle(binary, output / "dist", "0.1.4")
            self.assertFalse((output / "dist").exists())

    def test_signing_and_verification_failures_never_publish_an_archive(self):
        for failed_call in (0, 1):
            with self.subTest(failed_call=failed_call), tempfile.TemporaryDirectory() as directory:
                output = Path(directory)
                binary = output / "swiss"
                binary.write_bytes(struct.pack("<II", 0xFEEDFACF, 0x0100000C) + b"fixture")
                outcomes = [None] * failed_call + [subprocess.CalledProcessError(1, "signer")]
                argv = ["package_macos", str(binary), "--output", str(output / "dist"), "--rcodesign", "fixture-signer"]
                with patch("sys.argv", argv), patch("package_macos.subprocess.run", side_effect=outcomes):
                    with self.assertRaises(subprocess.CalledProcessError):
                        main()
                self.assertFalse((output / "dist/swiss.app.zip").exists())
                self.assertFalse((output / "dist/swiss.app.zip.sha256").exists())

    def test_bundle_and_archive_preserve_the_native_entry_point_and_execution_mode(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            binary = output / "swiss"
            binary.write_bytes(struct.pack("<II", 0xFEEDFACF, 0x0100000C) + b"fixture")
            app = make_bundle(binary, output / "dist", "0.1.4")
            with (app / "Contents" / "Info.plist").open("rb") as source:
                info = plistlib.load(source)
            self.assertEqual(info["CFBundleExecutable"], "swiss")
            self.assertEqual(info["CFBundlePackageType"], "APPL")
            self.assertEqual(info["NSAppTransportSecurity"], {"NSAllowsLocalNetworking": True})
            with zipfile.ZipFile(archive_bundle(app)) as archive:
                entry = archive.getinfo("swiss.app/Contents/MacOS/swiss")
                self.assertEqual((entry.external_attr >> 16) & 0o777, 0o755)
            with self.assertRaises(FileExistsError):
                make_bundle(binary, output / "dist", "0.1.4")

    def test_a_windows_binary_cannot_be_packaged_as_a_mac_application(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            binary = output / "swiss.exe"
            binary.write_bytes(b"MZ" + bytes(30))
            with self.assertRaises(ValueError):
                make_bundle(binary, output / "dist", "0.1.4")
            self.assertFalse((output / "dist").exists())


if __name__ == "__main__":
    unittest.main()
