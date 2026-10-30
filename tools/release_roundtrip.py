#!/usr/bin/env python3
"""Qualify release-only A -> B -> A conversions with an extracted native archive.

Requires a checked native-input packet from prepare_release.py, Python 3.9+,
Shaka Packager, and FFmpeg/FFprobe for fixture generation only. No CLI is built,
no network media is fetched, and native verification never requests decoding.
"""
import concurrent.futures
import json
import os
from pathlib import Path
import secrets
import shutil
import struct
import sys
import threading
import time
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import roundtrip_contract as CONTRACT
import roundtrip_fixtures as FIXTURES

SUPPORT = CONTRACT.module("qualify_cli_archive")
PREPARE = CONTRACT.module("prepare_release")
require = CONTRACT.require


def read(path, limit=2 * 1024 * 1024):
    return SUPPORT.load_json(SUPPORT.read_regular(path, limit))


def graph(path):
    return SUPPORT.graph_identity(path.parent if path.is_file() else path)


def packet(inputs, source, target):
    manifest = read(inputs / "release-manifest.json")
    require(manifest.get("kind") == "native-cli-roundtrip-inputs" and manifest.get("publication_ready") is False
            and manifest.get("source_head") == source.head and manifest.get("source_tree") == source.tree,
            "roundtrip-input-packet-invalid")
    assets = [asset for asset in manifest.get("assets", []) if asset.get("target") == target]
    require(len(assets) == 1, "roundtrip-input-target-invalid")
    asset = assets[0]
    for field in ("filename",):
        require(isinstance(asset.get(field), str) and Path(asset[field]).name == asset[field], "roundtrip-input-path-invalid")
    archive = inputs / asset["filename"]
    require(SUPPORT.VERIFY.file_digest(archive) == asset["sha256"], "roundtrip-input-digest-invalid")
    proofs = []
    for name in ("archive_report", "media_report"):
        reference = asset[name]
        require(Path(reference["filename"]).name == reference["filename"], "roundtrip-input-path-invalid")
        path = inputs / reference["filename"]
        require(SUPPORT.VERIFY.file_digest(path) == reference["sha256"], "roundtrip-input-digest-invalid")
        proofs.append(read(path))
    binary = PREPARE.reports(*proofs, source.head, source.tree, target, manifest["version"], asset["sha256"])
    require(binary == asset["binary_sha256"], "roundtrip-input-binary-invalid")
    return manifest["version"], archive, proofs[0]


def keys(profile, kid, key, iv, uri):
    container, protection = profile
    if protection == "clear":
        return None
    value = {"key": key}
    if protection in ("cenc", "cbcs"):
        value["kid"] = kid
    if container.startswith("hls-"):
        value["hls_key_uri"] = uri
    if protection == "hls-sample-aes":
        value["iv"] = iv
    if container == "dash":
        payload = bytes.fromhex("1210" + kid)
        pssh = struct.pack(">I4sI", 32 + len(payload), b"pssh", 0) + bytes.fromhex("edef8ba979d64acea3c827dcd51d21ed")
        pssh += struct.pack(">I", len(payload)) + payload
        value["signaling"] = {"system": "multi-drm", "widevine_pssh": pssh.hex(),
                              "playready_license_url": "https://license.example.invalid/roundtrip"}
    elif container.startswith("hls-") and protection in ("cbcs", "hls-sample-aes"):
        value["signaling"] = {"system": "multi-drm", "fairplay": True}
    return value


def manifest_profile(path, profile):
    container, protection = profile
    if container == "mp4":
        return []
    if container == "dash":
        document = ET.parse(path).getroot()
        namespace = {"d": "urn:mpeg:dash:schema:mpd:2011"}
        adaptations = document.findall(".//d:AdaptationSet", namespace)
        require(bool(adaptations), "roundtrip-dash-profile-invalid")
        expected = set(CONTRACT.drms(profile))
        systems = {"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed": "widevine",
                   "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95": "playready"}
        for adaptation in adaptations:
            protections = adaptation.findall("d:ContentProtection", namespace)
            common = [item for item in protections if item.get("schemeIdUri") == "urn:mpeg:dash:mp4protection:2011"]
            found = {systems[item.get("schemeIdUri").lower()] for item in protections
                     if item.get("schemeIdUri", "").lower() in systems}
            require(found == expected and (not protections if protection == "clear" else
                    len(protections) == 3 and len(common) == 1 and common[0].get("value") == protection),
                    "roundtrip-dash-profile-invalid")
        return CONTRACT.drms(profile)
    playlists = list(path.parent.rglob("*.m3u8"))
    media = [playlist.read_text(encoding="utf-8") for playlist in playlists
             if "#EXTINF:" in playlist.read_text(encoding="utf-8")]
    require(bool(media), "roundtrip-hls-profile-invalid")
    method = {"clear": None, "cenc": "SAMPLE-AES-CTR", "cbcs": "SAMPLE-AES",
              "hls-aes128": "AES-128", "hls-sample-aes": "SAMPLE-AES"}[protection]
    for text in media:
        require(("#EXT-X-MAP:" in text) == (container == "hls-fmp4"), "roundtrip-container-invalid")
        key_lines = [line for line in text.splitlines() if line.startswith("#EXT-X-KEY:")]
        require((not key_lines) if method is None else bool(key_lines)
                and all("METHOD=" + method + "," in line for line in key_lines), "roundtrip-hls-profile-invalid")
        if CONTRACT.drms(profile):
            require(all('KEYFORMAT="com.apple.streamingkeydelivery"' in line for line in key_lines),
                    "roundtrip-fairplay-profile-invalid")
    return CONTRACT.drms(profile)


