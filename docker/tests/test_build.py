import importlib.util
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("container_build", Path(__file__).parents[1] / "build.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)

class CommittedContext(unittest.TestCase):
    def test_untracked_secret_inside_an_allowed_source_prefix_never_enters_context(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "repo"
            root.mkdir()
            def git(*args):
                subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", *args], cwd=root, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            git("init", "--template=")
            (root / "crates").mkdir()
            (root / "crates/code.rs").write_text("tracked source")
            git("add", ".")
            git("commit", "-m", "fixture")
            (root / "crates/operator-keys.json").write_text("secret canary")
            destination = Path(temp) / "context.tar"
            captured = build.capture_context(root, destination)
            with tarfile.open(destination) as archive:
                self.assertEqual([x.name for x in archive if x.isfile()], ["crates/code.rs"])
            self.assertEqual(len(captured["source_commit"]), 40)
            self.assertEqual(len(captured["source_tree"]), 40)
            (root / "crates/code.rs").write_text("dirty tracked source")
            with self.assertRaisesRegex(ValueError, "Commit tracked changes"):
                build.capture_context(root, Path(temp) / "dirty.tar")

if __name__ == "__main__":
    unittest.main()
