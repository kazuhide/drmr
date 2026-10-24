use super::*;

fn check_depth(
    schema: u32,
    operation: &str,
    minimum: ValidationDepth,
    decode: bool,
    reported: ValidationDepth,
    mutate: impl FnOnce(&mut Value),
    accepted: bool,
) {
    let h = support::Harness::new();
    analyze_source(&h);
    let plan = saved_plan(&h, schema, minimum, ProcessingMode::Segments);
    let before = std::fs::read(h.input.join("plan.json")).unwrap();
    let started = start_saved_operation_decode(&h, operation, decode);
    assert_eq!(child_request(&h.cli, 1)["runtime"]["decode"], decode);
    // Legacy saved plans can also receive a valid current-version media report.
    let mut report_plan = plan.clone();
    if schema < 3 {
        report_plan["schema_version"] = json!(schema + 2);
    }
    let mut report = conversion_report(&h, &report_plan, reported);
    if operation == "verify" {
        report = verification_report(report);
    }
    mutate(&mut report);
    let ended = finish_child(&h, 1, &started, &report);
    assert_result(&h, &ended, &report, accepted);
    assert_eq!(std::fs::read(h.input.join("plan.json")).unwrap(), before);
    drmr_core::conversion::plan::read_saved(&h.input.join("plan.json")).unwrap();
}

#[test]
fn saved_single_conversion_refuses_native_proof_below_the_frozen_decoded_minimum() {
    for schema in [5, 3, 1] {
        check_depth(
            schema,
            "convert",
            ValidationDepth::Decoded,
            false,
            ValidationDepth::Native,
            |_| {},
            false,
        );
    }
}

#[test]
fn saved_bundle_conversion_refuses_native_proof_below_the_frozen_decoded_minimum() {
    for schema in [6, 4, 2] {
        check_depth(
            schema,
            "convert",
            ValidationDepth::Decoded,
            false,
            ValidationDepth::Native,
            |_| {},
            false,
        );
    }
}

#[test]
fn saved_single_verification_refuses_native_proof_below_the_frozen_decoded_minimum() {
    for schema in [5, 3, 1] {
        check_depth(
            schema,
            "verify",
            ValidationDepth::Decoded,
            false,
            ValidationDepth::Native,
            |_| {},
            false,
        );
    }
}

#[test]
fn saved_bundle_verification_refuses_native_proof_below_the_frozen_decoded_minimum() {
    for schema in [6, 4, 2] {
        check_depth(
            schema,
            "verify",
            ValidationDepth::Decoded,
            false,
            ValidationDepth::Native,
            |_| {},
            false,
        );
    }
}

#[test]
fn legacy_saved_minimum_cannot_be_lowered_by_a_current_native_report() {
    for schema in [1, 2] {
        for operation in ["convert", "verify"] {
            check_depth(
                schema,
                operation,
                ValidationDepth::Native,
                false,
                ValidationDepth::Native,
                |_| {},
                false,
            );
        }
    }
}

#[test]
fn explicit_decode_request_refuses_native_conversion_proof_for_a_native_plan() {
    for schema in [5, 6, 3, 4] {
        check_depth(
            schema,
            "convert",
            ValidationDepth::Native,
            true,
            ValidationDepth::Native,
            |_| {},
            false,
        );
    }
}

#[test]
fn explicit_decode_request_refuses_native_verification_proof_for_a_native_plan() {
    for schema in [5, 6, 3, 4] {
        check_depth(
            schema,
            "verify",
            ValidationDepth::Native,
            true,
            ValidationDepth::Native,
            |_| {},
            false,
        );
    }
}

#[test]
fn native_saved_plans_accept_stronger_decoded_proof_with_or_without_a_decode_request() {
    for schema in [3, 4, 5, 6] {
        for operation in ["convert", "verify"] {
            for decode in [false, true] {
                check_depth(
                    schema,
                    operation,
                    ValidationDepth::Native,
                    decode,
                    ValidationDepth::Decoded,
                    |_| {},
                    true,
                );
            }
        }
    }
}

fn lower_second_child(report: &mut Value) {
    let conversion = report["kind"] == "bundle-conversion";
    let row = &mut report["outputs"][1];
    let proof = if conversion {
        &mut row["conversion"]["verification"]
    } else {
        &mut row["verification"]
    };
    proof["required_depth"] = json!("native");
    proof["decode_evidence"] = json!({"status":"not-checked","reason":"not-requested"});
    if conversion {
        let segments = row["conversion"]["processing"]["mode"] == "segments";
        row["conversion"]["processing"] = actual(segments, ValidationDepth::Native);
    }
}

