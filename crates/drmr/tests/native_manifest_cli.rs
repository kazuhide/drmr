//! Native manifest-only checks require neither media nor installed validator processes.
use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Output};

fn native(input: &Path, empty_path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("verify")
        .arg(input)
        .args(["--manifest-only", "--json"])
        .env("PATH", empty_path)
        .output()
        .unwrap()
}

fn safe_report(output: &Output) -> Value {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("confidential-native-sentinel"));
    }
    serde_json::from_slice(&output.stdout).unwrap()
}

fn media(version: u32) -> String {
    format!(
        concat!(
            "#EXTM3U\n#EXT-X-VERSION:{}\n#EXT-X-TARGETDURATION:2\n",
            "#EXT-X-MAP:URI=\"missing-init.mp4\"\n",
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://confidential-native-sentinel\",",
            "KEYFORMAT=\"com.apple.streamingkeydelivery\"\n",
            "#EXTINF:2.0,\nmissing-segment.m4s\n#EXT-X-ENDLIST\n"
        ),
        version
    )
}

#[test]
fn native_default_checks_all_33_playlists_without_media_keys_or_external_tools() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let input = root.path().join("playlist.data");
    let mut master = "#EXTM3U\n".to_owned();
    for ordinal in 0..32 {
        let name = format!("variant-{ordinal:02}.list");
        writeln!(
            master,
            "#EXT-X-STREAM-INF:BANDWIDTH={}\n{name}",
            64000 + ordinal
        )
        .unwrap();
        std::fs::write(root.path().join(name), media(6)).unwrap();
    }
    std::fs::write(&input, &master).unwrap();
    let output = native(&input, &empty_path);
    let report = safe_report(&output);
    assert!(
        output.status.success(),
        "native checks failed: {}",
        report["error"]["code"]
    );
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["passed"], true);
    assert_eq!(report["scope"], "manifest-only");
    assert_eq!(report["media_verified"], false);
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(report["native_manifest"]["document_count"], 33);
    assert_eq!(report["native_manifest"]["scope"], "manifest-only");
    assert_eq!(report["native_manifest"]["errors"], 0);
    assert_eq!(report["native_manifest"]["incomplete"], false);
    assert_eq!(report["native_manifest"]["truncated"], false);
    assert!(report["external_manifest"].is_null());
    assert_eq!(std::fs::read_to_string(input).unwrap(), master);
    assert!(!root.path().join("missing-init.mp4").exists());
    assert!(!root.path().join("missing-segment.m4s").exists());
}

