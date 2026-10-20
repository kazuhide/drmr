use super::*;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};

fn playlist(version: u32, extra: &str) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:{version}\n#EXT-X-TARGETDURATION:2\n{extra}#EXTINF:2.0,\nsegment.m4s\n#EXT-X-ENDLIST\n"
    )
}

fn expected() -> hls::Manifest {
    hls::Manifest {
        original: PathBuf::from("media.m3u8"),
        relative: PathBuf::from("media.m3u8"),
        bytes: playlist(7, "#EXT-X-MAP:URI=\"init.mp4\"\n").into_bytes(),
        version: Some(7),
        minimum: 6,
        media: true,
        segments: 1,
    }
}

// Shape observed from mediastreamvalidator 1.25.36 in -p mode. Public fixtures
// retain only the fields that establish parser identity, completion, and scope.
fn apple_report() -> Value {
    json!({
        "dataVersion": 1.2, "validatorName": "mediastreamvalidator",
        "validatorVersion": "1.25.36 (1.25.36.18b-250603)",
        "url": "media.m3u8", "playlistKind": "media", "fetchCount": 1,
        "parsedSegmentsCount": 1, "processedSegmentsCount": 0, "hasEndTag": true,
        "discontinuities": [{"segments":[{"processed":false}]}]
    })
}

fn check_apple(value: &Value) -> Result<Option<String>, ConversionError> {
    apple::validate(
        &serde_json::to_vec(value).unwrap(),
        &expected(),
        "media.m3u8",
    )
}

fn dash_report() -> Value {
    json!({
        "kind":"drmr-dash-manifest-validation", "schema_version":1,
        "scope":"manifest-only", "engine":"dash-if",
        "engine_revision":"715d8641872566d644b63ba6eaeb5a6afc03890b",
        "manifest_sha256":digest(b"manifest"), "passed":true,
        "checks":{
            "xsd":{"completed":true,"passed":true,"issue_count":0},
            "schematron":{"completed":true,"passed":true,"issue_count":0}
        }
    })
}

fn check_dash(value: &Value) -> Result<Option<String>, ConversionError> {
    dash::validate(&serde_json::to_vec(value).unwrap(), &digest(b"manifest"))
}

#[test]
fn ordinary_fmp4_map_requires_at_least_six_and_accepts_seven() {
    let map = "#EXT-X-MAP:URI=\"init.mp4\"\n";
    assert_eq!(
        hls::parse(&playlist(5, map)).unwrap_err().0,
        "hls-protocol-version-too-low"
    );
    for version in [6, 7] {
        let parsed = hls::parse(&playlist(version, map)).unwrap();
        assert_eq!(parsed.minimum, 6);
        assert_eq!(parsed.version, Some(version));
    }
}

#[test]
fn iframe_map_requires_five_and_fairplay_ts_also_requires_five() {
    let iframe = "#EXT-X-I-FRAMES-ONLY\n#EXT-X-MAP:URI=\"init.mp4\"\n";
    let fairplay = "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://example.invalid/asset\",KEYFORMAT=\"com.apple.streamingkeydelivery\",KEYFORMATVERSIONS=\"1\"\n";
    for extra in [iframe, fairplay] {
        assert!(hls::parse(&playlist(4, extra)).is_err());
        assert_eq!(hls::parse(&playlist(5, extra)).unwrap().minimum, 5);
    }
}

#[test]
fn floating_duration_and_explicit_iv_raise_the_minimum() {
    assert!(hls::parse(&playlist(2, "")).is_err());
    assert_eq!(hls::parse(&playlist(3, "")).unwrap().minimum, 3);
    let integer = playlist(
        2,
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x00112233445566778899aabbccddeeff\n",
    )
    .replace("2.0,", "2,");
    assert_eq!(hls::parse(&integer).unwrap().minimum, 2);
    assert!(hls::parse(&integer.replace("VERSION:2", "VERSION:1")).is_err());
}

#[test]
fn caption_service_requires_seven_in_the_master() {
    let master = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"captions\",INSTREAM-ID=\"SERVICE1\"\n#EXT-X-STREAM-INF:BANDWIDTH=500000\nvideo.m3u8\n";
    assert_eq!(hls::parse(master).unwrap().minimum, 7);
    assert!(hls::parse(&master.replace("VERSION:7", "VERSION:6")).is_err());
}

#[test]
fn duplicate_missing_and_non_integer_versions_are_refused() {
    let good = playlist(7, "#EXT-X-MAP:URI=\"init.mp4\"\n");
    for bad in [
        good.replace("VERSION:7", "VERSION:7\n#EXT-X-VERSION:7"),
        good.replace("#EXT-X-VERSION:7\n", ""),
        good.replace("VERSION:7", "VERSION:7.0"),
    ] {
        assert!(hls::parse(&bad).is_err());
    }
}

