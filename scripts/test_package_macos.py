# Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.

"""SPEC §release.ci: bundle entry point, local networking and executable permission contract."""

from pathlib import Path
import plistlib
import struct
import tempfile
import unittest
import zipfile

from package_macos import archive_bundle, make_bundle


class MacBundleTests(unittest.TestCase):
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
