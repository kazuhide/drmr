#!/usr/bin/env python3
"""Register each native tool before exec; delay only the controlled packaging call."""

import json
import os
from pathlib import Path
import sys
import time
from process_owner import birth

control_path = Path(os.environ["TERMINAL_TEST_CONTROL"])
control = json.loads(control_path.read_text())
name = Path(sys.argv[0]).name
registry = Path(control["registry"])
receipt = registry / f"tool-{os.getpid()}.json"
pending = receipt.with_suffix(".pending")
with os.fdopen(os.open(pending, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as file:
    json.dump({"pid": os.getpid(), "birth": birth(os.getpid()), "tool": name}, file)
pending.replace(receipt)
if (registry / "closing").exists():
    raise SystemExit(125)
delayed_call = control["outcome"] == "success" or "--enable_raw_key_encryption" in sys.argv[1:]
if name == "packager" and "--version" not in sys.argv[1:] and control["outcome"] != "normal" and delayed_call:
    try:
        descriptor = os.open(control_path.parent / "delay-claimed", os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError:
        pass
    else:
        os.close(descriptor)
        time.sleep(6)
        if control["outcome"] == "failure":
            raise SystemExit(79)
os.environ["TERMINAL_TEST_TOOL"] = name
os.execv(control["tools"][name], [control["tools"][name], *sys.argv[1:]])
