#!/usr/bin/env python3
"""Check skip coverage in a complete Cargo test log for one package scope."""

import argparse
from pathlib import Path
import re


ALLOWED_SKIP_NOTES = (
    r"skipped: ffmpeg [0-6] omits packet timing on this fixture(?: \(.*\))?",
)

ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
IGNORED_MARKER = re.compile(r"\btest .* \.\.\. ignored\b")
SUMMARY = re.compile(
    r"\btest result: (?:ok|FAILED)\. [0-9]+ passed; [0-9]+ failed; "
    r"([0-9]+) ignored; [0-9]+ measured; [0-9]+ filtered out"
    r"(?:; finished in [0-9]+(?:\.[0-9]+)?s)?$"
)


class SkipCoverageError(ValueError):
    """The test log cannot establish the expected scope's skip coverage."""


def check_log(text, scope):
    if scope not in ("core", "workspace"):
        raise SkipCoverageError("Unknown Rust test scope.")
    lines = ANSI.sub("", text).splitlines()
    summaries = 0
    for line in lines:
        for note in re.findall(r"skipped: [^\n]*", line):
            if not any(re.fullmatch(pattern, note) for pattern in ALLOWED_SKIP_NOTES):
                raise SkipCoverageError("Unexpected skipped tests detected.")
        if IGNORED_MARKER.search(line):
            raise SkipCoverageError("Unexpected ignored test detected.")
        if "test result:" in line:
            summary = SUMMARY.search(line)
            if not summary or line.count("test result:") != 1:
                raise SkipCoverageError("Malformed test-result summary detected.")
            if int(summary.group(1)) != 0:
                raise SkipCoverageError("Unexpected ignored-test summary detected.")
            summaries += 1
    if not summaries:
        raise SkipCoverageError("The Rust test log has no complete test-result summary.")
    return summaries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scope", choices=("core", "workspace"), required=True)
    parser.add_argument("--log", type=Path, required=True)
    args = parser.parse_args()
    try:
        text = args.log.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        raise SystemExit("The Rust test log is missing or unreadable; skip coverage cannot be checked.")
    try:
        summaries = check_log(text, args.scope)
    except SkipCoverageError as error:
        raise SystemExit(str(error))
    # Cargo's pipefail exit status independently checks test success, including child oracles.
    print(f"No unexpected test skips; {summaries} complete test-result summaries checked ({args.scope}).")


if __name__ == "__main__":
    main()