#[test]
fn bundle_decoded_minimum_applies_to_every_child_after_a_valid_first_child() {
    for schema in [6, 4, 2] {
        for operation in ["convert", "verify"] {
            check_depth(
                schema,
                operation,
                ValidationDepth::Decoded,
                false,
                ValidationDepth::Decoded,
                lower_second_child,
                false,
            );
        }
    }
}

fn legacy_proof(report: &mut Value) {
    report["schema_version"] = json!(1);
    if let Some(outputs) = report.get_mut("outputs").and_then(Value::as_array_mut) {
        for output in outputs {
            if output.get("conversion").is_some() {
                legacy_proof(&mut output["conversion"]);
            } else {
                output["verification"] = legacy_verification();
            }
        }
    } else {
        report["verification"] = legacy_verification();
    }
}

fn legacy_verification() -> Value {
    json!({"schema_version":1,"passed":true,"media_verified":true,"license_playback_verified":false,
        "checks":{"stream_metadata":"pass","packet_payloads_equal":true,"packet_timing_equal":true,"decoded_content_equal":true}})
}

#[test]
fn valid_legacy_v1_decoded_proof_satisfies_current_and_legacy_saved_minimums() {
    for schema in [1, 2, 3, 4, 5, 6] {
        for operation in ["convert", "verify"] {
            check_depth(
                schema,
                operation,
                ValidationDepth::Decoded,
                true,
                ValidationDepth::Decoded,
                legacy_proof,
                true,
            );
        }
    }
}

#[test]
fn new_plan_minimum_must_honor_an_explicit_decode_request() {
    for schema in [6, 4] {
        check_new_plan_minimum(schema, ValidationDepth::Native, true, |_| {}, false, false);
    }
}

#[test]
fn new_plans_accept_stronger_and_implicitly_decoded_legacy_minimums() {
    for schema in [2, 4, 6] {
        for decode in [false, true] {
            check_new_plan_minimum(
                schema,
                ValidationDepth::Decoded,
                decode,
                |_| {},
                false,
                true,
            );
        }
    }
}

fn check_analysis_depth(
    decode: bool,
    depth: ValidationDepth,
    evidence: &Value,
    legacy: bool,
    accepted: bool,
) {
    let h = support::Harness::new();
    let mut payload = source_analysis_payload(&h);
    if !legacy {
        payload["schema_version"] = json!(2);
        payload.as_object_mut().unwrap().remove("input_checks");
        payload["verification"] = json!({"schema_version":2,"required_depth":depth,
            "scope":"source-admission","passed":true,"license_playback_verified":false,
            "checks":{"stream_metadata":"pass","packet_payloads_equal":true,"packet_timing_equal":true},
            "decode_evidence":evidence});
    }
    let (server_id, input_ref) = crate::media_ref(&h.client);
    let session = crate::local_session(&h.client, &server_id, &input_ref, 'a');
    h.cli.hold();
    let started = h.client.post_json("/api/common/jobs", &json!({"schema_version":1,
        "server_id":server_id,"session_id":session["session_id"],"request_id":"b".repeat(32),
        "operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":1,"decode":decode}}));
    assert_eq!(started.status, 202, "{}", started.text());
    assert_eq!(child_request(&h.cli, 0)["runtime"]["decode"], decode);
    let ended = finish_child(&h, 0, &started.json(), &payload);
    assert_result(&h, &ended, &payload, accepted);
    if !accepted {
        assert!(ended["analysis_ref"].is_null(), "{ended}");
    }
}

#[test]
fn explicit_decode_analysis_refuses_a_valid_native_source_admission_reply() {
    check_analysis_depth(
        true,
        ValidationDepth::Native,
        &json!({"status":"not-checked","reason":"not-requested"}),
        false,
        false,
    );
}

#[test]
fn analysis_accepts_source_decoded_proof_with_or_without_a_decode_request() {
    for decode in [false, true] {
        check_analysis_depth(
            decode,
            ValidationDepth::Decoded,
            &json!({"status":"source-decoded"}),
            false,
            true,
        );
    }
}

#[test]
fn analysis_decode_request_keeps_legacy_decoded_admission_evidence() {
    check_analysis_depth(true, ValidationDepth::Decoded, &Value::Null, true, true);
}

#[test]
fn analysis_decode_proof_cannot_use_comparison_evidence_for_source_admission() {
    check_analysis_depth(
        true,
        ValidationDepth::Decoded,
        &json!({"status":"content-compared","equal":true}),
        false,
        false,
    );
}
