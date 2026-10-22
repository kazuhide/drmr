"""Run the unchanged private ACL helper against owned Modify-only fixtures."""

import importlib.util
import json
import pathlib
import sys

sys.dont_write_bytecode = True


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def main():
    root, helper, research_path = map(pathlib.Path, sys.argv[1:])
    helper_module = load("qualify_cli_archive", helper)
    research = load("windows_archive_acl_research", research_path)
    original_run = helper_module.subprocess.run
    rows = []
    stage = None

    def observe(command, *args, **kwargs):
        if not isinstance(command, (tuple, list)) or not command or command[0] != "powershell.exe":
            return original_run(command, *args, **kwargs)
        return research.capture(original_run, rows.append, stage, command, *args, **kwargs)

    helper_module.subprocess.run = observe
    results = []
    for stage, name, directory in (("report-file", "qualification.json", False),
                                   ("temp-directory", "temp", True)):
        try:
            helper_module.secure_windows(root / name, directory)
        except helper_module.Refusal:
            results.append({"stage": stage, "result": "refused"})
        else:
            results.append({"stage": stage, "result": "secured"})
    record = {"schema_version": 1, "results": results, "calls": rows}
    payload = json.dumps(record, separators=(",", ":")).encode("utf-8")
    if len(payload) > 8192:
        return 2
    (root / "research.json").write_bytes(payload)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        # Diagnostic code must not turn a Python traceback into public metadata.
        sys.exit(3)
