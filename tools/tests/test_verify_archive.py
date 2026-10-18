"""Verify actual archive bytes, including failures invisible to packaging mocks."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import stat
import zipfile
import unittest

SPEC = importlib.util.spec_from_file_location("verify_archive", Path(__file__).resolve().parents[1] / "verify_archive.py")
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


def elf():
    data = bytearray(64)
    data[:7] = b"\x7fELF\x02\x01\x01"
    data[16:20] = b"\x02\x00\x3e\x00"
    return bytes(data)


class Archives(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = type("Source", (), {"head": "a" * 40, "tree": "b" * 40})()
        self.payload = b"Publisher notice\nOriginal bytes.\n"
        self.expected = {"schema_version": 1, "kind": "cli", "target": "x86_64-unknown-linux-gnu",
                         "source_head": self.source.head, "source_tree": self.source.tree,
                         "inventory": [], "package_count": 0,
                         "files": {"LICENSE": {"sha256": VERIFY.sha(self.payload), "bytes": len(self.payload), "comparison": "raw"}}}
        self.files = {"drmr": elf(), "LICENSE": self.payload, "licenses/inventory.json": b"[]"}
        self.manifest = {"schema_version": 1, "source_head": self.source.head, "source_tree": self.source.tree,
                         "target": self.expected["target"], "rustc": "rustc test fixture",
                         "binaries": {"drmr": VERIFY.sha(elf())}}

    def archive(self, files=None, mode=0o755, extras=()):
        files = dict(self.files if files is None else files)
        files.setdefault("build-manifest.json", json.dumps(self.manifest).encode())
        path = self.root / "candidate.tar.gz"
        with tarfile.open(path, "w:gz") as stream:
            for name, data in list(files.items()) + list(extras):
                prefix = "drmr-desktop/" if self.expected["kind"] == "desktop" else "package/"
                member = tarfile.TarInfo(prefix + name)
                member.size = len(data)
                member.mode = mode if name in ("drmr", "drmr-desktop") else 0o644
                stream.addfile(member, io.BytesIO(data))
        return path

    def check(self, path):
        return VERIFY.qualify(path, self.source, self.expected)

    def test_exact_archive_and_build_binding(self):
        result = self.check(self.archive())
        self.assertTrue(result["success"])
        self.assertEqual(result["binaries"]["drmr"]["sha256"], VERIFY.sha(elf()))

    def test_raw_publisher_bytes_do_not_accept_equivalent_crlf(self):
        files = dict(self.files, LICENSE=self.payload.replace(b"\n", b"\r\n"))
        with self.assertRaisesRegex(VERIFY.Failure, "archive-source-bytes-mismatch"):
            self.check(self.archive(files))

    def test_missing_extra_duplicate_and_wrong_manifest_refused(self):
        for change, token in [("missing", "archive-file-set"), ("extra", "archive-file-set"),
                              ("duplicate", "archive-duplicate-path"), ("manifest", "build-manifest-binary-mismatch")]:
            with self.subTest(change=change):
                files = dict(self.files)
                extras = ()
                if change == "missing": del files["LICENSE"]
                if change == "extra": files["unexpected.dll"] = b"foreign"
                if change == "duplicate": extras = (("LICENSE", self.payload),)
                if change == "manifest": self.manifest["binaries"]["drmr"] = "0" * 64
                with self.assertRaisesRegex(VERIFY.Failure, token): self.check(self.archive(files, extras=extras))

    def test_wrong_target_and_non_executable_refused(self):
        wrong = bytearray(elf()); wrong[18:20] = (183).to_bytes(2, "little")
        for files, mode, token in [(dict(self.files, drmr=bytes(wrong)), 0o755, "binary-architecture"),
                                   (self.files, 0o644, "binary-executable-permissions"),
                                   (self.files, 0o111, "binary-executable-permissions"),
                                   (self.files, 0o4755, "binary-executable-permissions")]:
            with self.subTest(token=token):
                with self.assertRaisesRegex(VERIFY.Failure, token): self.check(self.archive(files, mode))

    def test_paths_links_and_special_members_refused_before_extraction(self):
        for name, kind in [("../outside", tarfile.REGTYPE), ("link", tarfile.SYMTYPE), ("device", tarfile.CHRTYPE)]:
            path = self.root / "malformed.tar.gz"
            with tarfile.open(path, "w:gz") as stream:
                member = tarfile.TarInfo(name); member.type = kind; member.linkname = "outside"
                stream.addfile(member)
            with self.subTest(name=name), self.assertRaises(VERIFY.Failure): VERIFY.Archive(path)
        self.assertFalse((self.root / "outside").exists())

    def test_source_and_inventory_bindings_refused(self):
        self.expected["source_head"] = "c" * 40
        with self.assertRaisesRegex(VERIFY.Failure, "expectation-source-mismatch"): self.check(self.archive())
        self.expected["source_head"] = self.source.head
        files = dict(self.files); files["licenses/inventory.json"] = b'[{"name":"foreign"}]'
        with self.assertRaisesRegex(VERIFY.Failure, "inventory"): self.check(self.archive(files))

    def test_duplicate_json_refused(self):
        files = dict(self.files);files["build-manifest.json"] = b'{"schema_version":1,"schema_version":2}'
        with self.assertRaisesRegex(VERIFY.Failure, "duplicate-json-member"): self.check(self.archive(files))

    def test_dependency_graph_excludes_development_and_unrelated_gui(self):
        packages = [{"id": n, "name": n} for n in ["drmr", "runtime", "dev", "gui"]]
        metadata = {"packages": packages, "resolve": {"nodes": [
            {"id": "drmr", "deps": [{"pkg": "runtime", "dep_kinds": [{"kind": None}]}, {"pkg": "dev", "dep_kinds": [{"kind": "dev"}]}]},
            *[{"id": n, "deps": []} for n in ["runtime", "dev", "gui"]]]}}
        self.assertEqual({p["name"] for p in VERIFY.dependency_graph(metadata, ("drmr",))}, {"drmr", "runtime"})

    def test_desktop_archive_is_not_successful_without_native_pair_evidence(self):
        self.expected["kind"] = "desktop"
        self.files["drmr-desktop"] = elf()
        self.manifest["binaries"]["drmr-desktop"] = VERIFY.sha(elf())
        result = self.check(self.archive())
        self.assertFalse(result["success"])
        self.assertEqual(result["pair_check"], "not-executed")

    def test_native_header_checks_cover_macos_and_windows(self):
        mac = bytearray(64)
        mac[:4] = b"\xcf\xfa\xed\xfe"
        mac[4:8] = (0x0100000c).to_bytes(4, "little")
        mac[12:16] = (2).to_bytes(4, "little")
        windows = bytearray(154)
        windows[:2] = b"MZ"
        windows[60:64] = (128).to_bytes(4, "little")
        windows[128:132] = b"PE\0\0"
        windows[132:134] = (0x8664).to_bytes(2, "little")
        windows[152:154] = (0x20b).to_bytes(2, "little")
        for data, target in [(mac, "aarch64-apple-darwin"), (windows, "x86_64-pc-windows-msvc")]:
            with self.subTest(target=target):
                archive = VERIFY.Archive(self.archive(dict(self.files, drmr=bytes(data))))
                try:
                    self.assertEqual(VERIFY.binary(archive, "package/drmr", target)["header_target"], target)
                    with self.assertRaises(VERIFY.Failure): VERIFY.binary(archive, "package/drmr", "aarch64-unknown-linux-gnu")
                finally: archive.close()

    def test_zip_symlinks_type_masquerade_and_parent_traversal_are_refused(self):
        for name, mode in [("link", stat.S_IFLNK | 0o777), ("file", stat.S_IFDIR | 0o755), ("../escape", stat.S_IFREG | 0o644)]:
            path = self.root / "unsafe.zip"
            with zipfile.ZipFile(path, "w") as stream:
                info = zipfile.ZipInfo(name)
                info.external_attr = mode << 16
                stream.writestr(info, b"foreign")
            with self.subTest(name=name), self.assertRaises(VERIFY.Failure): VERIFY.Archive(path)