#[test]
fn remote_child_and_variable_expansion_are_refused_before_any_tool() {
    for reference in [
        "https://example.invalid/child.m3u8",
        "../outside.m3u8",
        "%2e%2e/outside.m3u8",
    ] {
        let text = format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=500000\n{reference}\n");
        if reference == "../outside.m3u8" {
            continue;
        }
        assert!(hls::parse(&text).is_err());
    }
    let text = playlist(7, "#EXT-X-DEFINE:NAME=\"x\",VALUE=\"y\"\n");
    assert!(hls::parse(&text).is_err());
}

#[test]
fn graph_captures_every_child_without_reading_media_or_keys() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("master.m3u8"),
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=500000\nvideo.m3u8\n",
    )
    .unwrap();
    std::fs::write(temp.path().join("video.m3u8"),playlist(7,"#EXT-X-MAP:URI=\"missing-init.mp4\"\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://example.invalid/asset\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n")).unwrap();
    let graph = hls::capture(&temp.path().join("master.m3u8")).unwrap();
    assert_eq!(graph.len(), 2);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 2);
    assert!(graph.iter().any(|m| m.media && m.minimum == 6));
}

#[test]
fn graph_refuses_escape_and_cycles_are_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let entry = temp.path().join("master.m3u8");
    std::fs::write(
        &entry,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=500000\n../outside.m3u8\n",
    )
    .unwrap();
    assert!(hls::capture(&entry).is_err());
    std::fs::write(
        &entry,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=500000\nmaster.m3u8\n",
    )
    .unwrap();
    assert_eq!(hls::capture(&entry).unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn graph_refuses_symlink_children() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("actual.m3u8");
    std::fs::write(&file, playlist(7, "")).unwrap();
    std::os::unix::fs::symlink(&file, temp.path().join("video.m3u8")).unwrap();
    let entry = temp.path().join("master.m3u8");
    std::fs::write(
        &entry,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=500000\nvideo.m3u8\n",
    )
    .unwrap();
    assert!(hls::capture(&entry).is_err());
}

#[test]
fn real_apple_parse_only_shape_passes_without_media_processing() {
    assert_eq!(
        check_apple(&apple_report()).unwrap().as_deref(),
        Some("1.25.36")
    );
}

#[test]
fn apple_issue_is_not_hidden_by_successful_process_exit_or_parse_flag() {
    let mut value = apple_report();
    value["messages"] = json!([{"errorRequirementLevel":4,"errorParserCode":-12310,"errorParserLine":"private signaling"}]);
    assert_eq!(
        check_apple(&value).unwrap_err().0,
        "manifest-conformance-failed"
    );
    let mut value = apple_report();
    value["parseFailed"] = json!(true);
    assert!(check_apple(&value).is_err());
}

#[test]
fn apple_partial_unknown_wrong_source_and_unexpected_media_are_refused() {
    for (key, value) in [
        ("dataVersion", json!(2.1)),
        ("validatorName", json!("other")),
        ("url", json!("other.m3u8")),
        ("fetchCount", json!(0)),
        ("hasEndTag", json!(false)),
        ("parsedSegmentsCount", json!(2)),
        ("processedSegmentsCount", json!(1)),
        ("discontinuities", json!([])),
    ] {
        let mut report = apple_report();
        report[key] = value;
        assert!(check_apple(&report).is_err(), "{key}");
    }
    let mut value = apple_report();
    value["discontinuities"][0]["segments"][0]["processed"] = json!(true);
    assert!(check_apple(&value).is_err());
}

#[test]
fn apple_empty_truncated_or_non_json_reports_never_pass() {
    for bytes in [
        &b""[..],
        b"{}",
        b"{\"dataVersion\":1.2",
        b"completed successfully",
    ] {
        assert!(apple::validate(bytes, &expected(), "media.m3u8").is_err());
    }
}

#[test]
fn apple_nested_child_issues_and_missing_children_are_refused() {
    let mut media = expected();
    media.media = false;
    let mut master = json!({"dataVersion":1.2,"validatorName":"mediastreamvalidator","url":"master.m3u8","playlistKind":"multivariant","variants":[apple_report()]});
    assert!(apple::validate(&serde_json::to_vec(&master).unwrap(), &media, "master.m3u8").is_ok());
    master["variants"][0]["messages"] = json!([{"errorRequirementLevel":4}]);
    assert!(apple::validate(&serde_json::to_vec(&master).unwrap(), &media, "master.m3u8").is_err());
    master["variants"] = json!([]);
    assert!(apple::validate(&serde_json::to_vec(&master).unwrap(), &media, "master.m3u8").is_err());
}