#[test]
fn native_version_findings_use_the_required_minimum_and_do_not_echo_key_identifiers() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let input = root.path().join("media.m3u8");
    for version in [5, 6, 7] {
        let source = media(version);
        std::fs::write(&input, &source).unwrap();
        let result = native(&input, &empty_path);
        let report = safe_report(&result);
        assert_eq!(result.status.code(), Some(i32::from(version == 5)));
        assert_eq!(report["passed"], version >= 6);
        assert_eq!(report["media_verified"], false);
        assert!(report["external_manifest"].is_null());
        if version == 5 {
            assert!(
                report["native_manifest"]["errors"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
            );
            assert!(
                report["native_manifest"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
            );
        }
        assert_eq!(std::fs::read_to_string(&input).unwrap(), source);
    }
}

#[test]
fn native_manifest_workers_are_bounded_and_media_arguments_stay_disallowed() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("media.m3u8");
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    std::fs::write(&input, media(6)).unwrap();
    for workers in ["1", "2", "64"] {
        let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
            .arg("verify")
            .arg(&input)
            .args(["--manifest-only", "--json", "--max-workers", workers])
            .env("PATH", &empty_path)
            .output()
            .unwrap();
        let report = safe_report(&output);
        assert!(output.status.success(), "{report}");
        assert!(
            report["native_manifest"]["workers_used"]
                .as_u64()
                .is_some_and(|used| used >= 1 && used <= workers.parse::<u64>().unwrap())
        );
    }
    for arguments in [
        vec!["--max-workers", "0"],
        vec!["--max-workers", "65"],
        vec!["--source", "private-source"],
        vec!["--keys-file", "private-key-file"],
        vec!["--plan", "private-plan"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
            .arg("verify")
            .arg(&input)
            .args(["--manifest-only", "--json"])
            .args(arguments)
            .env("PATH", &empty_path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
fn external_validation_requires_its_explicit_flag_and_failure_retains_native_evidence() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("media.m3u8");
    let absent = root.path().join("absent-validator");
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    std::fs::write(&input, media(6)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("verify")
        .arg(&input)
        .args(["--manifest-only", "--json", "--hls-validator-bin"])
        .arg(&absent)
        .env("PATH", &empty_path)
        .output()
        .unwrap();
    let report = safe_report(&output);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(report["error"]["code"], "external-validator-required");
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("verify")
        .arg(&input)
        .args([
            "--manifest-only",
            "--external-validator",
            "--json",
            "--hls-validator-bin",
        ])
        .arg(&absent)
        .env("PATH", &empty_path)
        .output()
        .unwrap();
    let report = safe_report(&output);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["passed"], false);
    assert_eq!(report["native_manifest"]["errors"], 0);
    assert_eq!(report["native_manifest"]["incomplete"], false);
    assert_eq!(report["external_requested"], true);
    assert_eq!(report["external_error"], "manifest-validator-unavailable");
    assert!(report["external_manifest"].is_null());
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("verify")
        .arg(&input)
        .args(["--external-validator", "--json"])
        .env("PATH", &empty_path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("verify")
        .arg(&input)
        .args(["--external-validator", "--source"])
        .arg(&input)
        .args(["--json"])
        .env("PATH", &empty_path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

const DASH: &str = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" profiles="urn:mpeg:dash:profile:isoff-live:2011" minBufferTime="PT1S" mediaPresentationDuration="PT2S"><Period><AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f"><SegmentTemplate timescale="1000" media="seg-$Number$.m4s" initialization="init.mp4"><SegmentTimeline><S t="0" d="2000"/></SegmentTimeline></SegmentTemplate><Representation id="v1" bandwidth="1000000"/></AdaptationSet></Period></MPD>"#;

#[test]
fn native_dash_checks_semantics_and_never_resolves_media_or_xml_entities() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let input = root.path().join("manifest.data");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!(
        "http://{}/confidential-native-sentinel",
        listener.local_addr().unwrap()
    );
    let remote = DASH
        .replace("seg-$Number$.m4s", &format!("{origin}/$Number$.m4s"))
        .replace("init.mp4", &format!("{origin}/init.mp4"));
    for (source, passed) in [
        (remote.clone(), true),
        (
            remote.replace("timescale=\"1000\"", "timescale=\"0\""),
            false,
        ),
        (
            format!("<!DOCTYPE MPD [<!ENTITY remote SYSTEM '{origin}'>]>{remote}"),
            false,
        ),
    ] {
        std::fs::write(&input, &source).unwrap();
        let output = native(&input, &empty_path);
        let report = safe_report(&output);
        assert_eq!(output.status.code(), Some(i32::from(!passed)), "{report}");
        assert_eq!(report["passed"], passed);
        assert_eq!(report["native_manifest"]["document_count"], 1);
        assert_eq!(report["native_manifest"]["profile"], "dash-static-manifest");
        assert_eq!(std::fs::read_to_string(&input).unwrap(), source);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn native_hls_never_fetches_media_initialization_or_key_urls() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let input = root.path().join("media.m3u8");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!(
        "http://{}/confidential-native-sentinel",
        listener.local_addr().unwrap()
    );
    let source = media(6)
        .replace("missing-init.mp4", &format!("{origin}/init.mp4"))
        .replace("missing-segment.m4s", &format!("{origin}/segment.m4s"))
        .replace(
            "skd://confidential-native-sentinel",
            &format!("{origin}/key"),
        );
    std::fs::write(&input, &source).unwrap();
    let output = native(&input, &empty_path);
    let report = safe_report(&output);
    assert!(output.status.success(), "{report}");
    assert_eq!(report["native_manifest"]["document_count"], 1);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(std::fs::read_to_string(input).unwrap(), source);
}

#[test]
fn plaintext_explains_incomplete_native_rules_without_echoing_opaque_input_or_outside_scope() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let input = root.path().join("confidential-native-sentinel.data");
    for incomplete in [false, true] {
        let source = if incomplete {
            DASH.replacen(
                "<MPD ",
                "<MPD confidential-native-sentinel=\"confidential-native-sentinel\" ",
                1,
            )
        } else {
            DASH.to_owned()
        };
        std::fs::write(&input, &source).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
            .arg("verify")
            .arg(&input)
            .arg("--manifest-only")
            .env("PATH", &empty_path)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(i32::from(incomplete)));
        let text = String::from_utf8(result.stdout).unwrap();
        assert_eq!(
            text.contains("DASH-ATTRIBUTE-COVERAGE"),
            incomplete,
            "plaintext should identify an incomplete in-scope rule"
        );
        assert_eq!(text.contains("unsupported-feature"), incomplete);
        for bytes in [text.as_bytes(), result.stderr.as_slice()] {
            let text = String::from_utf8_lossy(bytes);
            for excluded in [
                "confidential-native-sentinel",
                "DASH-FULL-XSD-SCHEMATRON",
                "DASH-MEDIA-EVIDENCE",
            ] {
                assert!(!text.contains(excluded));
            }
        }
        assert_eq!(std::fs::read_to_string(&input).unwrap(), source);
    }
}

#[test]
fn plaintext_incomplete_summaries_are_bounded_without_truncating_json_evidence() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let input = root.path().join("master.m3u8");
    let mut master = String::from("#EXTM3U\n");
    for ordinal in 0..40 {
        writeln!(
            master,
            "#EXT-X-STREAM-INF:BANDWIDTH={}\nvariant-{ordinal}.m3u8",
            64000 + ordinal
        )
        .unwrap();
        let source = media(6).replace(
            "missing-init.mp4\"",
            "missing-init.mp4\",CONFIDENTIAL-NATIVE-SENTINEL=\"opaque\"",
        );
        std::fs::write(root.path().join(format!("variant-{ordinal}.m3u8")), source).unwrap();
    }
    std::fs::write(&input, master).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("verify")
        .arg(&input)
        .arg("--manifest-only")
        .env("PATH", &empty_path)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let text = String::from_utf8(result.stdout).unwrap();
    assert_eq!(
        text.matches("HLS-UNKNOWN-ATTRIBUTES: not checked").count(),
        32
    );
    assert!(text.contains("8 additional incomplete native rule summaries"));
    assert!(!text.contains("CONFIDENTIAL-NATIVE-SENTINEL"));
    assert!(!String::from_utf8_lossy(&result.stderr).contains("CONFIDENTIAL-NATIVE-SENTINEL"));
    let report = safe_report(&native(&input, &empty_path));
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("CONFIDENTIAL-NATIVE-SENTINEL")
    );
    assert_eq!(report["passed"], false);
    assert_eq!(report["native_manifest"]["document_count"], 41);
    assert_eq!(report["native_manifest"]["truncated"], false);
    assert_eq!(
        report["native_manifest"]["coverage"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["rule_id"] == "HLS-UNKNOWN-ATTRIBUTES"
                && item["status"]["status"] == "not-checked")
            .count(),
        40
    );
}

fn authoring_fixture(root: &Path, variants: usize) -> std::path::PathBuf {
    let mut master = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
    for ordinal in 0..variants {
        writeln!(master, "#EXT-X-STREAM-INF:BANDWIDTH={},AVERAGE-BANDWIDTH={},CODECS=\"avc1.4d401e\",RESOLUTION={}x{},FRAME-RATE=30.000\nvariant-{ordinal}.m3u8", 1_000_000 + ordinal * 500_000, 900_000 + ordinal * 500_000, 320 + ordinal * 320, 180 + ordinal * 180).unwrap();
        std::fs::write(
            root.join(format!("variant-{ordinal}.m3u8")),
            media(7).replace(
                "#EXT-X-TARGETDURATION:2\n",
                "#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n",
            ),
        )
        .unwrap();
    }
    let input = root.join("master.m3u8");
    std::fs::write(&input, master).unwrap();
    input
}
fn profile_command(input: &Path, empty_path: &Path, profile: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command
        .arg("verify")
        .arg(input)
        .args([
            "--manifest-only",
            "--manifest-profile",
            profile,
            "--json",
            "--max-workers",
            "2",
        ])
        .env("PATH", empty_path);
    command
}

#[test]
fn apple_authoring_profile_is_explicit_and_preserves_the_multiple_video_bitrate_requirement() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    for variants in [2, 1] {
        let input = authoring_fixture(root.path(), variants);
        let base = native(&input, &empty_path);
        assert!(base.status.success());
        assert_eq!(
            safe_report(&base)["native_manifest"]["profile"],
            "hls-base-manifest"
        );
        let explicit = profile_command(&input, &empty_path, "base")
            .output()
            .unwrap();
        assert!(
            explicit.status.success(),
            "the explicit native profile option must be supported"
        );
        assert_eq!(
            safe_report(&explicit)["native_manifest"]["profile"],
            "hls-base-manifest"
        );
        let apple = profile_command(&input, &empty_path, "apple-vod")
            .output()
            .unwrap();
        assert_eq!(
            apple.status.code(),
            Some(i32::from(variants == 1)),
            "the authoring profile must be executable without external tools"
        );
        let report = safe_report(&apple);
        assert_eq!(report["native_manifest"]["profile"], "apple-vod-manifest");
        assert_eq!(report["native_manifest"]["document_count"], variants + 1);
        assert_eq!(report["passed"], variants > 1);
        assert!(report["external_manifest"].is_null());
        if variants == 1 {
            assert!(
                report["native_manifest"]["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|issue| issue["rule_id"] == "APPLE-HLS-VARIANTS")
            );
            assert_eq!(report["error"]["code"], "native-manifest-lint-failed");
        }
    }
}

#[test]
fn apple_profile_rejects_dash_and_profiles_require_manifest_only_scope() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let input = root.path().join("manifest.mpd");
    std::fs::write(&input, DASH).unwrap();
    let dash = profile_command(&input, &empty_path, "apple-vod")
        .output()
        .unwrap();
    assert_eq!(dash.status.code(), Some(2));
    assert_eq!(
        safe_report(&dash)["error"]["code"],
        "manifest-lint-profile-unsupported"
    );
    for profile in ["base", "apple-vod"] {
        let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
            .arg("verify")
            .arg(&input)
            .args(["--manifest-profile", profile, "--source"])
            .arg(&input)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
    }
}

#[cfg(unix)]
#[test]
fn apple_profile_and_explicit_external_evidence_are_reported_separately() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt as _;
    let root = drmr_core::native::private_tempdir().unwrap();
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let input = authoring_fixture(root.path(), 2);
    let marker = root.path().join("calls");
    let media = json!({"playlistKind":"media","fetchCount":1,"parsedSegmentsCount":1,"processedSegmentsCount":0,"hasEndTag":true,"discontinuities":[{"segments":[{"processed":false}]}]});
    let mut child = media.clone();
    child["dataVersion"] = json!(1.2);
    child["validatorName"] = "mediastreamvalidator".into();
    child["validatorVersion"] = "fixture".into();
    child["url"] = "%s".into();
    let parent = json!({"dataVersion":1.2,"validatorName":"mediastreamvalidator","validatorVersion":"fixture","url":"%s","playlistKind":"multivariant","variants":[media.clone(),media]});
    let validator = root.path().join("validator");
    let quoted = marker.to_str().unwrap().replace('\'', "'\\''");
    std::fs::write(&validator, format!("#!/bin/sh\nprintf '.\\n' >> '{quoted}'\ncase \"$6\" in\n*/master.m3u8) printf '{parent}' \"$6\";;\n*) printf '{child}' \"$6\";;\nesac\n")).unwrap();
    std::fs::set_permissions(&validator, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = profile_command(&input, &empty_path, "apple-vod")
        .args(["--external-validator", "--hls-validator-bin"])
        .arg(&validator)
        .output()
        .unwrap();
    let report = safe_report(&result);
    assert!(result.status.success());
    assert_eq!(report["native_manifest"]["profile"], "apple-vod-manifest");
    assert_eq!(report["native_manifest"]["errors"], 0);
    assert_eq!(
        report["external_manifest"]["checks"][0]["checked_manifest_count"],
        3
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap().lines().count(), 3);
}
