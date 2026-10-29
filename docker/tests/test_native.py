import hashlib
import http.client
import io
import ssl
import sys
import tarfile
from unittest.mock import call, patch
import importlib.util
from pathlib import Path
import tempfile
import unittest
import urllib.error

spec = importlib.util.spec_from_file_location("native", Path(__file__).parents[1] / "native.py")
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)
ffmpeg_spec = importlib.util.spec_from_file_location("ffmpeg_build", Path(__file__).parents[1] / "ffmpeg.py")
ffmpeg = importlib.util.module_from_spec(ffmpeg_spec)
with patch.dict(sys.modules, {"native": native}):
    ffmpeg_spec.loader.exec_module(ffmpeg)

class InterruptedResponse(io.BytesIO):
    def read(self, size=-1):
        if self.tell():
            raise http.client.IncompleteRead(b"interrupted")
        return super().read(3)


class SourceEvidence(unittest.TestCase):
    def fetch_pin(self, body=b"publisher source"):
        return {"archive": "source.tar.xz", "url": "https://publisher.invalid/source.tar.xz",
                "sha256": hashlib.sha256(body).hexdigest()}

    def test_partial_download_is_discarded_before_retry(self):
        body = b"publisher source"
        with tempfile.TemporaryDirectory() as temp, patch.object(native, "PREFIX", Path(temp)), \
                patch.object(native.urllib.request, "urlopen", side_effect=[InterruptedResponse(body), io.BytesIO(body)]) as fetch, \
                patch("time.sleep") as sleep:
            archive = native.fetch_archive(self.fetch_pin(body))
            self.assertEqual(archive.read_bytes(), body)
            self.assertEqual(list(archive.parent.iterdir()), [archive])
            self.assertEqual(fetch.call_args_list, [call(self.fetch_pin()["url"], timeout=20)] * 2)
            sleep.assert_called_once_with(1)

    def test_transient_connection_and_http_errors_are_retried(self):
        errors = [urllib.error.URLError(TimeoutError("TLS handshake timeout")),
                  ConnectionResetError("connection reset"), ssl.SSLEOFError("broken TLS read")]
        errors += [urllib.error.HTTPError("https://publisher.invalid", code, "temporary", {}, None)
                   for code in (408, 429, 500, 503, 599)]
        for error in errors:
            with self.subTest(error=error), tempfile.TemporaryDirectory() as temp, \
                    patch.object(native, "PREFIX", Path(temp)), patch("time.sleep"), \
                    patch.object(native.urllib.request, "urlopen", side_effect=[error, io.BytesIO(b"publisher source")]) as fetch:
                self.assertEqual(native.fetch_archive(self.fetch_pin()).read_bytes(), b"publisher source")
                self.assertEqual(fetch.call_count, 2)

    def test_retry_exhaustion_removes_only_owned_pending_files(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(native, "PREFIX", Path(temp)), \
                patch.object(native.urllib.request, "urlopen", side_effect=[InterruptedResponse(b"source") for _ in range(3)]) as fetch, \
                patch("time.sleep") as sleep:
            sources = Path(temp) / "sources"
            sources.mkdir()
            retained = sources / "other.pending"
            retained.write_bytes(b"retain")
            with self.assertRaises(http.client.IncompleteRead):
                native.fetch_archive(self.fetch_pin())
            self.assertEqual(fetch.call_count, 3)
            self.assertEqual(sleep.call_args_list, [call(1), call(2)])
            self.assertEqual(list(sources.iterdir()), [retained])
            self.assertEqual(retained.read_bytes(), b"retain")

    def test_permanent_fetch_errors_and_checksum_mismatch_are_not_retried(self):
        errors = [urllib.error.HTTPError("https://publisher.invalid", 404, "missing", {}, None),
                  PermissionError("denied"), ssl.SSLCertVerificationError("untrusted certificate")]
        for error in errors:
            with self.subTest(error=error), tempfile.TemporaryDirectory() as temp, \
                    patch.object(native, "PREFIX", Path(temp)), patch("time.sleep") as sleep, \
                    patch.object(native.urllib.request, "urlopen", side_effect=error) as fetch:
                with self.assertRaises(type(error)):
                    native.fetch_archive(self.fetch_pin())
                self.assertEqual(fetch.call_count, 1)
                sleep.assert_not_called()
                self.assertEqual(list((Path(temp) / "sources").iterdir()), [])
        with tempfile.TemporaryDirectory() as temp, patch.object(native, "PREFIX", Path(temp)), \
                patch("time.sleep") as sleep, \
                patch.object(native.urllib.request, "urlopen", return_value=io.BytesIO(b"modified")) as fetch:
            with self.assertRaisesRegex(ValueError, "source-checksum-mismatch"):
                native.fetch_archive(self.fetch_pin())
            self.assertEqual(fetch.call_count, 1)
            sleep.assert_not_called()
            self.assertEqual(list((Path(temp) / "sources").iterdir()), [])

    def test_existing_archive_is_preserved_including_publication_race(self):
        for race in (False, True):
            with self.subTest(race=race), tempfile.TemporaryDirectory() as temp, \
                    patch.object(native, "PREFIX", Path(temp)), patch("time.sleep") as sleep:
                archive = Path(temp) / "sources/source.tar.xz"
                archive.parent.mkdir()
                def response(*args, **kwargs):
                    archive.write_bytes(b"existing")
                    return io.BytesIO(b"publisher source")
                if not race:
                    archive.write_bytes(b"existing")
                with patch.object(native.urllib.request, "urlopen", side_effect=response) as fetch:
                    with self.assertRaises(FileExistsError):
                        native.fetch_archive(self.fetch_pin())
                self.assertEqual(archive.read_bytes(), b"existing")
                self.assertEqual(list(archive.parent.iterdir()), [archive])
                self.assertEqual(fetch.call_count, int(race))
                sleep.assert_not_called()

    def test_ffmpeg_archive_and_signature_use_the_same_verified_fetch(self):
        pin = self.fetch_pin()
        pin["signature"] = {**self.fetch_pin(b"signature"), "archive": "source.tar.xz.asc"}
        pin["key_sha256"] = "0" * 64
        archive, signature = Path("source.tar.xz"), Path("source.tar.xz.asc")
        with patch.object(ffmpeg, "fetch_archive", side_effect=[archive, signature]) as fetch, \
                patch.object(ffmpeg, "unpack", return_value=Path("source")), \
                patch.object(ffmpeg, "verify_file"), patch.object(ffmpeg, "record"), \
                patch.object(ffmpeg, "run") as run, \
                patch.object(native.urllib.request, "urlopen", side_effect=AssertionError("unverified direct fetch")):
            ffmpeg.ffmpeg(pin, 2)
        self.assertEqual(fetch.call_args_list, [call(pin), call(pin["signature"], read_limit=65536)])
        self.assertIn(call("gpgv", "--keyring", str(ffmpeg.WORK / "ffmpeg-keyring.gpg"),
                           str(signature), str(ffmpeg.PREFIX / "sources" / pin["archive"])), run.call_args_list)

    def test_raw_source_checksum_rejects_line_ending_changes(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "source"
            path.write_bytes(b"publisher\r\n")
            with self.assertRaisesRegex(ValueError, "source-checksum-mismatch"):
                native.verify_file(path, hashlib.sha256(b"publisher\n").hexdigest())

    def test_notice_filename_variants_preserve_raw_bytes(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "source"
            root.mkdir()
            bodies = {"LICENSE.md": b"terms\r\n", "LICENSE.MIT": b"copyright", "LICENSES/Apache-2.0.txt": b"full terms", "AUTHORS": b"publisher"}
            for name, body in bodies.items():
                path = root / name
                path.parent.mkdir(exist_ok=True)
                path.write_bytes(body)
            output = Path(temp) / "notices"
            records = native.collect_notices(root, output)
            self.assertEqual({r["path"] for r in records}, set(bodies))
            for name, body in bodies.items():
                self.assertEqual((output / name).read_bytes(), body)

    def test_unpack_works_without_the_new_tarfile_filter_api(self):
        with tempfile.TemporaryDirectory() as temp:
            archive = Path(temp) / "source.tar"
            with tarfile.open(archive, "w") as target:
                entry = tarfile.TarInfo("release/configure")
                entry.size = 7
                entry.mode = 0o755
                target.addfile(entry, io.BytesIO(b"source!"))
            with patch.object(tarfile.TarFile, "extractall", side_effect=TypeError("legacy tarfile API")):
                source = native.unpack(archive, Path(temp) / "unpacked")
            self.assertEqual((source / "configure").read_bytes(), b"source!")
            self.assertEqual((source / "configure").stat().st_mode & 0o777, 0o755)

    def test_unpack_rejects_links_and_parent_escape_before_writing_them(self):
        for name, kind in [("release/link", tarfile.SYMTYPE), ("../outside", tarfile.REGTYPE), ("/outside", tarfile.REGTYPE), ("C:/outside", tarfile.REGTYPE)]:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp:
                archive = Path(temp) / "source.tar"
                with tarfile.open(archive, "w") as target:
                    entry = tarfile.TarInfo(name)
                    entry.type = kind
                    entry.linkname = "configure" if kind == tarfile.SYMTYPE else ""
                    target.addfile(entry)
                with self.assertRaisesRegex(ValueError, "source-archive-entry-invalid"):
                    native.unpack(archive, Path(temp) / "unpacked")
                self.assertFalse((Path(temp) / "outside").exists())

    def test_pinned_publisher_copyright_is_an_explicit_license(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "source"
            root.mkdir()
            body = b"Copyright publisher\r\nPermission is hereby granted...\r\n"
            (root / "Copyright").write_bytes(body)
            with self.assertRaisesRegex(ValueError, "component-license-missing"):
                native.collect_notices(root, Path(temp) / "default")
            with patch.object(native, "PREFIX", Path(temp) / "prefix"):
                native.record("publisher", {"license_files": ["Copyright"], "notices": [
                    {"path": "Copyright", "sha256": hashlib.sha256(body).hexdigest()}]}, root)
                self.assertEqual((native.PREFIX / "licenses/publisher/Copyright").read_bytes(), body)
                with self.assertRaisesRegex(ValueError, "component-license-pin-missing"):
                    native.record("unbound", {"license_files": ["Copyright"], "notices": []}, root)
                with self.assertRaisesRegex(ValueError, "source-checksum-mismatch"):
                    native.record("changed", {"license_files": ["Copyright"], "notices": [
                        {"path": "Copyright", "sha256": "0" * 64}]}, root)

    def test_component_without_license_fails_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "source"
            root.mkdir()
            (root / "README").write_text("not license evidence")
            with self.assertRaisesRegex(ValueError, "component-license-missing"):
                native.collect_notices(root, Path(temp) / "notices")

if __name__ == "__main__":
    unittest.main()