#[test]
fn observed_version_never_copies_a_path_or_arbitrary_banner() {
    for value in [
        "/private/operator/tool",
        "v1.2 /private/path",
        "unknown key material",
    ] {
        assert_eq!(version_token(value), None);
    }
    assert_eq!(
        version_token("1.25.36 (/private/path)"),
        Some("1.25.36".to_owned())
    );
}

#[test]
fn dash_requires_both_official_manifest_checks_and_exact_input_digest() {
    assert!(
        check_dash(&dash_report())
            .unwrap()
            .unwrap()
            .starts_with("git:715d864")
    );
    for path in ["xsd", "schematron"] {
        for key in ["completed", "passed"] {
            let mut value = dash_report();
            value["checks"][path][key] = json!(false);
            assert!(check_dash(&value).is_err());
        }
        let mut value = dash_report();
        value["checks"][path]["issue_count"] = json!(1);
        assert!(check_dash(&value).is_err());
    }
    let mut value = dash_report();
    value["manifest_sha256"] = json!("different");
    assert!(check_dash(&value).is_err());
}

#[test]
fn dash_unknown_empty_and_untrusted_report_fields_never_pass() {
    for (key, value) in [
        ("schema_version", json!(2)),
        ("scope", json!("media")),
        ("engine", json!("custom-xml")),
        ("engine_revision", json!("/private/path")),
        ("unexpected", json!(true)),
    ] {
        let mut report = dash_report();
        report[key] = value;
        assert!(check_dash(&report).is_err());
    }
    for value in [
        json!({}),
        json!([]),
        json!({"passed":true}),
        json!({"verdict":"PASS"}),
    ] {
        assert!(check_dash(&value).is_err());
    }
}

#[test]
fn dash_namespace_and_remote_xlink_are_refused() {
    assert!(dash::admit(b"<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"/>").is_ok());
    for bytes in [&b"<MPD xmlns=\"invalid\"/>"[..],b"<!DOCTYPE MPD [<!ENTITY secret SYSTEM 'file:///private/file'>]><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\">&secret;</MPD>",b"<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xlink:href=\"https://example.invalid/remote\"/>"] { assert!(dash::admit(bytes).is_err()); }
}

#[test]
fn public_configuration_debug_never_prints_operator_paths() {
    let value = ManifestValidationTools {
        mediastreamvalidator: Some("/private/operator/apple".into()),
        dash_manifest_validator: Some("/private/operator/php".into()),
    };
    let text = format!("{value:?}");
    assert!(!text.contains("private"));
    assert!(text.contains("true"));
}

#[test]
fn unconfigured_missing_tool_and_pre_cancelled_calls_fail_closed() {
    let runner = Arc::new(crate::toolrun::ToolRunner::new(
        Arc::new(crate::toolrun::Secrets::new()),
        Arc::new(crate::events::NoSink),
    ));
    let converter =
        Converter::new(runner, "not-used", "not-used").with_timeout(Duration::from_secs(1));
    let missing = Path::new("not-read.m3u8");
    assert_eq!(
        converter
            .validate_manifest(
                missing,
                OutputFormat::Hls,
                &ManifestValidationTools::default()
            )
            .unwrap_err()
            .0,
        "manifest-validator-not-configured"
    );
    let stop = crate::concurrency::Stop::new();
    stop.set();
    let converter = converter.with_stop(stop);
    assert_eq!(
        converter
            .validate_manifest(
                missing,
                OutputFormat::Hls,
                &ManifestValidationTools::default()
            )
            .unwrap_err()
            .0,
        "cancelled"
    );
}

#[test]
fn fetch_inducing_hls_features_and_xml_includes_are_refused() {
    for extra in [
        "#EXT-X-DATERANGE:ID=\"event\",X-ASSET-URI=\"https://example.invalid/asset\"\n",
        "#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part.m4s\"\n",
    ] {
        assert!(hls::parse(&playlist(7, extra)).is_err());
    }
    assert!(dash::admit(b"<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" xmlns:xi=\"http://www.w3.org/2001/XInclude\"><xi:include href=\"https://example.invalid/remote\"/></MPD>").is_err());
    assert!(dash::admit(b"<?xml-stylesheet type=\"text/xsl\" href=\"https://example.invalid/style.xsl\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"/>").is_err());
}

#[cfg(unix)]
#[test]
fn symlink_entrypoint_is_refused_before_it_is_canonicalized() {
    let temp = tempfile::tempdir().unwrap();
    let actual = temp.path().join("actual.m3u8");
    std::fs::write(&actual, playlist(7, "")).unwrap();
    let alias = temp.path().join("alias.m3u8");
    std::os::unix::fs::symlink(actual, &alias).unwrap();
    assert!(hls::capture(&alias).is_err());
}
