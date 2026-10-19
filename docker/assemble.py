"""Collect CLI sources/notices after compilation, without enabling Desktop features."""
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tomllib
from native import PREFIX, collect_notices, digest, run, verify_file

SOURCE = Path("/source")

def add_source(output, path, name):
    with tarfile.open(output, "w:gz") as archive:
        def selected(info):
            if any(part in ("target", ".git", "__pycache__") for part in Path(info.name).parts):
                return None
            return info
        archive.add(path, arcname=name, filter=selected)

def main():
    target = next(line.split(": ", 1)[1] for line in run("rustc", "-vV", capture=True).splitlines() if line.startswith("host: "))
    metadata = json.loads(run("cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target, cwd=SOURCE, capture=True))
    run("python3", "tools/collect_notices.py", "--output", str(PREFIX / "licenses/rust"), "--target", target, cwd=SOURCE)
    inventory = json.loads((PREFIX / "licenses/rust/inventory.json").read_text())
    selected = {(row["name"], row["version"]) for row in inventory}
    packages = [p for p in metadata["packages"] if (p["name"], p["version"]) in selected]
    locked = tomllib.loads((SOURCE / "Cargo.lock").read_text())
    checksums = {(p["name"], p["version"]): p.get("checksum") for p in locked["package"]}
    output = PREFIX / "sources/rust-crates"
    output.mkdir()
    records = []
    for package in packages:
        if not (package.get("source") or "").startswith("registry+"):
            continue
        name = package["name"] + "-" + package["version"] + ".crate"
        candidates = list(Path("/usr/local/cargo/registry/cache").glob("*/" + name))
        if len(candidates) != 1 or not checksums[(package["name"], package["version"])]:
            raise ValueError("locked-crate-source-missing: " + name)
        verify_file(candidates[0], checksums[(package["name"], package["version"])] )
        shutil.copyfile(candidates[0], output / name)
        records.append({"package": package["name"], "version": package["version"], "file": "rust-crates/" + name, "sha256": digest(output / name)})
    add_source(PREFIX / "sources/drmr-source.tar.gz", SOURCE, "drmr")
    sysroot = Path(run("rustc", "--print", "sysroot", capture=True).strip())
    collect_notices(sysroot / "share/doc/rust", PREFIX / "licenses/rust-toolchain")
    add_source(PREFIX / "sources/rust-standard-library.tar.gz", sysroot / "lib/rustlib/src/rust", "rust")
    result = {"rustc": run("rustc", "-vV", capture=True), "target": target, "cargo_registry_sources": records,
              "local_source_archive": "drmr-source.tar.gz", "standard_library_source": "rust-standard-library.tar.gz"}
    (PREFIX / "metadata/rust.json").write_text(json.dumps(result, indent=2) + "\n")
    shutil.copyfile(Path("/recipe/sources.json"), PREFIX / "sources/source-pins.json")

if __name__ == "__main__":
    main()
