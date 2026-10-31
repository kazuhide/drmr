"""Classify publication-only pull requests and require real native job evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


# This is a reviewed static-publication boundary, not a Markdown suffix filter.
# Crate READMEs, embedded console assets and every executable/build/test input
# remain protected. Main pushes always qualify the new distribution source.
SITE_FILES = frozenset("site/" + name for name in (
    "index.html", "ja/index.html", "guide/index.html", "ja/guide/index.html",
    "assets/site.css", "assets/guide.css", "assets/drmr.svg",
    "assets/fonts/ibm-plex-sans-latin-400-normal.woff2",
    "assets/fonts/ibm-plex-sans-latin-600-normal.woff2",
    "assets/fonts/ibm-plex-mono-latin-400-normal.woff2",
    "assets/fonts/LICENSE-ibm-plex.txt", "assets/fonts/README.txt",
))
GUIDE_FILES = frozenset("site-content/guide/" + name for name in ("en.md", "ja.md", "README.md"))
GATE_NAMES = {"ci.yml": "CI / required checks", "windows.yml": "Windows / required checks",
              "container.yml": "Container / required checks", "advisories.yml": "Advisories / required checks"}
SCOPE_NAME = "scope / Classify changes"
NATIVE_NEEDS = {"ci.yml": ("verify", "core-tests", "console-browser", "release"),
                "windows.yml": ("native", "release"), "container.yml": ("qualify",),
                "advisories.yml": ("target-graphs",)}
NATIVE_JOBS = {
    "ci.yml": tuple(stage + " / " + os + " / " + rust
                    for stage in ("Rust", "Rust core", "CLI release")
                    for os in ("ubuntu-24.04", "macos-15") for rust in ("minimum", "stable"))
              + ("Console / Chromium / common CLI workflow",),
    "windows.yml": tuple("Windows / " + rust + " / private files and process trees" for rust in ("minimum", "stable"))
                   + tuple("Windows CLI release / " + rust for rust in ("minimum", "stable")),
    "container.yml": ("Docker / native Linux AMD64 / real media and lifecycle",),
    "advisories.yml": ("CLI and Desktop target advisory graphs",),
    "pages.yml": ("Pages / static validation / ubuntu-24.04", "Pages / static validation / windows-latest",
                  "Pages / Chromium accessibility and layout", "Pages / Guide Chromium accessibility and layout",
                  "Pages / publish", "Pages / published Chromium verification"),
}


def document_path(name):
    if not isinstance(name, str) or "\\" in name or any(ord(c) < 32 or ord(c) == 127 for c in name):
        return False
    parts = name.split("/")
    if any(part in ("", ".", "..") for part in parts):
        return False
    return (name == "README.md" or name in GUIDE_FILES or name in SITE_FILES
            or (len(parts) >= 2 and parts[0] == "docs" and parts[-1].endswith(".md")))


def protected_inventory(entries):
    return {name: value for name, value in entries.items() if not document_path(name)}


def inventory_digest(entries):
    raw = json.dumps(sorted(protected_inventory(entries).items()), separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(raw.encode("ascii")).hexdigest()


def documentation_only(before, after):
    changed = {name for name in before.keys() | after.keys() if before.get(name) != after.get(name)}
    return (bool(changed) and protected_inventory(before) == protected_inventory(after)
            and all(document_path(name) and all(
                entries[name][:2] == ("100644", "blob")
                for entries in (before, after) if name in entries) for name in changed))


class Refusal(ValueError):
    """A fixed diagnostic code; never include Git output or private paths."""


def require(condition, code):
    if not condition:
        raise Refusal(code)


def valid_sha(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{40}", value) is not None


def git(root, *args):
    try:
        return subprocess.run(["git", *args], cwd=root, capture_output=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        raise Refusal("source-evidence-unavailable") from None


def tree(root, revision):
    entries = {}
    try:
        for row in git(root, "ls-tree", "-rz", revision).split(b"\0"):
            if not row:
                continue
            metadata, name = row.split(b"\t", 1)
            mode, kind, oid = metadata.decode("ascii").split(" ")
            path = name.decode("utf-8")
            require(path not in entries and valid_sha(oid), "source-evidence-invalid")
            entries[path] = (mode, kind, oid)
    except (UnicodeError, ValueError):
        raise Refusal("source-evidence-invalid") from None
    return entries


def classify(root, event_name, payload, source):
    require(valid_sha(source), "source-identity-invalid")
    require(git(root, "rev-parse", "HEAD").decode("ascii").strip() == source, "source-checkout-mismatch")
    require(not git(root, "status", "--porcelain=v1", "--untracked-files=all"), "source-checkout-dirty")
    report = {"schema_version": 1, "profile": "full", "reason": "event-requires-native",
              "source_sha": source, "head_sha": source, "base_sha": None,
              "protected_base_sha256": None, "protected_head_sha256": None, "changed_files": None}
    if event_name != "pull_request":
        return report
    try:
        base, head = payload["pull_request"]["base"]["sha"], payload["pull_request"]["head"]["sha"]
    except (KeyError, TypeError):
        raise Refusal("pull-request-evidence-invalid") from None
    require(valid_sha(base) and valid_sha(head), "pull-request-evidence-invalid")
    parents = git(root, "rev-list", "--parents", "-n", "1", source).decode("ascii").strip().split(" ")
    require(parents == [source, base, head], "pull-request-checkout-mismatch")
    before, after = tree(root, base), tree(root, head)
    report.update(base_sha=base, head_sha=head, protected_base_sha256=inventory_digest(before),
                  protected_head_sha256=inventory_digest(after),
                  changed_files=sum(before.get(name) != after.get(name) for name in before.keys() | after.keys()),
                  reason="protected-inputs-changed")
    # A branch behind its current base is conservatively checked in full.
    ancestor = git(root, "merge-base", base, head).decode("ascii").strip()
    if ancestor == base and documentation_only(before, after) and tree(root, source) == after:
        report.update(profile="documentation", reason="publication-only-runtime-unchanged")
    return report


def positive(value):
    return type(value) is int and value > 0


def load_json(raw):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            require(key not in value, "ci-json-invalid")
            value[key] = item
        return value

    def constant(_value):
        raise Refusal("ci-json-invalid")

    try:
        return json.loads(raw, object_pairs_hook=unique, parse_constant=constant)
    except (ValueError, UnicodeError, TypeError):
        raise Refusal("ci-json-invalid") from None


def validate_jobs(workflow, jobs, head, run_id, attempt, *, release=False, profile="full"):
    require(workflow in NATIVE_JOBS and valid_sha(head) and positive(run_id) and positive(attempt),
            "job-contract-invalid")
    require(isinstance(jobs, list) and 0 < len(jobs) <= 256, "job-list-invalid")
    by_name, ids = {}, set()
    for job in jobs:
        require(isinstance(job, dict) and positive(job.get("id")) and job["id"] not in ids
                and isinstance(job.get("name"), str) and job["name"] not in by_name
                and job.get("head_sha") == head and type(job.get("run_id")) is int and job["run_id"] == run_id
                and type(job.get("run_attempt")) is int and job["run_attempt"] == attempt, "job-binding-invalid")
        by_name[job["name"]] = job
        ids.add(job["id"])
    auxiliary = {SCOPE_NAME, GATE_NAMES[workflow]} if workflow in GATE_NAMES else set()
    if profile == "documentation":
        require(not release and workflow in GATE_NAMES, "native-jobs-required")
        require(all(job.get("status") == "completed" and job.get("conclusion") == "skipped"
                    for name, job in by_name.items() if name not in auxiliary), "native-skip-not-confirmed")
    else:
        require(profile == "full", "job-profile-invalid")
        require(set(by_name) <= set(NATIVE_JOBS[workflow]) | auxiliary, "unexpected-native-job")
        for name in NATIVE_JOBS[workflow]:
            job = by_name.get(name, {})
            require(job.get("status") == "completed" and job.get("conclusion") == "success", "native-jobs-incomplete")
    if workflow in GATE_NAMES:
        scope = by_name.get(SCOPE_NAME, {})
        require(scope.get("status") == "completed" and scope.get("conclusion") == "success", "scope-job-incomplete")
        gate = by_name.get(GATE_NAMES[workflow], {})
        require(bool(gate), "required-gate-missing")
        if release:
            require(gate.get("status") == "completed" and gate.get("conclusion") == "success", "required-gate-incomplete")
        else:
            require((gate.get("status"), gate.get("conclusion")) in
                    (("in_progress", None), ("completed", "success")), "required-gate-incomplete")


def validate_gate(workflow, report, needs, jobs, run_id, attempt):
    require(workflow in NATIVE_NEEDS and isinstance(needs, dict)
            and set(needs) == {"scope", *NATIVE_NEEDS[workflow]}, "gate-needs-invalid")
    scope = needs["scope"]
    require(isinstance(scope, dict) and scope.get("result") == "success"
            and isinstance(scope.get("outputs"), dict), "scope-job-incomplete")
    require(scope["outputs"].get("profile") == report["profile"]
            and scope["outputs"].get("source_sha") == report["source_sha"], "scope-output-mismatch")
    expected = "skipped" if report["profile"] == "documentation" else "success"
    require(all(isinstance(needs[name], dict) and needs[name].get("result") == expected
                for name in NATIVE_NEEDS[workflow]), "native-needs-incomplete")
    validate_jobs(workflow, jobs, report["head_sha"], run_id, attempt, profile=report["profile"])


def job_rows(client, endpoint):
    result, total = [], None
    for page in range(1, 4):
        response = client(endpoint + "?per_page=100&page=" + str(page))
        require(isinstance(response, dict) and type(response.get("total_count")) is int
                and 0 < response["total_count"] <= 256 and isinstance(response.get("jobs"), list)
                and len(response["jobs"]) <= 100, "job-list-invalid")
        total = response["total_count"] if total is None else total
        require(response["total_count"] == total, "job-list-changed")
        result.extend(response["jobs"])
        if len(result) == total:
            return result
        require(len(response["jobs"]) == 100 and len(result) < total, "job-list-incomplete")
    raise Refusal("job-list-incomplete")


def current_jobs(repository, run_id, attempt):
    require(isinstance(repository, str) and re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository)
            and positive(run_id) and positive(attempt), "workflow-run-binding-invalid")

    def client(endpoint):
        try:
            raw = subprocess.run(["gh", "api", "--hostname", "github.com", endpoint],
                                 capture_output=True, check=True, timeout=30).stdout
            require(len(raw) <= 2 * 1024 * 1024, "job-list-invalid")
            return load_json(raw)
        except (OSError, subprocess.SubprocessError, ValueError):
            raise Refusal("job-evidence-unavailable") from None

    return job_rows(client, "repos/" + repository + "/actions/runs/" + str(run_id)
                    + "/attempts/" + str(attempt) + "/jobs")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("classify", "gate"))
    parser.add_argument("--workflow", choices=tuple(GATE_NAMES))
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    try:
        payload = load_json(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
        report = classify(Path.cwd(), os.environ["GITHUB_EVENT_NAME"], payload, os.environ["GITHUB_SHA"])
        if args.mode == "gate":
            run_id, attempt = int(os.environ["GITHUB_RUN_ID"]), int(os.environ["GITHUB_RUN_ATTEMPT"])
            jobs = current_jobs(os.environ["GITHUB_REPOSITORY"], run_id, attempt)
            validate_gate(args.workflow, report, load_json(os.environ["NEEDS_JSON"]), jobs, run_id, attempt)
        report["success"] = True
    except Refusal as error:
        report = {"schema_version": 1, "success": False, "error": str(error)}
    except (KeyError, OSError, ValueError, TypeError, UnicodeError):
        report = {"schema_version": 1, "success": False, "error": "ci-input-invalid"}
    with args.report.open("x", encoding="utf-8") as output:
        json.dump(report, output, sort_keys=True)
        output.write("\n")
    if report["success"] and args.mode == "classify":
        with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
            output.write("profile=" + report["profile"] + "\nsource_sha=" + report["source_sha"] + "\n")
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a", encoding="utf-8") as output:
            if report["success"]:
                output.write("### Validation scope: " + report["profile"] + "\n\n")
                output.write("Native checks are not run for this proven publication-only pull request.\n"
                             if report["profile"] == "documentation" else "All native qualification profiles are required.\n")
            else:
                output.write("CI qualification refused: " + report["error"] + "\n")
    print(json.dumps(report, sort_keys=True))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
