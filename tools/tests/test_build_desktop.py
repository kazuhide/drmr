"""Check Desktop package boundaries without launching a native application."""

import importlib.util
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("build_desktop", Path(__file__).resolve().parents[1] / "build_desktop.py")
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


def pin_notice(root, package):
    home = Path(package["manifest_path"]).parent
    revision = "1" * 40
    (home / ".cargo_vcs_info.json").write_text(json.dumps({"git": {"sha1": revision}}))
    folder = root / "tools" / "desktop-licenses"
    folder.mkdir(parents=True)
    notice = folder / "LICENSE"
    notice.write_bytes(b"Synthetic publisher notice for the packaging test")
    entry = {"name": package["name"], "version": package["version"], "license": package["license"],
             "repository": package.get("repository"), "revision": revision, "selected_license": package["license"],
             "notices": [{"file": "LICENSE", "source_url": "https://example.invalid/pinned/LICENSE",
                           "sha256": hashlib.sha256(notice.read_bytes()).hexdigest()}]}
    (folder / "manifest.json").write_text(json.dumps({"schema_version": 1, "packages": [entry]}))
    return entry


def missing_fixture(root):
    (root / "LICENSE").write_text("project notice")
    packages = []
    for name in ("drmr-desktop", "unresolved"):
        (root / name).mkdir()
        (root / name / "Cargo.toml").touch()
        packages.append({"id": name, "name": name, "version": "1.0.0", "authors": ["Publisher"],
                         "manifest_path": str(root / name / "Cargo.toml"), "license": "MIT"})
    nodes = [{"id": "drmr-desktop", "deps": [{"pkg": "unresolved", "dep_kinds": [{"kind": None}]}]},
             {"id": "unresolved", "deps": []}]
    return {"packages": packages, "resolve": {"nodes": nodes}}


