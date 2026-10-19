import hashlib
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
spec = importlib.util.spec_from_file_location("assemble", Path(__file__).parents[1] / "assemble.py")
assemble = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assemble)


class RustToolchainEvidence(unittest.TestCase):
    def test_slim_toolchain_layout_keeps_attribution_and_exact_license_bytes(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "sysroot"
            rust = root / "share/doc/rust"
            (rust / "licenses").mkdir(parents=True)
            body = b"Apache License\r\nfull terms\r\n"
            (rust / "licenses/Apache-2.0.txt").write_bytes(body)
            for name in ("COPYRIGHT.html", "COPYRIGHT-library.html"):
                (rust / name).write_bytes(b"publisher attribution and component licenses")
            cargo = root / "share/doc/cargo"
            cargo.mkdir()
            (cargo / "LICENSE-THIRD-PARTY").write_bytes(b"Cargo publisher terms")
            expected = {"licenses/Apache-2.0.txt": hashlib.sha256(body).hexdigest()}
            with patch.object(assemble, "RUST_PRIMARY_LICENSES", expected):
                output = Path(temp) / "notices"
                assemble.collect_toolchain(root, output)
                self.assertEqual((output / "rust-toolchain/licenses/Apache-2.0.txt").read_bytes(), body)
                self.assertEqual((output / "rust-toolchain/COPYRIGHT-library.html").read_bytes(), b"publisher attribution and component licenses")
                self.assertEqual((output / "cargo/LICENSE-THIRD-PARTY").read_bytes(), b"Cargo publisher terms")
                (rust / "licenses/Apache-2.0.txt").write_bytes(body.replace(b"\r\n", b"\n"))
                with self.assertRaisesRegex(ValueError, "source-checksum-mismatch"):
                    assemble.collect_toolchain(root, Path(temp) / "changed")
                (rust / "licenses/Apache-2.0.txt").write_bytes(body)
                (rust / "COPYRIGHT-library.html").unlink()
                with self.assertRaisesRegex(ValueError, "rust-toolchain-attribution-missing"):
                    assemble.collect_toolchain(root, Path(temp) / "unattributed")


if __name__ == "__main__":
    unittest.main()