def proof(value, profile):
    result = CONTRACT.EVIDENCE.v2_proof(value)
    CONTRACT.native_proof(result, profile[0])
    return result


class Executor:
    def __init__(self, cli, root):
        self.cli, self.root = cli, root
        self.packager = shutil.which("packager")
        require(self.packager is not None and Path(self.packager).is_absolute(), "roundtrip-packager-unavailable")
        self.kid, self.key, self.iv = (secrets.token_hex(16) for _ in range(3))
        self.uri = "skd://release-roundtrip-fixture"
        self.cancel = threading.Event()
        self.decoder_guards = root / "decoder-guards"
        self.decoder_guards.mkdir(mode=0o700)
        for tool in ("ffmpeg", "ffprobe"):
            path = self.decoder_guards / (tool + (".exe" if os.name == "nt" else ""))
            # Windows CreateProcess refuses this invalid PE; POSIX executes a
            # fixed failing script. Neither path can invoke an actual decoder.
            payload = b"native-only-qualification\n" if os.name == "nt" else b"#!/bin/sh\nexit 91\n"
            SUPPORT.private_write(path, payload)
            path.chmod(0o700)
        self.sensitive = tuple(value.encode("ascii") for value in (self.key, self.iv, self.uri,
                                                                 "https://license.example.invalid/roundtrip"))

    def runner(self, root):
        runner = SUPPORT.Runner(root, self.sensitive, self.cancel)
        runner.environment = {name: value for name, value in runner.environment.items()
                              if not name.upper().startswith(("DRM_", "DRMR_"))}
        runner.environment["PATH"] = str(self.decoder_guards)
        return runner

    def check_decoder_guards(self, root):
        runner = self.runner(root)
        for tool in ("ffmpeg", "ffprobe"):
            try:
                runner.run([tool, "-version"], "decoder-guard", 5)
            except SUPPORT.Refusal as error:
                require(str(error) in ("command-failed", "command-unavailable"), "roundtrip-decoder-guard-invalid")
            else:
                raise CONTRACT.Failure("roundtrip-decoder-guard-invalid")

    def key_file(self, root, profile):
        value = {"schema_version": 1, "input": {"keys": [{"kid": self.kid, "key": self.key}],
                 "hls_keys": [{"uri": self.uri, "key": self.key, "iv": self.iv}]}}
        output = keys(profile, self.kid, self.key, self.iv, self.uri)
        if output is not None:
            value["output"] = output
        path = root / "keys.json"
        SUPPORT.private_write(path, json.dumps(value).encode("ascii"))
        return path

    def verify(self, runner, source, output, key_file, profile, stage):
        value = runner.json([self.cli, "verify", output, "--source", source, "--keys-file", key_file,
                             "--packager-bin", self.packager, "--max-workers", "1", "--json"], stage)
        return proof(value.get("verification", value), profile)

    def convert(self, source, profile, layout, root):
        root.mkdir(mode=0o700)
        runner = self.runner(root)
        key_file = self.key_file(root, profile)
        output = root / "output"
        if profile[0] == "mp4":
            output.mkdir(mode=0o700)
            destination = output / "asset.mp4"
        else:
            destination = output
        value = runner.json([self.cli, "convert", source, "--to", profile[1], "--format",
                             "hls" if profile[0] == "hls-fmp4" else profile[0], "--processing", "auto",
                             "--output", destination, "--keys-file", key_file, "--packager-bin", self.packager,
                             "--max-workers", "1", "--json"], "convert")
        path = destination if profile[0] == "mp4" else output / ("manifest.mpd" if profile[0] == "dash" else "master.m3u8")
        inspection = value.get("output", {})
        expected_format = "hls" if profile[0].startswith("hls-") else profile[0]
        require(inspection.get("format") == expected_format and inspection.get("protection") == profile[1],
                "roundtrip-output-profile-invalid")
        kinds = [track.get("kind") for track in inspection.get("tracks", [])]
        expected_kinds = ["video", "audio"] if CONTRACT.joint(layout) else ["audio"] if layout.startswith("aac") else ["video"]
        require(sorted(kinds) == sorted(expected_kinds), "roundtrip-track-layout-invalid")
        result = {"format": profile[0], "protection": profile[1], "tracks": expected_kinds,
                  "drms": manifest_profile(path, profile), "conversion": proof(value.get("verification"), profile),
                  "independent_verification": self.verify(runner, source, path, key_file, profile, "independent-verify"),
                  "output_graph": graph(output)}
        CONTRACT.leg(result, profile, layout)
        return path, result