class DesktopPackages(unittest.TestCase):
    def test_notices_keep_runtime_dependencies_pinned_text_and_web_licenses(self):
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
                    (home / "native").mkdir()
                    (home / "native" / "NOTICE.txt").write_text("native component notice")
                    (home / "src").mkdir()
                    (home / "src" / "copying.rs").write_text("not a license document")
                packages.append({"id": name, "name": name, "version": "1.0.0", "authors": ["Publisher"],
                                 "manifest_path": str(home / "Cargo.toml"), "license": "MIT", "source": "registry+example"})
            nodes = [{"id": p["id"], "deps": []} for p in packages]
            nodes[1]["deps"] = [{"pkg": name, "dep_kinds": [{"kind": kind}]} for name, kind in
                                 (("runtime", None), ("metadata-only", "build"), ("development-only", "dev"))]
            pin_notice(root, packages[3])
            font = root / "crates" / "drmr-server" / "static" / "fonts" / "OFL.txt"
            font.parent.mkdir(parents=True)
            font.write_text("font notice")
            vendor = root / "vendor" / "process-wrap"
            vendor.mkdir(parents=True)
            for name in ("COPYRIGHT", "LICENSE-MIT", "LICENSE-APACHE"):
                (vendor / name).write_text("vendored " + name)
            output = root / "notices"
            with patch.object(BUILD, "ROOT", root):
                BUILD.collect_notices({"packages": packages, "resolve": {"nodes": nodes}}, output)
            raw = (output / "inventory.json").read_text()
            inventory = {p["name"]: p for p in json.loads(raw)}
            self.assertEqual(set(inventory), {"drmr", "drmr-desktop", "runtime", "metadata-only"})
            self.assertEqual((output / "runtime-1.0.0" / "LICENSE-MIT").read_text(), "runtime notice")
            self.assertEqual((output / "runtime-1.0.0" / "native" / "NOTICE.txt").read_text(), "native component notice")
            self.assertFalse((output / "runtime-1.0.0" / "src" / "copying.rs").exists())
            self.assertEqual(inventory["metadata-only"]["notice_status"], "publisher-pinned-notices")
            self.assertEqual((output / "metadata-only-1.0.0" / "LICENSE").read_bytes(), b"Synthetic publisher notice for the packaging test")
            self.assertEqual((output / "web-assets" / "fonts" / "OFL.txt").read_text(), "font notice")
            for name in ("COPYRIGHT", "LICENSE-MIT", "LICENSE-APACHE"):
                self.assertEqual((output / "process-wrap" / name).read_text(), "vendored " + name)
            self.assertNotIn(str(root), raw)

    def test_a_declared_license_without_a_notice_text_cannot_be_bundled(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata = missing_fixture(root)
            with patch.object(BUILD, "ROOT", root):
                with self.assertRaisesRegex(ValueError, "^dependency-license-missing: unresolved"):
                    BUILD.collect_notices(metadata, root / "notices", roots=("drmr-desktop",))
            self.assertFalse((root / "notices" / "inventory.json").exists())

    def test_a_nested_notice_does_not_replace_a_missing_package_license(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata = missing_fixture(root)
            nested = root / "unresolved" / "third-party" / "NOTICE.txt"
            nested.parent.mkdir()
            nested.write_text("A notice for a different included component")
            with patch.object(BUILD, "ROOT", root), self.assertRaisesRegex(ValueError, "^dependency-license-missing"):
                BUILD.collect_notices(metadata, root / "notices", roots=("drmr-desktop",))

    def test_changed_or_misbound_pinned_notices_are_refused(self):
        for mutation in ("checksum", "revision", "version", "license", "path"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                metadata = missing_fixture(root)
                entry = pin_notice(root, metadata["packages"][1])
                folder = root / "tools" / "desktop-licenses"
                if mutation == "checksum":
                    (folder / "LICENSE").write_text("changed notice")
                elif mutation == "revision":
                    entry["revision"] = "2" * 40
                elif mutation == "version":
                    entry["version"] = "2.0.0"
                elif mutation == "license":
                    entry["license"] = "BSD-3-Clause"
                else:
                    entry["notices"][0]["file"] = "../outside"
                (folder / "manifest.json").write_text(json.dumps({"schema_version": 1, "packages": [entry]}))
                with patch.object(BUILD, "ROOT", root), self.assertRaises(ValueError):
                    BUILD.collect_notices(metadata, root / "notices", roots=("drmr-desktop",))
                self.assertFalse((root / "notices" / "inventory.json").exists())

    def test_cli_roots_do_not_collect_or_require_optional_gui_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata = missing_fixture(root)
            cli = dict(metadata["packages"][0], id="drmr", name="drmr")
            metadata["packages"].append(cli)
            metadata["resolve"]["nodes"].append({"id": "drmr", "deps": []})
            with patch.object(BUILD, "ROOT", root):
                BUILD.collect_notices(metadata, root / "notices", roots=("drmr",))
            rows = json.loads((root / "notices" / "inventory.json").read_text())
            self.assertEqual([row["name"] for row in rows], ["drmr"])
            self.assertFalse((root / "notices" / "unresolved-1.0.0").exists())

    def test_mpl_notice_explains_where_complete_unmodified_source_is_available(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata = missing_fixture(root)
            package = metadata["packages"][1]
            package.update(license="MPL-2.0", source="registry+https://example.invalid")
            pin_notice(root, package)
            with patch.object(BUILD, "ROOT", root):
                BUILD.collect_notices(metadata, root / "notices", roots=("drmr-desktop",))
            source = (root / "notices" / "unresolved-1.0.0" / "SOURCE.txt").read_text()
            self.assertIn("complete, unmodified source", source)
            self.assertIn("https://static.crates.io/crates/unresolved/unresolved-1.0.0.crate", source)
            self.assertIn("https://mozilla.org/MPL/2.0/", source)
            self.assertNotIn(str(root), source)

    def test_an_existing_notice_destination_is_never_replaced(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata = missing_fixture(root)
            output = root / "notices"
            output.mkdir()
            (output / "operator-file").write_bytes(b"original")
            with patch.object(BUILD, "ROOT", root):
                with self.assertRaisesRegex(ValueError, "^dependency-notice-destination-exists"):
                    BUILD.collect_notices(metadata, output, roots=("drmr-desktop",))
            self.assertEqual((output / "operator-file").read_bytes(), b"original")

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
                for name in ("drmr", "drmr-desktop"):
                    binary = stream.getmember("drmr.app/Contents/MacOS/" + name)
                    self.assertEqual(binary.mode & 0o777, 0o755)
                    self.assertEqual(binary.uname, "")
                    self.assertEqual(stream.extractfile(binary).read(), name.encode())


    def test_tar_binary_permissions_do_not_depend_on_host_executable_bits(self):
        for platform in ("darwin", "linux"):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                bundle = root / ("drmr.app" if platform == "darwin" else "drmr-desktop")
                binaries = bundle / "Contents" / "MacOS" if platform == "darwin" else bundle
                binaries.mkdir(parents=True)
                for name in ("drmr", "drmr-desktop"):
                    (binaries / name).write_bytes(name.encode())
                    (binaries / name).chmod(0o666)
                notice = bundle / "NOTICE.txt"
                notice.write_text("publisher notice")
                notice.chmod(0o666)
                with patch.object(BUILD.sys, "platform", platform):
                    archive = BUILD.archive_bundle(bundle)
                with BUILD.tarfile.open(archive) as stream:
                    prefix = bundle.name + ("/Contents/MacOS/" if platform == "darwin" else "/")
                    for name in ("drmr", "drmr-desktop"):
                        binary = stream.getmember(prefix + name)
                        self.assertEqual(binary.mode & 0o777, 0o755)
                        self.assertEqual(stream.extractfile(binary).read(), name.encode())
                    document = stream.getmember(bundle.name + "/NOTICE.txt")
                    self.assertEqual(document.mode & 0o777, 0o666)
                    self.assertEqual(stream.extractfile(document).read(), b"publisher notice")
