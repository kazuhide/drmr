#!/usr/bin/env python3
"""Check source and CLI release identities without building or publishing.

Python 3.9 or newer, Cargo, and (for --require-tag) Git are required. Cargo
resolves TOML authoritatively in locked, offline, no-dependency mode. Product
version declarations use the repository's canonical workspace inheritance form.
Only final vMAJOR.MINOR.PATCH tags are accepted; release candidates are not
published by this gate. Normal CI can omit --tag and --require-tag.
"""
import argparse
import datetime
import json
from pathlib import Path
import re
import subprocess
import sys

VERSION = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
MAX_SOURCE_BYTES = 4 * 1024 * 1024


class Failure(Exception):
    """A fixed diagnostic safe for public CI output."""


def require(condition, code):
    if not condition:
        raise Failure(code)


def unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate-json-member")
        result[key] = value
    return result


def read(path):
    try:
        with path.open("rb") as stream:
            data = stream.read(MAX_SOURCE_BYTES + 1)
        require(len(data) <= MAX_SOURCE_BYTES, "source-file-too-large")
        return data.decode("utf-8").replace("\r\n", "\n")
    except (OSError, UnicodeError):
        raise Failure("source-file-unavailable") from None


def run(argv, *, cwd=None, code, empty_stderr=False, timeout=120):
    """Capture output privately; failures never forward process diagnostics."""
    try:
        result = subprocess.run(argv, cwd=cwd, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=timeout, check=False)
    except (OSError, subprocess.SubprocessError):
        raise Failure(code) from None
    require(result.returncode == 0 and (not empty_stderr or not result.stderr), code)
    return result.stdout


def section(text, name):
    """Select canonical version fields; Cargo also validates the full document."""
    tables = list(re.finditer(r"(?m)^[ \t]*\[\[?[^\r\n]+?\]\]?[ \t]*(?:#.*)?$", text))
    selected = [index for index, match in enumerate(tables)
                if re.fullmatch(r"[ \t]*\[" + re.escape(name) + r"\][ \t]*(?:#.*)?", match.group())]
    require(len(selected) == 1, "version-declaration-invalid")
    index = selected[0]
    return text[tables[index].end():tables[index + 1].start() if index + 1 < len(tables) else len(text)]


def canonical_version(text):
    values = re.findall(r'(?m)^[ \t]*version[ \t]*=[ \t]*"([^"\r\n]*)"[ \t]*(?:#.*)?$', section(text, "workspace.package"))
    require(len(values) == 1 and re.fullmatch(VERSION, values[0]) is not None,
            "workspace-version-invalid")
    return values[0]


def inherited_version(text):
    values = re.findall(r"(?m)^[ \t]*version\.workspace[ \t]*=[ \t]*true[ \t]*(?:#.*)?$", section(text, "package"))
    require(len(values) == 1, "package-version-not-inherited")


def lock_versions(text, packages, version):
    """Check generated local package blocks; metadata --no-deps omits resolves."""
    found = {name: [] for name in packages}
    for block in re.split(r"(?m)^\[\[package\]\][ \t]*$", text)[1:]:
        names = re.findall(r'(?m)^name = "([^"\r\n]+)"[ \t]*$', block)
        require(len(names) == 1, "lock-package-invalid")
        name = names[0]
        if name not in found:
            continue
        sources = re.findall(r"(?m)^source[ \t]*=", block)
        if sources:
            continue
        versions = re.findall(r'(?m)^version = "([^"\r\n]+)"[ \t]*$', block)
        require(len(versions) == 1, "lock-package-invalid")
        found[name].append(versions[0])
    require(all(values == [version] for values in found.values()), "lock-version-mismatch")


def changelog_version(text, version):
    headings = re.findall(r"(?m)^##[ \t]+(.+?)[ \t]*$", text)
    releases = [heading for heading in headings if heading != "[Unreleased]"]
    require(releases, "changelog-header-invalid")
    versions = []
    for heading in releases:
        match = re.fullmatch(r"\[(" + VERSION + r")\] - ([0-9]{4}-[0-9]{2}-[0-9]{2})", heading)
        require(match is not None, "changelog-header-invalid")
        try:
            datetime.date.fromisoformat(match[2])
        except ValueError:
            raise Failure("changelog-header-invalid") from None
        versions.append(match[1])
    require(len(versions) == len(set(versions)), "changelog-version-duplicate")
    require(versions[0] == version, "changelog-version-mismatch")


