#!/usr/bin/env python3
"""Qualify actual CLI progress under deterministic PTY resizes and real media work."""

import argparse
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

HERE = Path(__file__).resolve().parent
REPOSITORY = HERE.parents[1]
FIXTURE = REPOSITORY / "crates/drmr/tests/fixtures/bbb"
sys.path.insert(0, str(FIXTURE))
from regression import (prepare_source, private_json, require_media_verification,
                        source_fingerprints, verify_fixture)
import regression as media
from process_owner import ProcessOwner, CleanupUnconfirmed


def digest(data):
    return hashlib.sha256(data).hexdigest()


def execute(arguments, environment=None, timeout=30, owner=None, input=None):
    if owner is None:
        raise RuntimeError("process-owner-required")
    return owner.execute(arguments, environment, timeout, input)


def identity(binary, owner):
    result = {"binary_sha256": digest(binary.read_bytes()),
              "fixture_sha256": digest((FIXTURE / "clip.mp4").read_bytes()),
              "harness_sha256": {name: digest((HERE / name).read_bytes()) for name in
                  ["capture.py", "packager.py", "process_owner.py", "replay.mjs", "package.json", "package-lock.json"]}}
    if os.environ.get("CI") == "true":
        head = execute(["git", "rev-parse", "HEAD"], owner=owner).decode().strip()
        if head != os.environ.get("GITHUB_SHA") or execute(["git", "status", "--porcelain", "--untracked-files=all"], owner=owner):
            raise RuntimeError("source-checkout-changed")
        build = Path(os.environ["DRMR_CONSOLE_BUILD"]).read_bytes()
        artifacts = [row for line in build.splitlines() if (row := json.loads(line)).get("reason") == "compiler-artifact"
                     and row["target"]["name"] == "drmr" and "bin" in row["target"]["kind"]
                     and row.get("executable") and not row["profile"]["test"]]
        if len(artifacts) != 1 or Path(artifacts[0]["executable"]).resolve() != binary:
            raise RuntimeError("binary-build-artifact-mismatch")
        result.update(source_head=head, source_tree=execute(["git", "rev-parse", "HEAD^{tree}"], owner=owner).decode().strip(),
                      build_sha256=digest(build))
    return result


