"""Build the pinned shaka component without runtime network installation."""
from native import PREFIX, WORK, RECIPE, collect_notices, digest, fetch_archive, git_archive, record, run, unpack, verify_file
import json
import shutil
def shaka(pin, jobs):
    source = WORK / "shaka-packager"
    source.mkdir(parents=True)
    run("git", "init", str(source))
    run("git", "remote", "add", "origin", pin["repository"], cwd=source)
    tag = "refs/tags/" + pin["tag"]
    run("git", "fetch", "--depth=1", "origin", tag + ":" + tag, cwd=source)
    run("git", "checkout", "--detach", pin["commit"], cwd=source)
    if run("git", "rev-parse", "HEAD", cwd=source, capture=True).strip() != pin["commit"]:
        raise ValueError("shaka-revision-mismatch")
    run("git", "submodule", "update", "--init", "--recursive", "--depth=1", "--jobs", str(jobs), cwd=source)
    actual = {}
    for line in run("git", "submodule", "status", "--recursive", cwd=source, capture=True).splitlines():
        if not line.startswith(" "):
            raise ValueError("shaka-submodule-unconfirmed")
        revision, path, *_ = line.split()
        actual[path] = revision
    if actual != {node["path"]: node["commit"] for node in pin["submodules"]}:
        raise ValueError("shaka-submodule-graph-mismatch")
    archives = []
    archive = PREFIX / "sources/shaka-packager.tar.gz"
    git_archive(source, archive)
    archives.append({"file": archive.name, "sha256": digest(archive)})
    for node in pin["submodules"]:
        home = source / node["path"]
        record("shaka-" + node["name"], node, home)
        archive = PREFIX / "sources" / ("shaka-" + node["name"] + ".tar.gz")
        git_archive(home, archive)
        archives.append({"file": archive.name, "sha256": digest(archive)})
    build = WORK / "shaka-build"
    (build / ".cmake/api/v1/query").mkdir(parents=True)
    (build / ".cmake/api/v1/query/codemodel-v2").touch()
    run("cmake", "-S", str(source), "-B", str(build), "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release",
        "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON", "-DUSE_SYSTEM_DEPENDENCIES=OFF")
    run("cmake", "--build", str(build), "--target", "packager", "--parallel", str(jobs))
    shutil.copy2(build / "packager/packager", PREFIX / "bin/packager")
    for kind in ("graph", "commands"):
        text = run("ninja", "-C", str(build), "-t", kind, "packager", capture=True)
        (PREFIX / "metadata" / ("shaka-build-" + kind + ".txt")).write_text(text)
    shutil.copytree(build / ".cmake/api/v1/reply", PREFIX / "metadata/shaka-cmake-targets")
    replies = build / ".cmake/api/v1/reply"
    model = json.loads(next(replies.glob("codemodel-v2-*.json")).read_text())
    targets = {item["id"]: json.loads((replies / item["jsonFile"]).read_text()) for item in model["configurations"][0]["targets"]}
    pending = [key for key, item in targets.items() if item["name"] == "packager"]
    if len(pending) != 1:
        raise ValueError("packager-build-target-ambiguous")
    reached = set()
    while pending:
        key = pending.pop()
        if key in reached:
            continue
        reached.add(key)
        pending.extend(row["id"] for row in targets[key].get("dependencies", []))
    selected = [{"name": targets[key]["name"], "type": targets[key]["type"],
                 "paths": targets[key]["paths"], "artifacts": targets[key].get("artifacts", []),
                 "compile_groups": targets[key].get("compileGroups", [])} for key in sorted(reached)]
    (PREFIX / "metadata/shaka-selected-targets.json").write_text(json.dumps(selected, indent=2) + "\n")
    record("shaka-packager", pin, source, {"source_archives": archives,
           "license_scope": "Conservative source-wide notices; selected packager graph retained separately"})


if __name__ == "__main__":
    import argparse
    from pathlib import Path
    parser = argparse.ArgumentParser(description="Build the pinned native component")
    parser.add_argument("--jobs", type=int, default=2, choices=range(1, 9))
    args = parser.parse_args()
    (PREFIX / "bin").mkdir(parents=True, exist_ok=True)
    WORK.mkdir(parents=True, exist_ok=True)
    pins = json.loads((Path(__file__).parent / "sources.json").read_text())
    shaka(pins["shaka"], args.jobs)
