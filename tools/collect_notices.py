#!/usr/bin/env python3
"""Collect CLI dependency and embedded Web asset notices without enabling the GUI."""

import argparse
import json
import os
from pathlib import Path

from build_desktop import collect_notices, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path, help="Directory for notices and the source inventory")
    parser.add_argument("--target", help="Rust target triple (default: the compiler host)")
    args = parser.parse_args()
    if args.output.exists():
        parser.error("The output directory already exists; choose a new directory")
    env = os.environ.copy()
    target = args.target or next(line.split(": ", 1)[1] for line in run(["rustc", "-vV"], env, True).stdout.splitlines()
                                 if line.startswith("host: "))
    metadata = json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked",
                              "--filter-platform", target], env, True).stdout)
    collect_notices(metadata, args.output, roots=("drmr",))
    print("CLI dependency and Web asset notices collected.")


if __name__ == "__main__":
    main()
