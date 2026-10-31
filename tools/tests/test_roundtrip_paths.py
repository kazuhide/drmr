"""Exercise release scratch paths with real child creation and owned cleanup."""
import hashlib
from pathlib import Path
import sys
import tempfile
import threading
import unittest

TOOLS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS))
import release_roundtrip as ROUNDTRIP


def windows_length(path):
    return len(str(path).encode("utf-16-le")) // 2


def temporary_path_regression(parent, legacy_lengths=None):
    """Native callers use the two old child-TEMP lengths, never operator paths."""
    parent = parent.resolve()
    before = set(parent.iterdir())
    legacy_suffix = "\\drmr-cli-archive-xxxxxxxx\\drmr-cli-archive-xxxxxxxx\\forward\\temporary"
    minimum = windows_length(parent) + len(legacy_suffix) + 2
    strict = legacy_lengths is not None
    lengths = legacy_lengths or (max(122, minimum), max(122, minimum) + 14)
    summaries = []
    child = """import hashlib,json,os,pathlib,sys
sys.path.insert(0,sys.argv[1])
import qualify_cli_archive as support
temporary=pathlib.Path(os.environ['TEMP'])
assert os.environ['TMP']==os.environ['TMPDIR']==str(temporary)
workspace=support.Workspace(temporary)
try:
    units=len(str(workspace.path).encode('utf-16-le'))//2
    count=143 if sys.argv[2]=='strict' or os.name!='nt' else min(143,258-units-1)
    assert count>0
    path=workspace.path/('p'*count)
    length=len(str(path).encode('utf-16-le'))//2
    assert os.name!='nt' or length<260
    support.private_write(path,b'private path probe')
    assert path.read_bytes()==b'private path probe'
    result={'created':True,'path_length':length,'sha256':hashlib.sha256(path.read_bytes()).hexdigest()}
finally:
    workspace.close()
assert not workspace.path.exists()
result['cleanup_confirmed']=True
print(json.dumps(result))
"""
    for legacy in lengths:
        padding = legacy - windows_length(parent) - len(legacy_suffix)
        if padding < 2:
            raise AssertionError("The native regression root is too deep")
        selected = parent / ("p" * (padding - 1))
        selected.mkdir(mode=0o700)
        workspace = None
        observed = []
        try:
            workspace = ROUNDTRIP.SUPPORT.Workspace(selected)
            original = workspace.path / "source"
            original.mkdir(mode=0o700)
            source = original / "asset.mp4"
            ROUNDTRIP.SUPPORT.private_write(source, b"original")

            class Executor:
                cancel = threading.Event()

                def runner(self, root):
                    return ROUNDTRIP.SUPPORT.Runner(root)

                def probe(self, runner):
                    length = windows_length(runner.environment["TEMP"])
                    if length > legacy - 40:
                        raise AssertionError("Roundtrip child TEMP retained the deep helper layout")
                    value = runner.json([sys.executable, "-B", "-c", child, TOOLS,
                                         "strict" if strict else "portable"], "temporary-path-probe", 30)
                    if (value.get("created") is not True or value.get("cleanup_confirmed") is not True
                            or value.get("sha256") != hashlib.sha256(b"private path probe").hexdigest()):
                        raise AssertionError("The child did not prove private creation and cleanup")
                    observed.append((length, value["path_length"]))

                def convert(self, _source, _profile, _layout, root):
                    root.mkdir(mode=0o700)
                    self.probe(self.runner(root))
                    output = root / "asset.mp4"
                    ROUNDTRIP.SUPPORT.private_write(output, b"converted")
                    return output, {}

                def key_file(self, root, _profile):
                    path = root / "keys.json"
                    ROUNDTRIP.SUPPORT.private_write(path, b"{}")
                    return path

                def verify(self, runner, *_arguments):
                    self.probe(runner)
                    return {}

            result = ROUNDTRIP.execute_case(ROUNDTRIP.CONTRACT.cases()[0], Executor(),
                                            {("video", ("mp4", "clear")): source}, workspace.path)
            if result["status"] != "complete" or len(observed) != 3:
                raise AssertionError("The scratch regression did not exercise all three operations")
            if set(workspace.path.iterdir()) != {original} or source.read_bytes() != b"original":
                raise AssertionError("Operation cleanup changed the source or retained scratch files")
        finally:
            if workspace is not None:
                workspace.close()
            selected.rmdir()
        summaries.append({"legacy_temp_length": legacy, "maximum_temp_length": max(v[0] for v in observed),
                          "maximum_created_path_length": max(v[1] for v in observed), "child_checks": 3})
    if set(parent.iterdir()) != before:
        raise AssertionError("The regression changed unrelated parent entries")
    return summaries


class RoundtripPaths(unittest.TestCase):
    def test_real_children_create_private_files_and_release_all_operation_scratch(self):
        with tempfile.TemporaryDirectory() as directory:
            result = temporary_path_regression(Path(directory))
            self.assertEqual(len(result), 2)
            self.assertEqual(sum(row["child_checks"] for row in result), 6)
            for row in result:
                self.assertLessEqual(row["maximum_temp_length"], row["legacy_temp_length"] - 40)


if __name__ == "__main__":
    unittest.main()
