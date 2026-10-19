import importlib.util
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("container_build", Path(__file__).parents[1] / "build.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)

class BuilderBinding(unittest.TestCase):
    def test_named_remote_and_multiple_node_builders_are_refused(self):
        for nodes in [[{"Name": "owned0", "Endpoint": "remote"}],
                      [{"Name": "owned0", "Endpoint": "local"}, {"Name": "owned1", "Endpoint": "local"}]]:
            with self.subTest(nodes=nodes), self.assertRaisesRegex(ValueError, "builder-not-single-local-node"):
                build.validate_builder([{"Name": "owned", "Driver": "docker-container", "Nodes": nodes}],
                                       "owned", "local", "unix:///var/run/docker.sock")

    def test_only_identical_local_bindings_can_be_deduplicated(self):
        row = {"Name": "owned", "Driver": "docker-container", "Nodes": [{"Name": "owned0", "Endpoint": "local"}]}
        self.assertEqual(build.validate_builder([row, dict(row)], "owned", "local", "unix:///var/run/docker.sock")["node"], "owned0")
        remote = {"Name": "owned", "Driver": "docker-container", "Nodes": [{"Name": "owned0", "Endpoint": "remote"}]}
        with self.assertRaisesRegex(ValueError, "builder-binding-ambiguous"):
            build.validate_builder([row, remote], "owned", "local", "unix:///var/run/docker.sock")

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
