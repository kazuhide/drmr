"""Diagnostic-only real PowerShell 5.1 identity-transport RED/GREEN probe."""

import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    checkout = Path(__file__).resolve().parents[2]
    relative = ".github/scripts/windows-cli-archive-cleanup.ps1"
    baseline = subprocess.run(["git", "show", "3e7ecf4cfef23fd78f7b88d379b784d4f5686c17:" + relative],
                              cwd=checkout, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=20)
    if baseline.returncode or not 1 <= len(baseline.stdout) <= 32768:
        return 2
    current = (checkout / relative).read_bytes()
    expected = baseline.stdout.decode("utf-8").replace('"st_file_attributes"', "'st_file_attributes'").replace('\":\"', "':'")
    if current.decode("utf-8").replace("\r\n", "\n") != expected:
        return 6
    spec = importlib.util.spec_from_file_location("identity_research", checkout / ".github/scripts/windows_archive_acl_research.py")
    research = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(research)
    rows = []
    driver = r"""
$ErrorActionPreference = 'Stop'
. $env:DRMR_ACL_PROBE_HELPER -Python $env:DRMR_ACL_PROBE_PYTHON
Get-DrmrArchiveDirectoryIdentity $env:DRMR_ACL_PROBE_ROOT $env:DRMR_ACL_PROBE_PYTHON | Out-Null
"""
    encoded = base64.b64encode(driver.encode("utf-16le")).decode("ascii")
    with tempfile.TemporaryDirectory(prefix="drmr-archive-identity-", dir=os.environ["RUNNER_TEMP"]) as temporary:
        root = Path(temporary)
        old = root / "baseline.ps1"
        old.write_bytes(baseline.stdout)
        for stage, script in (("identity-legacy", old), ("identity-current", checkout / relative)):
            environment = dict(os.environ, DRMR_ACL_PROBE_HELPER=str(script), DRMR_ACL_PROBE_ROOT=str(root),
                               DRMR_ACL_PROBE_PYTHON=sys.executable)
            research.capture(subprocess.run, rows.append, stage,
                             ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             env=environment, timeout=20)
        valid = (len(rows) == 2 and rows[0]["status"] == "nonzero" and
                 "OwnedIdentityUnavailable" in rows[0]["categories"] and rows[1]["status"] == "ok" and
                 all(row["stderr_state"] == "complete" for row in rows))
    if Path(temporary).exists():
        return 3
    report = {"event": "powershell-identity-transport", "schema_version": 1, "success": valid,
              "baseline_source_sha256": hashlib.sha256(baseline.stdout).hexdigest(),
              "current_source_sha256": hashlib.sha256((checkout / relative).read_bytes()).hexdigest(),
              "quote_substitution_only": True, "owned_cleanup_confirmed": True, "calls": rows}
    print("DRMR_IDENTITY_TRANSPORT " + json.dumps(report, separators=(",", ":")))
    return 0 if valid else 4


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        print("DRMR_IDENTITY_TRANSPORT_UNCLASSIFIED_FAILURE")
        sys.exit(5)
