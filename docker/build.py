"""Build a committed source snapshot with an explicitly selected local Docker builder."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]

def command(argv, cwd=None):
    return subprocess.run(argv, cwd=cwd, check=True, stdout=subprocess.PIPE, text=True).stdout.strip()

def capture_context(repo, destination):
    revision = command(["git", "rev-parse", "HEAD"], repo)
    tree = command(["git", "rev-parse", revision + "^{tree}"], repo)
    if command(["git", "status", "--porcelain", "--untracked-files=no"], repo):
        raise ValueError("Commit tracked changes before building the source snapshot")
    subprocess.run(["git", "archive", "--format=tar", "--output", str(destination), revision], cwd=repo, check=True)
    value = hashlib.sha256()
    with destination.open("rb") as stream:
        for block in iter(lambda: stream.read(65536), b""):
            value.update(block)
    return {"source_commit": revision, "source_tree": tree, "context_sha256": value.hexdigest()}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--builder", required=True, help="An existing dedicated named builder; the default is never changed")
    parser.add_argument("--platform", required=True, choices=("linux/amd64", "linux/arm64"))
    parser.add_argument("--tag")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--target", choices=("runtime", "qualification-artifacts"), default="runtime")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--jobs", type=int, default=2, choices=range(1, 9))
    args = parser.parse_args()
    if args.builder == "default":
        parser.error("Use a dedicated named builder")
    if args.report.exists() or (args.output and args.output.exists()):
        parser.error("Report and export destinations must be new")
    if (args.target == "runtime" and (not args.tag or args.output)) or (args.target == "qualification-artifacts" and (not args.output or args.tag)):
        parser.error("runtime requires --tag; qualification-artifacts requires --output")
    endpoint = json.loads(command(["docker", "context", "inspect", "--format", "{{json .Endpoints.docker.Host}}"] ))
    if not endpoint.startswith("unix://"):
        parser.error("This qualification helper requires a local Unix Docker endpoint")
    architecture = command(["docker", "info", "--format", "{{.Architecture}}"])
    native = {"aarch64": "arm64", "arm64": "arm64", "x86_64": "amd64", "amd64": "amd64"}.get(architecture)
    if args.platform != "linux/" + str(native):
        parser.error("Use a matching native daemon; emulation is not qualification")
    command(["docker", "buildx", "inspect", args.builder])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    with args.report.open("x") as report, tempfile.TemporaryDirectory(prefix="drmr-container-context-") as temp:
        context = Path(temp) / "context.tar"
        result = capture_context(ROOT, context)
        metadata = Path(temp) / "build.json"
        argv = ["docker", "buildx", "build", "--builder", args.builder, "--platform", args.platform,
                "--target", args.target, "--progress=plain", "--build-arg", "DRMR_REVISION=" + result["source_commit"],
                "--build-arg", "DRMR_TREE=" + result["source_tree"], "--build-arg", "BUILD_JOBS=" + str(args.jobs),
                "--metadata-file", str(metadata)]
        argv += ["--load", "--tag", args.tag] if args.tag else ["--output", "type=local,dest=" + str(args.output)]
        argv.append("-")
        with context.open("rb") as stream:
            completed = subprocess.run(argv, stdin=stream)
        result.update({"platform": args.platform, "builder": args.builder, "target": args.target, "exit_code": completed.returncode})
        if metadata.exists():
            result["build_metadata"] = json.loads(metadata.read_text())
        if completed.returncode == 0 and args.tag:
            image = json.loads(command(["docker", "image", "inspect", args.tag]))[0]
            result.update({"image_id": image["Id"], "labels": image["Config"]["Labels"], "size_bytes": image["Size"]})
            if image["Config"]["Labels"].get("org.opencontainers.image.revision") != result["source_commit"] or image["Config"]["Labels"].get("io.drmr.source.tree") != result["source_tree"]:
                raise ValueError("Built image source labels do not match the captured context")
        if completed.returncode == 0 and args.output:
            artifact_root = args.output / "out"
            result["qualification_manifest"] = json.loads((artifact_root / "qualification-manifest.json").read_text())
            result["artifacts"] = {name: {"path": str(artifact_root / name), "sha256": hashlib.sha256((artifact_root / name).read_bytes()).hexdigest()}
                                   for name in ("container-qualification", "drmr-native-fixture", "container-resources")}
        json.dump(result, report, indent=2)
        report.write("\n")
    raise SystemExit(completed.returncode)

if __name__ == "__main__":
    main()