def fixture_profiles(executor, seeds, root):
    profiles, reports = {}, []
    for layout in CONTRACT.LAYOUTS:
        for ordinal, profile in enumerate(CONTRACT.OUTPUTS):
            path, report = executor.convert(seeds[layout], profile, layout, root / f"{layout}-{ordinal}")
            profiles[(layout, profile)] = path
            rate = 44100 if "44100" in layout else 48000 if "48000" in layout else None
            reports.append({"id": f"{layout}:{CONTRACT.profile_id(profile)}", "graph": report["output_graph"],
                            "source_graph_sha256": graph(seeds[layout])["sha256"],
                            "verification": report["independent_verification"], "sample_rate": rate,
                            "final_presented_samples": 240 if rate else None})
        profile = ("hls-fmp4", "cenc")
        path = FIXTURES.cenc_hls(profiles[(layout, ("dash", "cenc"))], root / f"{layout}-cenc-hls",
                                 executor.uri, SUPPORT)
        operation = root / f"{layout}-cenc-hls-check"
        operation.mkdir(mode=0o700)
        runner = executor.runner(operation)
        key_file = executor.key_file(operation, profile)
        verified = executor.verify(runner, seeds[layout], path, key_file, profile, "input-only-fixture-verify")
        manifest_profile(path, profile)
        profiles[(layout, profile)] = path
        rate = 44100 if "44100" in layout else 48000 if "48000" in layout else None
        reports.append({"id": f"{layout}:{CONTRACT.profile_id(profile)}", "graph": graph(path),
                        "source_graph_sha256": graph(seeds[layout])["sha256"], "verification": verified,
                        "sample_rate": rate, "final_presented_samples": 240 if rate else None})
    return profiles, reports


def execute_case(case, executor, profiles, root):
    if case["kind"] == "layout-condition":
        return {**case, "status": "not-qualified"}
    source_profile, target_profile = tuple(case["source"]), tuple(case["target"])
    source = profiles[(case["layout"], source_profile)]
    before = graph(source)
    workspace = SUPPORT.Workspace(parent=root)
    try:
        require(not executor.cancel.is_set(), "roundtrip-cancelled")
        forward_path, forward = executor.convert(source, target_profile, case["layout"], workspace.path / "forward")
        result = {**case, "status": "complete", "forward": forward, "source_unchanged": True}
        if case["kind"] == "roundtrip":
            final_path, backward = executor.convert(forward_path, source_profile, case["layout"], workspace.path / "backward")
            operation = workspace.path / "final"
            operation.mkdir(mode=0o700)
            key_file = executor.key_file(operation, source_profile)
            result.update(backward=backward, final_against_original=executor.verify(
                executor.runner(operation), source, final_path, key_file, source_profile, "final-against-original"))
        require(graph(source) == before, "roundtrip-source-changed")
        return result
    except (CONTRACT.Failure, SUPPORT.Refusal, OSError, ValueError, TypeError, KeyError):
        return {**case, "status": "failed", "failure_code": "roundtrip-operation-failed"}
    finally:
        workspace.close()


