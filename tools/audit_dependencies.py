#!/usr/bin/env python3
"""Audit selected CLI/Desktop target graphs without discarding known findings.

Local source patches are verified separately. A second, explicitly identified
lock input restores their publisher identities for RustSec advisory discovery.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

from verify_archive import Failure as ArchiveFailure, Source, dependency_graph, metadata, target_info, unique
from verify_backports import Failure as BackportFailure, verify as verify_backport
from verify_backports import verify_private_fs

ROOT = Path(__file__).resolve().parents[1]


class Failure(Exception):
    pass


def require(condition, token):
    if not condition:
        raise Failure(token)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_json(data):
    return json.loads(data, object_pairs_hook=unique)


def scoped_findings(report, packages):
    require(type(report) is dict and type(report.get("vulnerabilities")) is dict and type(report.get("warnings")) is dict, "audit-schema")
    vulnerabilities = report["vulnerabilities"].get("list")
    require(type(vulnerabilities) is list, "audit-vulnerabilities")
    selected = {(p["name"], p["version"]) for p in packages}
    groups = [("vulnerability", vulnerabilities)] + list(report["warnings"].items())
    found, seen = [], set()
    for classification, rows in groups:
        require(classification in ("vulnerability", "unsound", "unmaintained", "notice", "yanked") and type(rows) is list, "audit-warning-schema")
        for row in rows:
            require(type(row) is dict and type(row.get("package")) is dict and type(row.get("advisory")) is dict, "audit-finding-schema")
            package, advisory = row["package"], row["advisory"]
            require(all(type(package.get(k)) is str for k in ("name", "version")) and type(advisory.get("id")) is str, "audit-finding-identity")
            identity = (package["name"], package["version"])
            if identity not in selected:
                continue
            key = (classification, *identity, advisory["id"])
            require(key not in seen, "audit-duplicate-finding")
            seen.add(key)
            found.append({"name": identity[0], "version": identity[1], "advisory": advisory["id"],
                          "classification": classification, "title": advisory.get("title", ""),
                          "finding_sha256": digest(json.dumps({key: value for key, value in row.items() if key != "package"}, sort_keys=True, separators=(",", ":")).encode())})
    return sorted(found, key=lambda row: (row["name"], row["advisory"], row["classification"]))


def source_identity(metadata_value, package):
    source = package.get("source")
    if source is not None:
        require(type(source) is str and bool(source), "audit-package-source")
        return {"source": source}
    root, manifest = metadata_value.get("workspace_root"), package.get("manifest_path")
    require(type(root) is str and type(manifest) is str, "audit-path-source")
    try:
        relative = Path(manifest).resolve().relative_to(Path(root).resolve()).as_posix()
    except ValueError:
        raise Failure("audit-path-source-outside-workspace") from None
    return {"workspace_manifest": relative}


def ancestry(metadata_value, packages, name, version):
    selected = {p["id"]: p for p in packages}
    labels = {key: p["name"] + "@" + p["version"] for key, p in selected.items()}
    require(len(set(labels.values())) == len(labels), "audit-ambiguous-package")
    nodes = {node["id"]: node for node in metadata_value["resolve"]["nodes"]}
    affected = {key for key, p in selected.items() if (p["name"], p["version"]) == (name, version)}
    require(len(affected) == 1, "audit-affected-package")
    edges = []
    for parent in selected:
        for edge in nodes[parent]["deps"]:
            kinds = sorted({kind["kind"] or "normal" for kind in edge["dep_kinds"] if kind["kind"] != "dev"})
            if kinds and edge["pkg"] in selected:
                edges.append((parent, edge["pkg"], kinds))
    ancestors = set(affected)
    while True:
        expanded = ancestors | {parent for parent, child, _ in edges if child in ancestors}
        if expanded == ancestors:
            break
        ancestors = expanded
    rows = sorted([[labels[parent], labels[child], kinds] for parent, child, kinds in edges if parent in ancestors and child in ancestors])
    direct_ids = {parent for parent, child, _ in edges if child in affected}
    direct = sorted(labels[parent] for parent in direct_ids)
    roles = sorted([[labels[key], sorted({kind for target in selected[key]["targets"] for kind in target["kind"]}), sorted(nodes[key]["features"]), source_identity(metadata_value, selected[key])] for key in ancestors])
    fingerprint = {"edges": rows, "target_kinds": roles}
    build_time_only = bool(direct_ids) and all(any("proc-macro" in target["kind"] for target in selected[key]["targets"]) for key in direct_ids)
    return {"edges": rows, "target_kinds": roles, "sha256": digest(json.dumps(fingerprint, separators=(",", ":")).encode()),
            "direct_parents": direct, "build_time_only": build_time_only}


def normalized_lock(original, pin):
    blocks = original.decode("utf-8").split("[[package]]")
    matched = 0
    inserted = '\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "' + pin["source_sha256"] + '"'
    for index, block in enumerate(blocks):
        if re.search(r'^name = "glib"$', block, re.MULTILINE):
            require('version = "0.18.5"' in block and not re.search(r'^(source|checksum) =', block, re.MULTILINE), "audit-backport-lock-shape")
            changed = block.replace('version = "0.18.5"', 'version = "0.18.5"' + inserted, 1)
            require(changed.replace(inserted, "", 1) == block, "audit-lock-normalization")
            blocks[index] = changed
            matched += 1
    require(matched == 1, "audit-backport-lock-entry")
    return "[[package]]".join(blocks).encode()


def normalized_private_fs_lock(original, pin):
    require(pin.get("package") == "private-fs" and pin.get("version") == "0.2.1"
            and type(pin.get("source_sha256")) is str
            and re.fullmatch(r"[0-9a-f]{64}", pin.get("source_sha256", "")) is not None,
            "audit-native-patch-pin")
    blocks = original.decode("utf-8").split("[[package]]")
    matched = 0
    inserted = '\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "' + pin["source_sha256"] + '"'
    for index, block in enumerate(blocks):
        if re.search(r'^name = "private-fs"$', block, re.MULTILINE):
            require('version = "0.2.1"' in block and not re.search(r'^(source|checksum) =', block, re.MULTILINE),
                    "audit-native-patch-lock-shape")
            changed = block.replace('version = "0.2.1"', 'version = "0.2.1"' + inserted, 1)
            require(changed.replace(inserted, "", 1) == block, "audit-lock-normalization")
            blocks[index] = changed
            matched += 1
    require(matched == 1, "audit-native-patch-lock-entry")
    return "[[package]]".join(blocks).encode()


def apply_policy(rows, kind, target, policy, backport):
    accepted = []
    for row in rows:
        entries = [entry for entry in policy["dispositions"] if all(entry[key] == value for key, value in {
            "kind": kind, "target": target, "name": row["name"], "version": row["version"],
            "advisory": row["advisory"], "classification": row["classification"]}.items())]
        disposition = "unresolved"
        if len(entries) == 1:
            entry = entries[0]
            if entry["finding_sha256"] == row["finding_sha256"] and entry["ancestry_sha256"] == row["dependency_ancestry"]["sha256"] and entry["direct_parents"] == row["dependency_ancestry"]["direct_parents"]:
                if entry["disposition"] == "source-mitigated" and row["advisory"] == backport["advisory"]:
                    disposition = "source-mitigated-pending-native-regression"
                elif entry["disposition"] == "acknowledged-maintenance" and row["classification"] == "unmaintained" and row["dependency_ancestry"]["build_time_only"]:
                    disposition = "acknowledged-maintenance"
        accepted.append(dict(row, disposition=disposition))
    return accepted


def native_regression(source, output):
    compiler = subprocess.run(["rustc", "-vV"], capture_output=True, check=True).stdout.decode()
    target = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    require(target == "x86_64-unknown-linux-gnu", "mitigation-requires-native-linux-regression")
    manifest = source.path / "tools/regressions/glib-variant-iter/Cargo.toml"
    result = subprocess.run(["cargo", "test", "--manifest-path", str(manifest), "--release", "--locked", "--offline",
                             "--test", "variant_iter", "--", "--exact", "optimized_variant_string_iterators_return_the_original_strings"],
                            cwd=source.path, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=300)
    (output / "native-optimized-regression.log").write_bytes(result.stdout)
    require(result.returncode == 0 and b"1 passed; 0 failed; 0 ignored" in result.stdout, "native-optimized-regression-failed")
    return {"target": target, "compiler": compiler, "optimized": True,
            "test_source_sha256": digest(source.read("tools/regressions/glib-variant-iter/tests/variant_iter.rs"))}


def audit_input(program, database, source, lock, destination):
    with tempfile.TemporaryDirectory(prefix="drmr-advisory-") as directory:
        path = Path(directory) / "Cargo.lock"
        path.write_bytes(lock)
        result = subprocess.run([str(program), "audit", "--file", str(path), "--db", str(database),
                                 "--no-fetch", "--deny", "warnings", "--json"], cwd=source.path,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180)
    require(result.returncode in (0, 1) and result.stdout, "audit-command-failed")
    parsed = read_json(result.stdout)
    destination.write_bytes(result.stdout)
    return parsed


def run(args):
    source = Source(args.source)
    require(not args.output.exists(), "audit-output-exists")
    require(subprocess.run(["git", "-C", str(args.database), "remote", "get-url", "origin"], capture_output=True, check=True).stdout.decode().strip().removesuffix(".git") == "https://github.com/RustSec/advisory-db", "audit-database-origin")
    revision = subprocess.run(["git", "-C", str(args.database), "rev-parse", "HEAD"], capture_output=True, check=True).stdout.decode().strip()
    require(re.fullmatch(r"[0-9a-f]{40}", revision), "audit-database-revision")
    require(not subprocess.run(["git", "-C", str(args.database), "status", "--porcelain", "--untracked-files=all"], capture_output=True, check=True).stdout.strip(), "audit-database-dirty")
    backport = verify_backport(source.path)
    native_patch = verify_private_fs(source.path)
    pin = read_json(source.read("tools/backports/glib-0.18.5.json"))
    native_pin = read_json(source.read("tools/backports/private-fs-0.2.1.json"))
    original = source.read("Cargo.lock")
    normalized = normalized_private_fs_lock(normalized_lock(original, pin), native_pin)
    policy = read_json(source.read("tools/advisory-policy.json"))
    require(policy["schema_version"] == 1, "audit-policy-schema")
    args.output.mkdir()
    actual = audit_input(args.cargo_audit, args.database, source, original, args.output / "actual-lock-audit.json")
    discovery = audit_input(args.cargo_audit, args.database, source, normalized, args.output / "registry-identity-audit.json")
    (args.output / "registry-identity-Cargo.lock").write_bytes(normalized)
    regression = native_regression(source, args.output) if args.native_regression else None
    scopes, failed = [], False
    for target in args.targets:
        target_info(target)
        for kind in ("cli", "desktop"):
            meta = metadata(source, kind, target)
            packages = dependency_graph(meta, ("drmr", "drmr-desktop") if kind == "desktop" else ("drmr",))
            rows = scoped_findings(discovery, packages)
            actual_rows = scoped_findings(actual, packages)
            # Preserve all actual-lock findings as well, even if discovery differs.
            for row in actual_rows:
                if row not in rows:
                    rows.append(row)
            for row in rows:
                row["dependency_ancestry"] = ancestry(meta, packages, row["name"], row["version"])
            rows = apply_policy(rows, kind, target, policy, backport)
            if regression:
                for row in rows:
                    if row["disposition"] == "source-mitigated-pending-native-regression":
                        row["disposition"] = "source-mitigated"
            failed |= any(row["disposition"] in ("unresolved", "source-mitigated-pending-native-regression") for row in rows)
            scopes.append({"kind": kind, "target": target, "packages": len(packages), "findings": rows})
    report = {"schema_version": 1, "success": not failed, "source_head": source.head, "source_tree": source.tree,
              "database_revision": revision, "actual_lock_sha256": digest(original),
              "registry_identity_lock_sha256": digest(normalized),
              "normalization": "Only the reviewed path-vendored glib 0.18.5 and private-fs 0.2.1 entries receive their official registry sources and checksums for advisory discovery. Both raw audit results are retained.",
              "backport": backport, "native_handle_patch": native_patch, "scopes": scopes,
              "native_regression": regression}
    (args.output / "summary.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    for scope in scopes:
        print(scope["kind"], scope["target"], json.dumps([{key: row[key] for key in ("advisory", "classification", "disposition")} for row in scope["findings"]]))
    return 1 if failed else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--cargo-audit", type=Path, default=Path("cargo-audit"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--native-regression", action="store_true", help="Run the mandatory real optimized GLib regression on native Linux before accepting its mitigation")
    parser.add_argument("--targets", nargs="+", default=["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"])
    args = parser.parse_args()
    try:
        return run(args)
    except (Failure, ArchiveFailure, BackportFailure, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print("advisory: validation-or-tool-failed", type(error).__name__)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
