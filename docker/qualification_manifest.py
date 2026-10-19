"""Bind test-only observers to the same compiler and CLI used by the runtime."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

revision, tree, architecture = sys.argv[1:]
if not all(re.fullmatch("[0-9a-f]{40}", value) for value in (revision, tree)):
    raise ValueError("qualification-source-identity-invalid")
def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()
manifest = {"source_commit": revision, "source_tree": tree, "target_architecture": architecture,
            "rustc": subprocess.check_output(["rustc", "-vV"], text=True),
            "drmr_sha256": digest(Path("/opt/drmr/bin/drmr")),
            "artifacts": {name: {"path": "out/" + name, "sha256": digest(Path("/out") / name)}
                          for name in ("container-qualification", "drmr-native-fixture", "container-resources")}}
Path("/out/qualification-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
