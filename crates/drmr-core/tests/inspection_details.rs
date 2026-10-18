//! Metadata facts stay distinct from sample evidence and conditional processing routes.
use drmr_core::inspection::{self, Inspection, OutputFormat, TargetProtection};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}
fn json(path: &Path) -> Value {
    serde_json::to_value(inspection::inspect(path).unwrap()).unwrap()
}
#[test]
fn container_defaults_never_claim_full_or_subsample_encryption() {
    for name in ["cenc-iv8.mp4", "cenc-iv16.mp4"] {
        let report = json(&fixture(name));
        let descriptions = report["details"]["track_protection"].as_array().unwrap();
        assert_eq!(descriptions.len(), 2);
        for item in descriptions {
            assert_eq!(item["evidence"], "container-default");
            assert_eq!(item["cipher"], "aes-128-ctr");
            assert_eq!(item["encryption_unit"], "media-sample");
            assert_eq!(item["sample_coverage"], "unknown");
            assert_eq!(
                item["sample_coverage_reason"],
                "sample-auxiliary-data-not-inspected"
            );
            assert_eq!(item["pattern"]["status"], "known");
        }
        assert_eq!(report["details"]["periods"]["status"], "not-applicable");
        assert_eq!(report["media_verified"], false);
    }
}
#[test]
fn manifest_cipher_is_a_declaration_and_periods_are_only_direct_children() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.mpd");
    std::fs::write(&path, r#"<MPD><Period><AdaptationSet contentType="video"><ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cbcs"/><Representation codecs="avc1.64001f"/></AdaptationSet></Period><Period/></MPD>"#).unwrap();
    let report = json(&path);
    let item = &report["details"]["track_protection"][0];
    assert_eq!(item["evidence"], "manifest-declaration");
    assert_eq!(item["cipher"], "aes-128-cbc");
    assert_eq!(item["sample_coverage"], "unknown");
    assert_eq!(item["pattern"]["status"], "unknown");
    assert_eq!(
        report["details"]["periods"],
        serde_json::json!({"status":"known", "count":2})
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
#[test]
fn hls_resource_and_sample_declarations_have_different_units() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.m3u8");
    for (method, cipher, unit, coverage) in [
        ("AES-128", "aes-128-cbc", "hls-resource", "not-applicable"),
        ("SAMPLE-AES", "aes-128-cbc", "media-sample", "unknown"),
        ("SAMPLE-AES-CTR", "aes-128-ctr", "media-sample", "unknown"),
    ] {
        std::fs::write(&path, format!("#EXTM3U\n#EXT-X-KEY:METHOD={method},URI=\"https://example.test/key?private-sentinel\"\n#EXTINF:2,\nmedia.ts\n#EXT-X-ENDLIST\n")).unwrap();
        let report = json(&path);
        let item = &report["details"]["track_protection"][0];
        assert_eq!(item["cipher"], cipher);
        assert_eq!(item["encryption_unit"], unit);
        assert_eq!(item["sample_coverage"], coverage);
        assert_eq!(item["hls_key_methods"], serde_json::json!([method]));
        assert_eq!(report["details"]["periods"]["status"], "not-applicable");
        assert!(!report.to_string().contains("private-sentinel"));
        if method != "AES-128" {
            assert_eq!(report["protection"], "unknown");
        }
    }
}
#[test]
fn hls_method_evidence_stays_with_each_child_and_unknown_methods_are_fixed_tokens() {
    let dir = tempfile::tempdir().unwrap();
    for (name, method) in [
        ("sample", "SAMPLE-AES"),
        ("unknown", "private-method-sentinel"),
    ] {
        std::fs::write(
            dir.path().join(format!("{name}.m3u8")),
            format!("#EXTM3U\n#EXT-X-KEY:METHOD={method},URI=\"private-key-sentinel\"\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n"),
        ).unwrap();
    }
    let master = dir.path().join("master.m3u8");
    std::fs::write(&master, "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nsample.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=200\nunknown.m3u8\n").unwrap();
    let report = json(&master);
    let descriptions = report["details"]["track_protection"].as_array().unwrap();
    assert_eq!(descriptions.len(), 2);
    assert_eq!(
        descriptions[0]["hls_key_methods"],
        serde_json::json!(["SAMPLE-AES"])
    );
    assert_eq!(
        descriptions[1]["hls_key_methods"],
        serde_json::json!(["UNKNOWN"])
    );
    for (track, description) in report["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(descriptions)
    {
        assert_eq!(track["id"], description["track_id"]);
        assert_eq!(track["protection"], "unknown");
        assert_eq!(description["evidence"], "manifest-declaration");
    }
    assert!(!report.to_string().contains("private-"));
    let plan = inspection::plan(
        serde_json::from_value(report).unwrap(),
        TargetProtection::Clear,
        vec![OutputFormat::Hls],
    );
    assert!(!plan.compatible && !plan.execution_available);
    assert_eq!(plan.processing.unwrap().candidates, []);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
}

#[test]
fn only_correlated_sample_aes_declarations_enable_the_conditional_ts_route() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.m3u8");
    for method in ["SAMPLE-AES", "SAMPLE-AES-CTR", "private-method-sentinel"] {
        std::fs::write(&path, format!("#EXTM3U\n#EXT-X-KEY:METHOD={method},URI=\"not-read-key\"\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n")).unwrap();
        let input = inspection::inspect(&path).unwrap();
        let plan = inspection::plan(
            input.clone(),
            TargetProtection::Clear,
            vec![OutputFormat::Hls],
        );
        if method == "SAMPLE-AES" {
            assert!(plan.compatible && plan.execution_available);
            assert_eq!(plan.input.protection, inspection::Protection::Unknown);
            assert!(!plan.input.media_verified);
            assert!(
                plan.findings
                    .iter()
                    .any(|f| f.code == "source-protection-unresolved")
            );
            let routes = plan.processing.unwrap().candidates;
            assert_eq!(routes.len(), 2);
            let ts = routes
                .iter()
                .find(|r| r.route.as_str() == "mpeg-ts-sample-aes-hls")
                .unwrap();
            assert!(ts.revalidation_required);
            assert!(
                ts.condition_codes
                    .iter()
                    .any(|c| c == "sample-aes-transport-profile-must-pass")
            );
            assert!(!routes.iter().any(|r| r.route.as_str() == "mpeg-ts-hls"));
            for missing_correlation in [false, true] {
                let mut input = input.clone();
                if missing_correlation {
                    input.details.as_mut().unwrap().track_protection[0].track_id =
                        Some("unrelated".into());
                } else {
                    input.details = None;
                }
                let untrusted =
                    inspection::plan(input, TargetProtection::Clear, vec![OutputFormat::Hls]);
                assert!(!untrusted.compatible && !untrusted.execution_available);
                assert_eq!(untrusted.processing.unwrap().candidates, []);
            }
        } else {
            assert!(!plan.compatible && !plan.execution_available);
            assert_eq!(plan.processing.unwrap().candidates, []);
        }
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn older_track_details_without_hls_methods_read_without_inventing_declarations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.m3u8");
    std::fs::write(&path, "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"not-read\"\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n").unwrap();
    let mut report = json(&path);
    report["details"]["track_protection"][0]
        .as_object_mut()
        .unwrap()
        .remove("hls_key_methods");
    let older: Inspection = serde_json::from_value(report.clone()).unwrap();
    assert_eq!(serde_json::to_value(older).unwrap(), report);
    let mp4 = json(&fixture("clip.mp4"));
    assert!(
        mp4["details"]["track_protection"][0]
            .get("hls_key_methods")
            .is_none()
    );
}
#[test]
fn older_reports_without_details_still_read_and_do_not_invent_observations() {
    let mut value = json(&fixture("clip.mp4"));
    value.as_object_mut().unwrap().remove("details");
    let old: Inspection = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(old).unwrap(), value);
}
#[test]
fn finite_candidates_show_tool_requirements_without_claiming_selection() {
    let input = inspection::inspect(&fixture("cenc-iv8.mp4")).unwrap();
    let plan = serde_json::to_value(inspection::plan(
        input,
        TargetProtection::Clear,
        vec![OutputFormat::Mp4],
    ))
    .unwrap();
    assert_eq!(plan["processing"]["status"], "metadata-only");
    let candidates = plan["processing"]["candidates"].as_array().unwrap();
    let progressive = candidates
        .iter()
        .find(|c| c["route"] == "progressive-mp4")
        .unwrap();
    assert_eq!(progressive["revalidation_required"], true);
    assert_eq!(
        progressive["required_tools"],
        serde_json::json!(["ffmpeg", "ffprobe"])
    );
    assert_eq!(plan["input"]["media_verified"], false);
}

#[test]
fn unknown_periods_and_unknown_sample_defaults_are_not_not_applicable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.mpd");
    std::fs::write(&path, "<MPD><p:Period xmlns:p=\"urn:unsupported\"/></MPD>").unwrap();
    let report = json(&path);
    assert_eq!(report["details"]["periods"]["status"], "unknown");
    assert!(report["details"]["periods"].get("count").is_none());
    std::fs::write(&path, "<MPD><Period/></MPD>").unwrap();
    assert_eq!(json(&path)["details"]["periods"]["count"], 1);
    std::fs::write(&path, "<MPD/>").unwrap();
    assert_eq!(json(&path)["details"]["periods"]["count"], 0);
    for invalid in [
        "<MPD><Wrapper><Period/></Wrapper></MPD>",
        "<MPD><Wrapper><p:Period xmlns:p=\"urn:unsupported\"/></Wrapper></MPD>",
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert_eq!(json(&path)["details"]["periods"]["status"], "unknown");
        assert!(json(&path)["details"]["periods"].get("count").is_none());
    }
}