def qualify(inputs, target, source_root, jobs):
    SUPPORT.require_native(target)
    require(not SUPPORT.VERIFY.command(["git", "status", "--porcelain", "--untracked-files=normal"], source_root).strip(),
            "roundtrip-clean-source-required")
    source = SUPPORT.VERIFY.Source(source_root)
    version, archive, archive_report = packet(inputs, source, target)
    SUPPORT.require_version(source, version)
    workspace = SUPPORT.Workspace()
    try:
        snapshot = workspace.path / "candidate.archive"
        SUPPORT.snapshot_archive(archive, snapshot)
        binary_hash = archive_report["binaries"]["drmr"]["sha256"]
        suffix = ".exe" if os.name == "nt" else ""
        cli = SUPPORT.extract_cli(snapshot, target, binary_hash, workspace.path / ("drmr" + suffix))
        runner = SUPPORT.Runner(workspace.path)
        require(runner.run([cli, "--version"], "version", 20).strip() == ("drmr " + version).encode("ascii"),
                "roundtrip-binary-version-invalid")
        seeds = FIXTURES.source_seeds(workspace.path, source.read(SUPPORT.CLIP), runner, SUPPORT)
        executor = Executor(cli, workspace.path)
        guard_check = workspace.path / "guard-check"
        guard_check.mkdir(mode=0o700)
        executor.check_decoder_guards(guard_check)
        cache = workspace.path / "profiles"
        cache.mkdir(mode=0o700)
        profiles, fixtures = fixture_profiles(executor, seeds, cache)
        cases = CONTRACT.cases()
        records = [None] * len(cases)
        with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
            futures = {pool.submit(execute_case, case, executor, profiles, workspace.path): index
                       for index, case in enumerate(cases)}
            try:
                for completed, future in enumerate(concurrent.futures.as_completed(futures), 1):
                    records[futures[future]] = future.result()
                    if completed % 24 == 0 or completed == len(cases):
                        print(f"Release matrix: {completed}/{len(cases)} records checked", flush=True)
            except BaseException:
                executor.cancel.set()
                for future in futures:
                    future.cancel()
        for future, index in futures.items():
            if records[index] is None:
                try:
                    records[index] = future.result()
                except concurrent.futures.CancelledError:
                    records[index] = {**cases[index], "status": "not-checked"}
                except BaseException:
                    records[index] = {**cases[index], "status": "failed", "failure_code": "roundtrip-worker-failed"}
        require(SUPPORT.VERIFY.file_digest(snapshot) == archive_report["archive_sha256"]
                and SUPPORT.VERIFY.file_digest(cli) == binary_hash, "roundtrip-artifact-changed")
        summary = {"expected": len(cases), "complete": sum(record["status"] == "complete" for record in records),
                   "failed": sum(record["status"] == "failed" for record in records),
                   "not_checked": sum(record["status"] == "not-checked" for record in records),
                   "not_qualified": sum(record["status"] == "not-qualified" for record in records), "kinds": CONTRACT.counts(cases)}
        result = {"schema_version": 1, "kind": "native-cli-release-roundtrip",
                  "success": summary["failed"] == 0 and summary["not_checked"] == 0 and not executor.cancel.is_set(),
                  "source_head": source.head, "source_tree": source.tree, "target": target, "version": version,
                  "archive_sha256": archive_report["archive_sha256"], "binary_sha256": binary_hash,
                  "binary_header_target": target, "binary_reported_version": version,
                  "fixture_sha256": SUPPORT.CLIP_SHA256, "matrix_sha256": CONTRACT.matrix_digest(),
                  "native_ordinary_user": True, "required_depth": "native", "decode_evidence": "not-checked",
                  "license_playback_verified": False, "fixture_metadata": CONTRACT.FIXTURE_METADATA,
                  "fixture_profiles": fixtures, "records": records,
                  "counts": summary, "owned_workspace_cleanup": "confirmed", "limits": CONTRACT.LIMITS}
        if result["success"]:
            CONTRACT.validate(result, source.head, source.tree, target, version, archive_report["archive_sha256"], binary_hash)
        return result
    finally:
        workspace.close()


def main(argv=None):
    parser = SUPPORT.Parser(description=__doc__)
    parser.add_argument("--inputs", type=Path, required=True, help="Checked native-input packet, never source binaries")
    parser.add_argument("--target", required=True)
    parser.add_argument("--source", type=Path, default=CONTRACT.ROOT)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=max(1, min(4, (os.cpu_count() or 2) // 2)))
    started = time.monotonic()
    report_path = None
    try:
        args = parser.parse_args(argv)
        require(1 <= args.jobs <= 8, "roundtrip-worker-count-invalid")
        require(not args.report.exists() and not args.report.is_symlink()
                and not args.report.resolve().is_relative_to(args.source.resolve()), "roundtrip-report-destination-invalid")
        report_path = args.report
        result = qualify(args.inputs.resolve(), args.target, args.source.resolve(), args.jobs)
    except (CONTRACT.Failure, SUPPORT.Refusal, SUPPORT.VERIFY.Failure, OSError, ValueError, TypeError, KeyError):
        result = {"schema_version": 1, "kind": "native-cli-release-roundtrip", "success": False,
                  "failure_code": "roundtrip-qualification-failed"}
    except KeyboardInterrupt:
        result = {"schema_version": 1, "kind": "native-cli-release-roundtrip", "success": False,
                  "failure_code": "roundtrip-qualification-interrupted"}
    if report_path is not None:
        try:
            SUPPORT.private_write(report_path, (json.dumps(result, indent=2, sort_keys=True) + "\n").encode("ascii"))
        except (OSError, SUPPORT.Refusal):
            print('{"success":false,"failure_code":"roundtrip-report-unavailable"}')
            return 1
    print(json.dumps({"success": result["success"], "elapsed_seconds": round(time.monotonic() - started, 1)}))
    return 0 if result["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