def capture(owner, binary, source, keys, outputs, directory, environment, outcome, no_color, tty_input):
    workers = 2 if outcome == "success" else 1
    output = directory / "output"
    plan = directory / "plan.json"
    planning_environment = owner.environment(environment, directory / "planning")
    execute([binary, "plan", source, "--outputs", outputs, "--output", output, "--save", plan,
             "--json", "--progress", "off"], planning_environment, owner=owner)
    plan_before = plan.read_bytes()
    conversion_environment = owner.environment(environment, directory / "conversion", outcome)
    conversion_environment["TERM"] = "xterm-256color"
    if no_color:
        conversion_environment["NO_COLOR"] = "1"
    else:
        conversion_environment.pop("NO_COLOR", None)
    master, slave = pty.openpty()
    attributes = termios.tcgetattr(slave)
    attributes[1] &= ~termios.OPOST
    attributes[3] &= ~termios.ECHO
    termios.tcsetattr(slave, termios.TCSANOW, attributes)
    baseline_attributes = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 120, 0, 0))
    sentinel = b"queued-terminal-input\n"
    pipe = None
    if tty_input:
        os.write(master, sentinel)
        stdin = slave
    else:
        pipe, writer = os.pipe()
        os.write(writer, sentinel)
        os.close(writer)
        stdin = pipe
    process = None
    raw = bytearray()
    events = []
    schedule = [(0.5, 80, 12), (1.5, 40, 8), (2.5, 300, 24)]
    resized = False
    cancelled = False
    split_time = None
    started = time.monotonic()
    try:
        with tempfile.TemporaryFile() as stdout:
            process = owner.spawn([str(binary), "convert", "--plan", str(plan),
                "--keys-file", str(keys), "--max-workers", str(workers), "--json",
                "--progress", "text", "--packager-bin", str(owner.proxies / "packager")], env=conversion_environment,
                stdin=stdin, stdout=stdout, stderr=slave)

            def resize(columns, rows):
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
                events.append({"kind": "resize", "offset": len(raw), "columns": columns, "rows": rows,
                               "elapsed_seconds": time.monotonic() - started})

            while True:
                elapsed = time.monotonic() - started
                if elapsed > 40:
                    raise RuntimeError("terminal-process-deadline")
                while schedule and elapsed >= schedule[0][0]:
                    _, columns, rows = schedule.pop(0)
                    resize(columns, rows)
                if outcome == "cancel" and resized and not cancelled and elapsed >= split_time + 0.15:
                    process.send_signal(signal.SIGINT)
                    cancelled = True
                readable, _, _ = select.select([master], [], [], 0.025)
                if not readable:
                    if process.poll() is not None:
                        break
                    continue
                data = os.read(master, 64)
                if not data:
                    break
                old_size = len(raw)
                raw.extend(data)
                events.append({"kind": "write", "offset": old_size, "length": len(data),
                               "elapsed_seconds": time.monotonic() - started})
                if len(raw) > 262144:
                    raise RuntimeError("terminal-byte-limit")
                # Resize after a partial header has been consumed, while the old-width paint continues.
                if elapsed >= 3.3 and not schedule and not resized and b"drmr convert" in raw[max(0, old_size - 12):]:
                    resize(120, 24)
                    resized = True
                    split_time = time.monotonic() - started
            process.wait(timeout=5)
            stdout.seek(0)
            result_bytes = stdout.read(4194305)
            if len(result_bytes) > 4194304:
                raise RuntimeError("result-byte-limit")
        os.set_blocking(stdin, False)
        remaining = os.read(stdin, 4096)
        input_unchanged = remaining == sentinel and termios.tcgetattr(slave) == baseline_attributes
        result = json.loads(result_bytes)
        expected_exit = {"success": 0, "cancel": 130, "failure": 1}[outcome]
        processing = {"json_result_valid": isinstance(result, dict), "expected_exit": process.returncode == expected_exit,
            "input_untouched": input_unchanged, "controlled_mid_draw_resize": resized,
            "no_partial_publication": outcome == "success" or not output.exists(), "plan_unchanged": plan.read_bytes() == plan_before}
        if outcome == "success":
            processing["bundle_result_valid"] = (result.get("kind") == "bundle-conversion"
                and {row["id"] for row in result.get("outputs", [])} == {"dash", "hls"})
            verification_environment = owner.environment(environment, directory / "verification")
            verified = json.loads(execute([binary, "verify", "--decode", "--plan", plan,
                                         "--keys-file", keys, "--max-workers", "2", "--json"], verification_environment, owner=owner))
            for row in verified["outputs"]:
                require_media_verification(row["verification"])
            processing["native_media_verification"] = (verified["passed"] is True and verified["source_unchanged"] is True
                and {row["id"] for row in verified["outputs"]} == {"dash", "hls"}
                and all(row["output_matches_plan"] is True for row in verified["outputs"]))
        else:
            status = "cancelled" if outcome == "cancel" else "failed"
            processing["structured_failure"] = result.get("status") == status and isinstance(result.get("error"), dict)
            if outcome == "cancel":
                processing["cancelled_result"] = result.get("error", {}).get("code") == "cancelled"
        key = json.loads(keys.read_text())["outputs"]["dash"]["key"].encode()
        processing["sensitive_values_absent"] = key not in raw and key not in result_bytes and str(directory).encode() not in raw
        replay = execute(["node", str(HERE / "replay.mjs")], input=json.dumps({"columns": 120, "rows": 24,
            "outcome": outcome, "no_color": no_color, "require_mid_draw": True,
            "error_code": result.get("error", {}).get("code"),
            "stop_phase": "outputs",
            "raw": base64.b64encode(raw).decode(), "events": events}).encode(), owner=owner, timeout=20)
        screen = json.loads(replay)
        processing["owned_tools_reaped"] = owner.idle()
        return {"passed": screen["passed"] and all(processing.values()), "outcome": outcome,
                "no_color": no_color, "tty_input": tty_input, "processing": processing,
                "workers": workers, "delay_phase": "source" if outcome == "success" else "cenc-output",
                "conversion_decode_requested": False, "independent_verification_decode_requested": outcome == "success",
                "stdout_sha256": digest(result_bytes), "terminal": screen}
    finally:
        try:
            if not owner.idle() and not owner.close():
                raise CleanupUnconfirmed("terminal-cleanup-unconfirmed")
        finally:
            for descriptor in [master, slave, pipe]:
                if descriptor is not None:
                    os.close(descriptor)