#[test]
fn hls_candidates_leave_actual_media_format_and_tool_availability_unresolved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.m3u8");
    std::fs::write(&path, "#EXTM3U\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n").unwrap();
    let report = inspection::plan(
        inspection::inspect(&path).unwrap(),
        TargetProtection::Clear,
        vec![OutputFormat::Hls],
    );
    let mut value = serde_json::to_value(&report).unwrap();
    let candidates = value["processing"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .all(|item| item["revalidation_required"] == true)
    );
    assert!(candidates.iter().any(|item| item["route"] == "mpeg-ts-hls"));
    assert!(
        candidates.iter().all(|item| item["required_tools"]
            == serde_json::json!(["ffmpeg", "ffprobe", "shaka-packager"]))
    );
    value.as_object_mut().unwrap().remove("processing");
    value["input"].as_object_mut().unwrap().remove("details");
    let old: inspection::ConversionPlan = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(old).unwrap(), value);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn child_playlist_details_keep_the_synthetic_track_identity_without_exposing_references() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("child.m3u8"), "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"secret-key\"\n#EXTINF:2,\nsecret-media\n#EXT-X-ENDLIST\n").unwrap();
    let path = dir.path().join("master.m3u8");
    std::fs::write(
        &path,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nchild.m3u8\n",
    )
    .unwrap();
    let report = json(&path);
    assert_eq!(
        report["details"]["track_protection"][0]["track_id"],
        report["tracks"][0]["id"]
    );
    assert_eq!(
        report["details"]["track_protection"][0]["cipher"],
        "aes-128-cbc"
    );
    assert!(!report.to_string().contains("secret-"));
}

