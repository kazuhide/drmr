"""Check Desktop package boundaries without launching a native application."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("build_desktop", Path(__file__).resolve().parents[1] / "build_desktop.py")
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


class DesktopPackages(unittest.TestCase):
    def test_notices_keep_the_runtime_graph_and_mark_metadata_only_publishers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "LICENSE").write_text("project notice")
            packages = []
            for name in ("drmr", "drmr-desktop", "runtime", "metadata-only", "development-only"):
                home = root / name
                home.mkdir()
                (home / "Cargo.toml").touch()
                if name == "runtime":
                    (home / "LICENSE-MIT").write_text("runtime notice")
                packages.append({"id": name, "name": name, "version": "1.0.0", "authors": ["Publisher"],
                                 "manifest_path": str(home / "Cargo.toml"), "license": "MIT", "source": "registry+example"})
            nodes = [{"id": p["id"], "deps": []} for p in packages]
            nodes[1]["deps"] = [{"pkg": name, "dep_kinds": [{"kind": kind}]} for name, kind in
                                 (("runtime", None), ("metadata-only", "build"), ("development-only", "dev"))]
            output = root / "notices"
            with patch.object(BUILD, "ROOT", root):
                BUILD.collect_notices({"packages": packages, "resolve": {"nodes": nodes}}, output)
            raw = (output / "inventory.json").read_text()
            inventory = {p["name"]: p for p in json.loads(raw)}
            self.assertEqual(set(inventory), {"drmr", "drmr-desktop", "runtime", "metadata-only"})
            self.assertEqual((output / "runtime-1.0.0" / "LICENSE-MIT").read_text(), "runtime notice")
            self.assertEqual(inventory["metadata-only"]["notice_status"], "publisher-metadata-only")
            self.assertNotIn(str(root), raw)

    def test_an_existing_bundle_is_never_replaced(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "desktop-bundles" / "drmr-desktop"
            output.mkdir(parents=True)
            (output / "sentinel").write_bytes(b"original")
            with patch.object(BUILD.sys, "platform", "linux"):
                with self.assertRaisesRegex(ValueError, "^bundle-destination-exists"):
                    BUILD.bundle(root, "", "1.0.0", root)
            self.assertEqual((output / "sentinel").read_bytes(), b"original")

    def test_mac_bundle_keeps_both_binaries_and_dependency_notices(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "profile"
            profile.mkdir()
            for name in ("drmr", "drmr-desktop"):
                (profile / name).write_bytes(name.encode())
                (profile / name).chmod(0o755)
            resources = root / "desktop" / "bundle-resources"
            resources.mkdir(parents=True)
            icons = root / "desktop" / "icons"
            icons.mkdir()
            (icons / "icon.icns").write_bytes(b"icon")
            networking = {"NSExceptionDomains": {"127.0.0.1": {"NSExceptionAllowsInsecureHTTPLoads": True}}}
            (root / "desktop" / "Info.plist").write_bytes(BUILD.plistlib.dumps({"NSAppTransportSecurity": networking}))
            (resources / "inventory.json").write_text("[]")
            (root / "docs").mkdir()
            (root / "docs" / "desktop.md").write_text("guide")
            for name in ("LICENSE", "README.md"):
                (root / name).write_text(name)
            with patch.object(BUILD, "ROOT", root), patch.object(BUILD, "DESKTOP", root / "desktop"), patch.object(BUILD.sys, "platform", "darwin"):
                bundle = BUILD.bundle(profile, "", "1.0.0", root)
            info = BUILD.plistlib.loads((bundle / "Contents" / "Info.plist").read_bytes())
            self.assertEqual(info["NSAppTransportSecurity"], networking)
            self.assertEqual((bundle / "Contents" / "MacOS" / "drmr").read_bytes(), b"drmr")
            self.assertEqual((bundle / "Contents" / "MacOS" / "drmr-desktop").read_bytes(), b"drmr-desktop")
            self.assertEqual((bundle / "Contents" / "Resources" / "licenses" / "inventory.json").read_text(), "[]")
            self.assertEqual((bundle / "Contents" / "Resources" / "docs" / "desktop.md").read_text(), "guide")
            with patch.object(BUILD.sys, "platform", "darwin"):
                archive = BUILD.archive_bundle(bundle)
            with BUILD.tarfile.open(archive) as stream:
                cli = stream.getmember("drmr.app/Contents/MacOS/drmr")
                self.assertEqual(cli.mode & 0o777, 0o755)
                self.assertEqual(cli.uname, "")
                self.assertEqual(stream.extractfile(cli).read(), b"drmr")
