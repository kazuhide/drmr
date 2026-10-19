"""Exercise pinned native Cargo with equal/older archived source timestamps."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from cargo_cache import prepare


def run(arguments, directory):
    return subprocess.check_output(arguments, cwd=directory, stderr=subprocess.STDOUT,
                                   text=True, timeout=30).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    arguments = parser.parse_args()
    toolchain = run(["rustc", "-vV"], None)
    with tempfile.TemporaryDirectory(prefix="drmr-cargo-freshness-") as temporary:
        source = Path(temporary)
        (source / "crates/probe/src").mkdir(parents=True)
        (source / "vendor").mkdir()
        target = source / "target"
        target.mkdir()
        (source / "Cargo.toml").write_text('[workspace]\nmembers=["crates/probe"]\nresolver="3"\n')
        (source / "crates/probe/Cargo.toml").write_text('[package]\nname="cache-probe"\nversion="0.1.0"\nedition="2024"\n')
        path = source / "crates/probe/src/main.rs"
        def program(label):
            path.write_text('fn main(){println!("' + label + '");}\n')
            os.utime(path, (946684800, 946684800))
        def build():
            run(["cargo", "build", "--release", "--locked", "--offline", "--target-dir", str(target)], source)
            return run([str(target / "release/cache-probe")], source)
        program("first")
        run(["cargo", "generate-lockfile", "--offline"], source)
        initial = prepare(source, target, toolchain)
        if build() != "first":
            raise ValueError("cache-control-initial-output-invalid")
        program("other")
        unguarded = build()
        if unguarded != "first":
            raise ValueError("pinned-cargo-stale-control-not-reproduced")
        changed = prepare(source, target, toolchain)
        guarded = build()
        if not changed["target_reset"] or changed["inputs_sha256"] == initial["inputs_sha256"] or guarded != "other":
            raise ValueError("content-cache-fence-failed")
        if prepare(source, target, toolchain)["target_reset"] or build() != "other":
            raise ValueError("unchanged-cache-reuse-failed")
        program("first")
        rollback = prepare(source, target, toolchain)
        reverted = build()
        if not rollback["target_reset"] or reverted != "first":
            raise ValueError("older-source-cache-fence-failed")
        report = {"schema_version": 1, "success": True, "rustc": toolchain,
                  "equal_old_source_mtime": 946684800, "unguarded_stale_output": unguarded,
                  "content_fenced_output": guarded, "older_source_fenced_output": reverted,
                  "unchanged_inputs_reused": True, "single_temporary_target": True}
    arguments.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