#[test]
fn the_original_version_one_json_shape_keeps_its_values() {
    let original = serde_json::json!({"schema_version":1,"format":"mp4","evidence":"container-metadata","protection":"clear","tracks":[{"id":"sample-description-1","kind":"video","codec":"avc1","protection":"clear","key_ids":[],"pattern":null}],"drm_systems":[],"findings":[],"media_verified":false});
    let input: Inspection = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(&input).unwrap(), original);
    let mut plan = serde_json::to_value(inspection::plan(
        input,
        TargetProtection::Cbcs,
        vec![OutputFormat::Mp4],
    ))
    .unwrap();
    plan.as_object_mut().unwrap().remove("processing");
    assert_eq!(
        plan,
        serde_json::json!({"schema_version":1,"input":original,"target":"cbcs","formats":["mp4"],"operations":["validate-source","repackage-media","encrypt-output","verify-output"],"keys":[{"role":"output","known_key_ids":[]}],"compatible":true,"execution_available":true,"findings":[{"code":"media-validation-required"},{"code":"source-profile-validation-required"}]})
    );
}

#[test]
fn known_text_roles_have_no_executable_candidates_but_keep_version_one_intent_fields() {
    use inspection::{Evidence, InputFormat, Protection, Track, TrackKind};
    for codec in ["wvtt", "stpp"] {
        for format in [OutputFormat::Mp4, OutputFormat::Dash, OutputFormat::Hls] {
            let report = inspection::plan(
                Inspection::new(
                    InputFormat::Mp4,
                    Evidence::ContainerMetadata,
                    vec![Track {
                        id: "text-1".into(),
                        kind: TrackKind::Text,
                        codec: Some(codec.into()),
                        protection: Protection::Clear,
                        key_ids: vec![],
                        pattern: None,
                    }],
                    vec![],
                    vec![],
                ),
                TargetProtection::Clear,
                vec![format],
            );
            assert!(
                report.compatible && report.execution_available,
                "existing intent fields must remain unchanged"
            );
            assert!(
                report.processing.unwrap().candidates.is_empty(),
                "{codec} -> {format:?} must not advertise an impossible media handler"
            );
        }
    }
    let mixed = Inspection::new(
        InputFormat::Mp4,
        Evidence::ContainerMetadata,
        vec![
            Track {
                id: "video".into(),
                kind: TrackKind::Video,
                codec: Some("avc1".into()),
                protection: Protection::Clear,
                key_ids: vec![],
                pattern: None,
            },
            Track {
                id: "text".into(),
                kind: TrackKind::Text,
                codec: Some("wvtt".into()),
                protection: Protection::Clear,
                key_ids: vec![],
                pattern: None,
            },
        ],
        vec![],
        vec![],
    );
    let report = inspection::plan(mixed, TargetProtection::Clear, vec![OutputFormat::Dash]);
    assert!(report.compatible && report.execution_available);
    assert!(
        report.processing.unwrap().candidates.is_empty(),
        "the whole source includes a refused text track"
    );
    let root = tempfile::tempdir().unwrap();
    let playlist = root.path().join("unknown.m3u8");
    std::fs::write(
        &playlist,
        "#EXTM3U\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    let input = inspection::inspect(&playlist).unwrap();
    assert_eq!(input.tracks[0].kind, TrackKind::Unknown);
    assert_eq!(
        inspection::plan(input, TargetProtection::Clear, vec![OutputFormat::Hls])
            .processing
            .unwrap()
            .candidates
            .len(),
        2,
        "unknown HLS roles still require actual resource admission"
    );
}
