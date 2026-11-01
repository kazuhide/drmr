"""Exercise attributed codecs with the extracted CLI and unavailable decoders.

The caller owns archive/source identity and final workspace cleanup. This module
never compiles, downloads or regenerates media, and never requests decoding.
"""
import concurrent.futures
import importlib.util
import json
from pathlib import Path
import secrets
import struct
import subprocess
import sys
import xml.etree.ElementTree as ET

_ROOT = Path(__file__).resolve().parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, _ROOT / ("tools/" + name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


CONTRACT = module("codec_contract")
BASE = module("release_roundtrip")
SUPPORT = BASE.SUPPORT
require = CONTRACT.require


class Executor(BASE.Executor):
    def __init__(self, cli, root):
        super().__init__(cli, root)
        self.material = tuple(tuple(secrets.token_hex(16) for _ in range(3)) for _ in range(2))
        require(self.material[0][0] != self.material[1][0] and self.material[0][1] != self.material[1][1],
                "codec-key-slots-not-distinct")
        self.uris = ("skd://codec-qualification-0", "skd://codec-qualification-1")
        self.sensitive += tuple(value.encode("ascii") for row in self.material for value in row[1:]) + tuple(
            value.encode("ascii") for value in self.uris)

    def key_file(self, root, profile, index=0, input_policy="correct"):
        content = [{"kid": row[0], "key": row[1] if input_policy == "correct" else secrets.token_hex(16)}
                   for row in self.material]
        hls = [{"uri": uri, "key": row[1] if input_policy == "correct" else secrets.token_hex(16), "iv": row[2]}
               for row, uri in zip(self.material, self.uris)]
        value = {"schema_version": 1, "input": {"keys": content, "hls_keys": hls}}
        if input_policy == "missing":
            value["input"] = {"keys": [], "hls_keys": []}
        output = BASE.keys(profile, *self.material[index], self.uris[index])
        if output is not None and input_policy == "wrong":
            output["key"] = secrets.token_hex(16)
        if input_policy == "missing":
            output = None
        if output is not None:
            value["output"] = output
        path = root / "keys.json"
        SUPPORT.private_write(path, json.dumps(value).encode("ascii"))
        return path

    def native(self, value, profile, scope="comparison"):
        result = CONTRACT.EVIDENCE.v2_proof(value, scope=scope)
        CONTRACT.proof(result, profile[0], scope)
        return result

    def convert(self, source, profile, fixture, root, processing="auto", key_index=0):
        root.mkdir(mode=0o700)
        runner = self.runner(root)
        before = BASE.graph(source)
        key_file = self.key_file(root, profile, key_index)
        arguments = ["--keys-file", key_file, "--packager-bin", self.packager, "--max-workers", "1", "--json"]
        analyzed = runner.json([self.cli, "analyze", source, *arguments], "codec-analyze")
        require(isinstance(analyzed, dict) and analyzed.get("kind") == "input-analysis"
                and analyzed.get("schema_version") == 2 and analyzed.get("input_admitted") is True,
                "codec-analysis-invalid")
        analysis = self.native(analyzed.get("verification"), profile, "source-admission")
        inventory = analyzed.get("source")
        require(isinstance(inventory, dict) and CONTRACT.hexadecimal(inventory.get("graph_digest")), "codec-analysis-invalid")
        output = root / "output"
        if profile[0] == "mp4":
            output.mkdir(mode=0o700)
            destination = output / "asset.mp4"
        else:
            destination = output
        plan_path = root / "plan.json"
        planned = runner.json([self.cli, "plan", source, "--to", profile[1], "--format",
                               "hls" if profile[0] == "hls-fmp4" else profile[0], "--output", destination,
                               "--processing", processing, "--save", plan_path, *arguments], "codec-plan")
        plan_bytes = SUPPORT.read_regular(plan_path, 1024 * 1024)
        require(planned == SUPPORT.load_json(plan_bytes) and planned.get("kind") == "execution-plan"
                and planned.get("schema_version") == 9 and planned.get("required_depth") == "native"
                and planned.get("source") == inventory and CONTRACT.hexadecimal(planned.get("plan_id")),
                "codec-plan-invalid")
        intent = planned.get("intent", {})
        intended = Path(intent.get("output", ""))
        require(intent.get("target") == profile[1]
                and intent.get("format") == ("hls" if profile[0] == "hls-fmp4" else profile[0])
                and intended.is_absolute() and intended.name == destination.name
                and intended.parent.samefile(destination.parent), "codec-plan-intent-invalid")
        converted = runner.json([self.cli, "convert", "--plan", plan_path, *arguments], "codec-convert")
        path = destination if profile[0] == "mp4" else output / ("manifest.mpd" if profile[0] == "dash" else "master.m3u8")
        inspection = converted.get("output", {})
        tracks = inspection.get("tracks", [])
        require(converted.get("plan_id") == planned["plan_id"] and inspection.get("format") == (
                "hls" if profile[0] == "hls-fmp4" else profile[0]) and inspection.get("protection") == profile[1]
                and len(tracks) == 1 and tracks[0].get("kind") == "video", "codec-conversion-invalid")
        # Manifest track codecs may be a complete RFC 6381 string; native
        # inspection and the fixed source digest establish the actual profile.
        codec = tracks[0].get("codec")
        require(isinstance(codec, str) and codec.startswith(("hvc1", "hev1") if fixture["codec"] == "hevc" else ("vp09",)),
                "codec-conversion-codec-invalid")
        verified = runner.json([self.cli, "verify", "--plan", plan_path, *arguments], "codec-independent-verify")
        require(verified.get("plan_id") == planned["plan_id"] and verified.get("source_unchanged") is True
                and verified.get("output_matches_plan") is True, "codec-independent-plan-invalid")
        require(before == BASE.graph(source) and SUPPORT.read_regular(plan_path, 1024 * 1024) == plan_bytes,
                "codec-source-or-plan-changed")
        result = {"format": profile[0], "protection": profile[1], "tracks": ["video"],
                  "drms": BASE.manifest_profile(path, profile), "analysis": analysis,
                  "plan": {"schema_version": planned["schema_version"], "plan_id": planned["plan_id"],
                           "sha256": CONTRACT.sha(plan_bytes), "processing": planned["processing"]},
                  "conversion": self.native(converted.get("verification"), profile),
                  "independent_verification": self.native(verified.get("verification"), profile),
                  "output_graph": BASE.graph(output), "source_and_plan_unchanged": True}
        CONTRACT.leg(result, profile, processing)
        return path, result

    def refusal(self, case, seed, encrypted, root):
        root.mkdir(mode=0o700)
        runner = self.runner(root)
        before = (BASE.graph(seed), BASE.graph(encrypted))
        output, plan_path = root / "unexpected-output", root / "unexpected-plan.json"
        kind = case["check"]
        key_file = self.key_file(root, ("mp4", "cenc"), input_policy={
            "missing-key": "missing", "wrong-key": "wrong"}.get(kind, "correct"))
        private = SUPPORT.load_json(SUPPORT.read_regular(key_file, 65536))
        materials = [*private["input"]["keys"], *private["input"]["hls_keys"], private.get("output", {})]
        runner.secrets += tuple(item[field].encode("ascii") for item in materials for field in ("key", "iv") if field in item)
        options = ["--keys-file", key_file, "--packager-bin", self.packager, "--max-workers", "1", "--json"]
        if kind in ("ts-output", "cbcs-output"):
            command = "plan"
            arguments = [command, seed, "--to", "clear" if kind == "ts-output" else "cbcs",
                         "--format", "hls-ts" if kind == "ts-output" else "dash", "--output", output,
                         "--save", plan_path, *options]
        else:
            command = "verify"
            result = encrypted
            if kind == "changed-sample":
                data = bytearray(SUPPORT.read_regular(seed, SUPPORT.MAX_GRAPH_BYTES))
                offset = 0
                while offset + 8 <= len(data):
                    size, box = struct.unpack_from(">I4s", data, offset)
                    require(8 <= size <= len(data) - offset, "codec-fixture-box-invalid")
                    if box == b"mdat":
                        require(size > 8, "codec-fixture-box-invalid")
                        data[offset + size - 1] ^= 1
                        break
                    offset += size
                else:
                    raise CONTRACT.Failure("codec-fixture-box-invalid")
                result = root / "changed.mp4"
                SUPPORT.private_write(result, data)
            arguments = [command, result, "--source", seed, *options]
        status = 2 if kind == "missing-key" else 1
        raw = runner.run([self.cli, *arguments], "codec-refusal", expected_exit=status)
        value = SUPPORT.load_json(raw)
        code = value.get("error", {}).get("code") if isinstance(value, dict) else None
        require(isinstance(value, dict) and value.get("command") == command and value.get("status") == "failed"
                and code in CONTRACT.REFUSAL_CODES[kind], "codec-refusal-invalid")
        require(before == (BASE.graph(seed), BASE.graph(encrypted)) and not output.exists() and not output.is_symlink()
                and not plan_path.exists() and not plan_path.is_symlink(),
                "codec-refusal-source-or-output-changed")
        return {**case, "status": "refused", "exit_code": status, "code": code,
                "source_unchanged": True, "destination_absent": True}


ERRORS = (CONTRACT.Failure, BASE.CONTRACT.Failure, SUPPORT.Refusal, OSError, ValueError, TypeError, KeyError,
          AttributeError, ET.ParseError, subprocess.SubprocessError)
FAILURE_STAGES = frozenset(("codec-analyze", "codec-plan", "codec-convert", "codec-independent-verify", "codec-final-against-original"))


def failure(case, error):
    # Every emitted description is fixed here or is an allowlisted classification
    # from the shared runner. Operator paths and child output never enter reports.
    code = str(error) if isinstance(error, CONTRACT.Failure) else "codec-operation-failed"
    result = {**case, "status": "failed", **BASE.failure_fields(error, code, "codec-qualification")}
    if isinstance(error, SUPPORT.Refusal) and error.stage in FAILURE_STAGES:
        result["failed_stage"] = error.stage
    return result


def execute_case(case, executor, profiles, seeds, fixtures, parent):
    workspace = SUPPORT.Workspace(parent=parent)
    try:
        fixture = fixtures[case["fixture"]]
        source = profiles[(case["fixture"], tuple(case["source"]))]
        seed = seeds[case["fixture"]]
        before = (BASE.graph(source), BASE.graph(seed))
        require(not executor.cancel.is_set(), "codec-cancelled")
        forward_path, forward = executor.convert(source, tuple(case["target"]), fixture, workspace.path / "forward",
                                                  case["processing"], 1)
        final_path, backward = executor.convert(forward_path, tuple(case["source"]), fixture, workspace.path / "backward",
                                                case["processing"], 0)
        check = workspace.path / "final"
        check.mkdir(mode=0o700)
        key_file = executor.key_file(check, tuple(case["source"]))
        final = executor.verify(executor.runner(check), seed, final_path, key_file,
                                tuple(case["source"]), "codec-final-against-original")
        require(before == (BASE.graph(source), BASE.graph(seed)), "codec-source-changed")
        return {**case, "status": "complete", "source_unchanged": True,
                "rekeyed": case["source"][1] != "clear" and case["target"][1] != "clear",
                "forward": forward, "backward": backward, "final_against_original": final}
    except ERRORS as error:
        return failure(case, error)
    finally:
        workspace.close()


def qualify(cli, source, binding, root, scope="smoke", jobs=4):
    """The final caller binds and cleans this report together with its archive."""
    SUPPORT.require_native(binding["target"])
    require(type(jobs) is int and 1 <= jobs <= 8, "codec-workers-invalid")
    cases = CONTRACT.cases(scope)
    root.mkdir(mode=0o700)
    executor = Executor(cli, root)
    guards = root / "guard-check"
    guards.mkdir(mode=0o700)
    executor.check_decoder_guards(guards)
    seeds, profiles = {}, {}
    fixtures = {fixture["id"]: fixture for fixture in CONTRACT.FIXTURES}
    for ordinal, fixture in enumerate(CONTRACT.FIXTURES):
        data = source.read(fixture["path"])
        require(CONTRACT.sha(data) == fixture["sha256"], "codec-fixture-identity-invalid")
        directory = root / f"seed-{ordinal}"
        directory.mkdir(mode=0o700)
        seed = directory / "seed.mp4"
        SUPPORT.private_write(seed, data)
        seeds[fixture["id"]] = seed
        profiles[(fixture["id"], ("mp4", "clear"))] = seed
        needed = {tuple(case["source"]) for case in cases if case["fixture"] == fixture["id"]}
        # One independently verified protected profile also supplies the wrong /
        # missing-key controls. It is reused instead of regenerated per refusal.
        needed.add(("mp4", "cenc"))
        for index, profile in enumerate(sorted(needed)):
            if profile != ("mp4", "clear"):
                path, _ = executor.convert(seed, profile, fixture, root / f"profile-{ordinal}-{index}")
                profiles[(fixture["id"], profile)] = path
    records = [None] * len(cases)
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = {}
        try:
            for index, case in enumerate(cases):
                future = pool.submit(execute_case, case, executor, profiles, seeds, fixtures, root)
                futures[future] = index
            for completed, future in enumerate(concurrent.futures.as_completed(futures), 1):
                records[futures[future]] = future.result()
                if scope == "full" and (completed % 32 == 0 or completed == len(cases)):
                    print(f"Release codecs: {completed}/{len(cases)} roundtrips checked", flush=True)
        except BaseException:
            executor.cancel.set()
            for future in futures:
                future.cancel()
            raise
    refusals = []
    for ordinal, case in enumerate(CONTRACT.refusals()):
        workspace = SUPPORT.Workspace(parent=root)
        try:
            refusals.append(executor.refusal(case, seeds[case["fixture"]],
                            profiles[(case["fixture"], ("mp4", "cenc"))], workspace.path / "refusal"))
        except ERRORS as error:
            refusals.append(failure(case, error))
        finally:
            workspace.close()
    failed = sum(record["status"] == "failed" for record in (*records, *refusals))
    result = {"schema_version": 1, "kind": "native-cli-codec-qualification", "scope": scope, "success": failed == 0,
              **binding, "fixtures": list(CONTRACT.FIXTURES), "matrix_sha256": CONTRACT.matrix_digest(scope),
              "records": records, "refusals": refusals, "decoder_guard": {"ffmpeg": "unavailable", "ffprobe": "unavailable"},
              "decode_evidence": "not-checked", "license_playback_verified": False,
              "counts": {"expected": len(cases), "complete": sum(record["status"] == "complete" for record in records),
                         "refusals": sum(record["status"] == "refused" for record in refusals), "failed": failed, "not_checked": 0},
              "native_ordinary_user": True, "owned_workspace_cleanup": "confirmed", "limits": CONTRACT.LIMITS}
    if result["success"]:
        CONTRACT.validate(result, scope, *(binding[name] for name in (
            "source_head", "source_tree", "target", "version", "archive_sha256", "binary_sha256")))
    return result


def qualify_archive(archive_path, target, version, source_root, scope="full", jobs=4):
    """Requalify a CI archive without rebuilding, tagging or publishing it."""
    SUPPORT.require_native(target)
    require(not SUPPORT.VERIFY.command(["git", "status", "--porcelain", "--untracked-files=normal"], source_root).strip(),
            "codec-clean-source-required")
    source = SUPPORT.VERIFY.Source(source_root)
    SUPPORT.require_version(source, version)
    workspace = SUPPORT.Workspace()
    try:
        snapshot = workspace.path / "candidate.archive"
        SUPPORT.snapshot_archive(archive_path, snapshot)
        try:
            expected = SUPPORT.VERIFY.build_expectation(source, "cli", target, SUPPORT.VERIFY.metadata(source, "cli", target))
            archive = SUPPORT.VERIFY.qualify(snapshot, source, expected)
        except SUPPORT.ARCHIVE_ERRORS + (subprocess.SubprocessError,):
            raise CONTRACT.Failure("codec-archive-verification-failed") from None
        require(archive.get("success") is True, "codec-archive-verification-failed")
        binary_hash = archive["binaries"]["drmr"]["sha256"]
        cli = SUPPORT.extract_cli(snapshot, target, binary_hash, workspace.path / ("drmr.exe" if sys.platform == "win32" else "drmr"))
        runner = SUPPORT.Runner(workspace.path)
        require(runner.run([cli, "--version"], "version", 20).strip() == ("drmr " + version).encode("ascii"),
                "codec-binary-version-invalid")
        result = qualify(cli, source, {"source_head": source.head, "source_tree": source.tree, "target": target,
                         "version": version, "archive_sha256": archive["archive_sha256"], "binary_sha256": binary_hash},
                         workspace.path / "codecs", scope, jobs)
        current = SUPPORT.VERIFY.Source(source_root)
        require(current.head == source.head and current.tree == source.tree
                and not SUPPORT.VERIFY.command(["git", "status", "--porcelain", "--untracked-files=normal"], source_root).strip()
                and SUPPORT.VERIFY.file_digest(snapshot) == archive["archive_sha256"]
                and SUPPORT.VERIFY.file_digest(cli) == binary_hash, "codec-source-or-artifact-changed")
        return result
    finally:
        workspace.close()


def main(argv=None):
    parser = SUPPORT.Parser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--source", type=Path, default=_ROOT)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--scope", choices=("smoke", "full"), default="full")
    parser.add_argument("--jobs", type=int, default=4)
    report_path = None
    try:
        args = parser.parse_args(argv)
        require(not args.report.exists() and not args.report.is_symlink()
                and not args.report.resolve().is_relative_to(args.source.resolve()), "codec-report-destination-invalid")
        report_path = args.report
        result = qualify_archive(args.archive.resolve(), args.target, args.expected_version, args.source.resolve(), args.scope, args.jobs)
    except ERRORS + (SUPPORT.VERIFY.Failure,):
        result = {"schema_version": 1, "kind": "native-cli-codec-qualification", "success": False,
                  "failure_code": "codec-qualification-failed"}
    except KeyboardInterrupt:
        result = {"schema_version": 1, "kind": "native-cli-codec-qualification", "success": False,
                  "failure_code": "codec-qualification-interrupted"}
    if report_path is not None:
        try:
            SUPPORT.private_write(report_path, (json.dumps(result, indent=2, sort_keys=True) + "\n").encode("ascii"))
        except (SUPPORT.Refusal, OSError):
            print('{"success":false,"failure_code":"codec-report-unavailable"}')
            return 1
    print(json.dumps({"success": result["success"], "counts": result.get("counts")}))
    return 0 if result["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
