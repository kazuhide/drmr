#!/usr/bin/env python3
"""Pause one processing call, then execute the real media tool unchanged."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

control_path = Path(os.environ["TERMINAL_TEST_CONTROL"])
control = json.loads(control_path.read_text())
if "--version" not in sys.argv[1:]:
    started = subprocess.check_output(
        ["ps", "-p", str(os.getpid()), "-o", "lstart="], text=True).strip()
    receipt = control_path.parent / f"tool-{os.getpid()}.json"
    pending = receipt.with_suffix(".pending")
    pending.write_text(json.dumps({"pid": os.getpid(), "started": started}))
    pending.chmod(0o600)
    pending.replace(receipt)
    try:
        descriptor = os.open(control_path.parent / "delay-claimed", os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError:
        pass
    else:
        os.close(descriptor)
        time.sleep(6)
        if control["outcome"] == "failure":
            raise SystemExit(79)
os.execv(control["packager"], [control["packager"], *sys.argv[1:]])