def plain_fallback(owner, binary, directory, environment):
    environment = owner.environment(environment, directory)
    environment["TERM"] = "linux"
    master, slave = pty.openpty()
    process = None
    raw = bytearray()
    try:
        with tempfile.TemporaryFile() as stdout:
            process = owner.spawn([binary, "analyze", FIXTURE / "clip.mp4", "--metadata-only", "--json", "--progress", "text"],
                env=environment, stdin=subprocess.DEVNULL, stdout=stdout, stderr=slave)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                readable, _, _ = select.select([master], [], [], 0.025)
                if readable:
                    raw.extend(os.read(master, 4096))
                    if len(raw) > 16384:
                        raise RuntimeError("fallback-byte-limit")
                elif process.poll() is not None:
                    break
            else:
                raise TimeoutError("fallback-deadline")
            process.wait(timeout=1)
            stdout.seek(0)
            result_bytes = stdout.read(1048576)
        result = json.loads(result_bytes)
        assertions = {"expected_exit": process.returncode == 0, "no_alternate_screen": b"\x1b[?1049" not in raw,
            "no_owned_clear": b"\x1b[2J" not in raw, "plain_progress": b"Finished analyze" in raw,
            "json_metadata_result": result.get("format") == "mp4"}
        return {"passed": all(assertions.values()), "assertions": assertions,
                "stdout_sha256": digest(result_bytes), "stderr_sha256": digest(raw)}
    finally:
        try:
            if not owner.idle() and not owner.close():
                raise CleanupUnconfirmed("fallback-cleanup-unconfirmed")
        finally:
            os.close(master)
            os.close(slave)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=os.environ.get("DRMR_CONSOLE_BINARY"))
    parser.add_argument("--evidence", type=Path, default=os.environ.get("TERMINAL_REPORT_ROOT"))
    arguments = parser.parse_args()
    if not arguments.binary or not arguments.evidence:
        parser.error("--binary and --evidence (or their CI environment variables) are required")
    evidence = arguments.evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    report = {"schema_version": 1, "engine": "@xterm/headless@6.0.0", "cases": [], "passed": False,
              "platform": {"system": platform.system(), "machine": platform.machine(), "python": platform.python_version()}}
    root = Path(tempfile.mkdtemp(prefix="drmr-terminal-"))
    owner = None
    previous_run = media.run
    try:
        binary = arguments.binary.resolve()
        owner = ProcessOwner(root)
        report.update(identity(binary, owner))
        environment = {key: value for key, value in os.environ.items() if not key.startswith(("DRM_", "DRMR_"))}
        fixture_environment = owner.environment(environment, root / "fixture-tools")
        # Existing fixture checks keep their assertions; every subprocess they
        # request uses the same bounded owner and registered real-tool proxies.
        media.run = lambda arguments, env=None: execute(arguments, env or fixture_environment, owner=owner).decode()
        verify_fixture()
        source = prepare_source(root / "source")
        before = source_fingerprints(source.parent)
        outputs = private_json(root / "outputs.json", {"schema_version": 2, "outputs": [
            {"id": "dash", "format": "dash", "target": "cenc", "drms": [], "processing": "stitch"},
            {"id": "hls", "format": "hls", "target": "clear", "drms": [], "processing": "stitch"}]})
        keys = private_json(root / "keys.json", {"schema_version": 2, "outputs": {"dash": {
            "kid": "000102030405060708090a0b0c0d0e0f", "key": "10101010101010101010101010101010"}}})
        for name, outcome, no_color, tty_input in [("success", "success", False, True),
            ("no-color", "success", True, False), ("cancel", "cancel", False, False),
            ("failure", "failure", False, False)]:
            directory = root / name
            directory.mkdir(mode=0o700)
            try:
                result = capture(owner, binary, source, keys, outputs, directory, environment, outcome, no_color, tty_input)
            except Exception as error:
                result = {"passed": False, "error": "terminal-case-failed",
                          "error_sha256": digest((type(error).__name__ + str(error)).encode())}
            report["cases"].append({"name": name, **result})
            (evidence / "report.json").write_text(json.dumps(report, indent=2) + "\n")
            if owner.closing:
                break
        report["plain_fallback"] = plain_fallback(owner, binary, root / "plain-fallback", environment)
        report["source_unchanged"] = before == source_fingerprints(source.parent)
        report["binary_unchanged"] = report["binary_sha256"] == digest(binary.read_bytes())
        report["fixture_unchanged"] = report["fixture_sha256"] == digest((FIXTURE / "clip.mp4").read_bytes())
        report["tracked_tools"] = {name: sum(row["tool"] == name for row in owner.receipts())
                                   for name in ("ffmpeg", "ffprobe", "packager")}
    except Exception as error:
        report.update(error="terminal-qualification-failed", error_sha256=digest((type(error).__name__ + str(error)).encode()))
    finally:
        media.run = previous_run
        report["owned_temporary_removed"] = owner.remove_temporary() if owner else False
        report["retained_owned_temporary"] = not report["owned_temporary_removed"]
        if report["retained_owned_temporary"]:
            report["retained_temporary_sha256"] = digest(str(root).encode())
    report["passed"] = (len(report["cases"]) == 4 and all(row["passed"] for row in report["cases"])
        and all(report.get(name) is True for name in ("source_unchanged", "binary_unchanged", "fixture_unchanged", "owned_temporary_removed"))
        and report.get("plain_fallback", {}).get("passed") is True and all(report.get("tracked_tools", {}).values())
        and "error" not in report)
    (evidence / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"Terminal progress: {sum(row['passed'] for row in report['cases'])}/4 cases passed")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
