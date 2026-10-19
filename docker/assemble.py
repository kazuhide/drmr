"""Collect CLI sources/notices after compilation, without enabling Desktop features."""
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
from native import PREFIX, collect_notices, digest, run, verify_file

SOURCE = Path("/source")
# The pinned Rust 1.95 slim image supplies SPDX-named texts, plus the actual
# compiler and standard-library attribution in these two COPYRIGHT documents.
RUST_PRIMARY_LICENSES = {
    "licenses/Apache-2.0.txt": "074e6e32c86a4c0ef8b3ed25b721ca23aca83df277cd88106ef7177c354615ff",
    "licenses/MIT.txt": "b85dcd3e453d05982552c52b5fc9e0bdd6d23c6f8e844b984a88af32570b0cc0",
}


def collect_toolchain(sysroot, destination):
    rust = sysroot / "share/doc/rust"
    for name in ("COPYRIGHT.html", "COPYRIGHT-library.html"):
        path = rust / name
        if not path.is_file() or not path.stat().st_size:
            raise ValueError("rust-toolchain-attribution-missing")
    for relative, expected in RUST_PRIMARY_LICENSES.items():
        verify_file(rust / relative, expected)
    collect_notices(rust, destination / "rust-toolchain", list(RUST_PRIMARY_LICENSES))
    collect_notices(sysroot / "share/doc/cargo", destination / "cargo")


def add_source(output, path, name):
    with tarfile.open(output, "w:gz") as archive:
        def selected(info):
            if any(part in ("target", ".git", "__pycache__") for part in Path(info.name).parts):
                return None
            return info
        archive.add(path, arcname=name, filter=selected)

def main():
    import tomllib
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
    collect_toolchain(sysroot, PREFIX / "licenses")
    add_source(PREFIX / "sources/rust-standard-library.tar.gz", sysroot / "lib/rustlib/src/rust", "rust")
    result = {"rustc": run("rustc", "-vV", capture=True), "target": target, "cargo_registry_sources": records,
              "local_source_archive": "drmr-source.tar.gz", "standard_library_source": "rust-standard-library.tar.gz",
              "toolchain_primary_licenses": RUST_PRIMARY_LICENSES}
    (PREFIX / "metadata/rust.json").write_text(json.dumps(result, indent=2) + "\n")
    shutil.copyfile(Path("/recipe/sources.json"), PREFIX / "sources/source-pins.json")

if __name__ == "__main__":
    main()