def check_cli(path, version):
    output = run([str(path), "--version"], code="cli-command-failed", empty_stderr=True, timeout=30)
    require(output in (("drmr " + version + "\n").encode("ascii"),
                       ("drmr " + version + "\r\n").encode("ascii")), "cli-version-mismatch")


def git_publication(root, tag):
    def git(*args):
        return run(["git", *args], cwd=root, code="tag-command-failed").decode("ascii").strip()
    commit = git("rev-parse", "--verify", "HEAD^{commit}")
    tagged = git("rev-parse", "--verify", "refs/tags/" + tag + "^{commit}")
    require(re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", commit) is not None, "tag-command-failed")
    require(tagged == commit, "tag-commit-mismatch")
    # Ignored build outputs are allowed; all tracked changes and untracked files
    # must be committed or removed before a release identity is accepted.
    status = run(["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=root, code="tag-command-failed")
    require(not status, "working-tree-dirty")
    return commit


def check_release(root, *, tag=None, cli=None, require_tag=False):
    require(tag is None or re.fullmatch("v" + VERSION, tag) is not None, "tag-invalid")
    require(not require_tag or tag is not None, "tag-required")
    root = Path(root).resolve()
    version = canonical_version(read(root / "Cargo.toml"))
    require(tag is None or tag[1:] == version, "tag-version-mismatch")
    metadata = json.loads(run(["cargo", "metadata", "--manifest-path", str(root / "Cargo.toml"),
                               "--locked", "--offline", "--no-deps", "--format-version", "1"],
                              cwd=root, code="metadata-command-failed"), object_pairs_hook=unique)
    require(Path(metadata["workspace_root"]).resolve() == root, "workspace-root-mismatch")
    member_ids = metadata["workspace_members"]
    require(member_ids and len(member_ids) == len(set(member_ids)), "workspace-members-invalid")
    members = [package for package in metadata["packages"] if package["id"] in member_ids]
    require(len(members) == len(member_ids), "workspace-members-invalid")
    names = []
    for package in members:
        manifest = Path(package["manifest_path"]).resolve()
        require(root in manifest.parents, "workspace-member-outside-root")
        inherited_version(read(manifest))
        require(package["version"] == version, "package-version-mismatch")
        names.append(package["name"])
    require(len(names) == len(set(names)), "workspace-package-duplicate")
    lock_versions(read(root / "Cargo.lock"), names, version)
    desktop = json.loads(read(root / "crates/drmr-desktop/tauri.conf.json"), object_pairs_hook=unique)
    require(desktop.get("version") == version, "desktop-version-mismatch")
    changelog_version(read(root / "CHANGELOG.md"), version)
    if cli is not None:
        check_cli(Path(cli).resolve(), version)
    commit = git_publication(root, tag) if require_tag else None
    return {"schema_version": 1, "success": True, "version": version, "tag": tag,
            "packages": sorted(names), "cli_verified": cli is not None,
            "tag_verified": require_tag, "commit": commit}


class Parser(argparse.ArgumentParser):
    def error(self, message):
        raise Failure("arguments-invalid")


def main(argv=None):
    parser = Parser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1], help="Source checkout root")
    parser.add_argument("--tag", help="Expected final release tag: vMAJOR.MINOR.PATCH")
    parser.add_argument("--cli", type=Path, help="Check this built or extracted CLI with --version")
    parser.add_argument("--require-tag", action="store_true", help="Require the exact local tag at clean HEAD; requires --tag")
    try:
        args = parser.parse_args(argv)
        report = check_release(args.root, tag=args.tag, cli=args.cli, require_tag=args.require_tag)
    except Failure as error:
        report = {"schema_version": 1, "success": False, "error": str(error)}
    except (OSError, ValueError, TypeError, KeyError):
        report = {"schema_version": 1, "success": False, "error": "release-input-invalid"}
    print(json.dumps(report, sort_keys=True))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
