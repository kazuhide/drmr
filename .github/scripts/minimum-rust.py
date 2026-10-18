"""Read the declared minimum Rust version for the compatibility CI jobs."""

from pathlib import Path
import re
import tomllib

version = tomllib.loads(Path("Cargo.toml").read_text())["workspace"]["package"]["rust-version"]
if not re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", version):
    raise SystemExit("Declare a numeric minimum Rust version in Cargo.toml")
print(version + ".0" if version.count(".") == 1 else version)
