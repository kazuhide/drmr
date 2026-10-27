use super::*;
use serde_json::json;

fn verification() -> Value {
    json!({
        "schema_version": 2,
        "required_depth": "native",
        "scope": "comparison",
        "passed": true,
        "checks": {"stream_metadata":"pass", "packet_payloads_equal":true, "packet_timing_equal":true},
        "decode_evidence": {"status":"not-checked", "reason":"not-requested"},
        "license_playback_verified": false,
        "native_manifest": {
            "schema_version":1, "scope":"manifest-only", "errors":0, "warnings":0,
            "truncated":false, "incomplete":false, "document_count":1
        }
    })
}

fn output(command: &str, verification: Value) -> Value {
    let mut value = json!({
        "id":"dash", "signaling":{"systems":["widevine", "playready"], "verified":true},
        "conformance":null
    });
    if command == "convert" {
        value["conversion"] = json!({"output":{"format":"dash"}, "verification":verification});
    } else {
        value["verification"] = verification;
    }
    value
}

fn render(command: &str, output: Value) -> String {
    let mut bytes = Vec::new();
    write_text(&mut bytes, command, &json!({"outputs":[output]}), true).unwrap();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn successful_convert_and_verify_show_executed_native_checks_without_optional_noise() {
    for command in ["convert", "verify"] {
        let text = render(command, output(command, verification()));
        assert!(
            text.contains(
                "Media comparison passed; native manifest checks passed; DRM declarations passed."
            ),
            "{text}"
        );
        assert!(!text.contains("external"), "{text}");
        assert!(
            !text.contains("decoded") && !text.contains("playback"),
            "{text}"
        );
    }
}

#[test]
fn absent_failed_or_incomplete_media_evidence_cannot_claim_a_comparison_pass() {
    for pointer in [
        "/passed",
        "/checks/stream_metadata",
        "/checks/packet_payloads_equal",
        "/checks/packet_timing_equal",
    ] {
        for replacement in [Value::Null, json!(false)] {
            let mut proof = verification();
            *proof.pointer_mut(pointer).unwrap() = replacement;
            let text = render("convert", output("convert", proof));
            assert!(
                !text.contains("Media comparison passed"),
                "{pointer}: {text}"
            );
        }
    }
    for proof in [
        Value::Null,
        json!({"passed":true}),
        {
            let mut proof = verification();
            proof["scope"] = "source-admission".into();
            proof
        },
        {
            let mut proof = verification();
            proof["failure"] = "child-check-failed".into();
            proof
        },
    ] {
        let text = render("verify", output("verify", proof));
        assert!(!text.contains("Media comparison passed"), "{text}");
    }
}

#[test]
fn manifest_errors_truncation_incompleteness_or_missing_fields_cannot_claim_a_pass() {
    for (field, replacement) in [
        ("errors", json!(1)),
        ("truncated", json!(true)),
        ("incomplete", json!(true)),
        ("errors", Value::Null),
        ("truncated", Value::Null),
        ("incomplete", Value::Null),
        ("document_count", json!(0)),
        ("scope", json!("unknown")),
    ] {
        let mut proof = verification();
        proof["native_manifest"][field] = replacement;
        let text = render("convert", output("convert", proof));
        assert!(
            !text.contains("native manifest checks passed"),
            "{field}: {text}"
        );
    }
}

#[test]
fn mp4_comparison_does_not_claim_manifest_decoding_or_playback_checks() {
    let mut proof = verification();
    proof.as_object_mut().unwrap().remove("native_manifest");
    let mut item = output("convert", proof);
    item["conversion"]["output"]["format"] = "mp4".into();
    item["signaling"] = json!({"systems":[], "verified":false});
    let text = render("convert", item);
    assert!(
        text.contains("Media comparison passed; DRM declarations not requested."),
        "{text}"
    );
    assert!(
        !text.contains("manifest") && !text.contains("decoded") && !text.contains("playback"),
        "{text}"
    );
}

fn external() -> Value {
    json!({
        "scope":"manifest-only", "passed":true, "manifest_count":1,
        "checks":[{"passed":true, "checked_manifest_count":1}]
    })
}

#[test]
fn requested_external_checks_show_only_an_explicit_complete_manifest_pass() {
    let mut item = output("convert", verification());
    item["conformance"] = external();
    let text = render("convert", item.clone());
    assert!(
        text.contains("external manifest checks passed (manifest only)"),
        "{text}"
    );
    for (pointer, replacement) in [
        ("/passed", json!(false)),
        ("/passed", Value::Null),
        ("/checks/0/passed", json!(false)),
        ("/checks/0/passed", Value::Null),
        ("/checks/0/checked_manifest_count", json!(0)),
        ("/checks", json!([])),
        ("/manifest_count", json!(0)),
        ("/scope", json!("unknown")),
    ] {
        item["conformance"] = external();
        *item["conformance"].pointer_mut(pointer).unwrap() = replacement;
        let text = render("convert", item.clone());
        assert!(
            !text.contains("external manifest checks passed"),
            "{pointer}: {text}"
        );
    }
}

#[test]
fn only_complete_equal_decoded_comparison_can_claim_a_decode_pass() {
    for (status, equal, expected) in [
        ("content-compared", true, true),
        ("content-compared", false, false),
        ("source-decoded", true, false),
        ("not-checked", true, false),
    ] {
        let mut proof = verification();
        proof["required_depth"] = "decoded".into();
        proof["decode_evidence"] = json!({"status":status, "equal":equal});
        let text = render("verify", output("verify", proof));
        assert_eq!(
            text.contains("decoded media comparison passed"),
            expected,
            "{text}"
        );
    }
}

#[test]
fn requested_drm_with_failed_or_missing_evidence_is_not_reported_as_unrequested() {
    for verified in [json!(false), Value::Null] {
        let mut item = output("verify", verification());
        item["signaling"]["verified"] = verified;
        let text = render("verify", item);
        assert!(
            !text.contains("DRM declarations passed")
                && !text.contains("DRM declarations not requested"),
            "{text}"
        );
    }
}

#[test]
fn manifest_only_and_plan_text_keep_their_existing_scope() {
    let mut bytes = Vec::new();
    write_text(
        &mut bytes,
        "verify",
        &json!({"scope":"manifest-only"}),
        true,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "Manifest validation: passed (manifest only; media correspondence was not checked).\n"
    );
    let text = render(
        "plan",
        json!({"id":"dash", "format":"dash", "target":"cbcs"}),
    );
    assert!(
        text.contains("Execution plan saved with 1 outputs."),
        "{text}"
    );
    assert!(text.ends_with("  dash: dash / cbcs\n"), "{text}");
    assert!(!text.contains("comparison"), "{text}");
}
