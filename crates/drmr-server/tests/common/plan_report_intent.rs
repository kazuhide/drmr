use super::*;

#[test]
fn new_plan_report_refuses_payload_only_output_target_changes() {
    check_new_plan_report(
        |payload| {
            payload["outputs"][1]["target"] =
                json!(drmr_core::inspection::TargetProtection::HlsSampleAes);
        },
        false,
        false,
    );
}

#[test]
fn new_plan_report_refuses_payload_only_drm_signaling_changes() {
    check_new_plan_report(
        |payload| {
            payload["outputs"][1]["drms"] = json!(["fairplay"]);
        },
        false,
        false,
    );
}

#[test]
fn new_plan_report_refuses_payload_only_output_format_changes() {
    check_new_plan_report(
        |payload| {
            payload["outputs"][1]["format"] = json!("dash");
        },
        false,
        false,
    );
}

#[test]
fn new_plan_report_refuses_payload_only_conformance_changes() {
    check_new_plan_report(
        |payload| {
            payload["outputs"][1]["conformance"] = json!("required");
        },
        false,
        false,
    );
}

#[test]
fn new_plan_report_refuses_payload_only_source_format_changes() {
    check_new_plan_report(
        |payload| {
            payload["source_format"] = json!("dash");
        },
        false,
        false,
    );
}

#[test]
fn new_plan_report_refuses_payload_only_required_depth_changes() {
    check_new_plan_report(
        |payload| {
            payload["required_depth"] = json!("decoded");
        },
        false,
        false,
    );
}

#[test]
fn new_plan_report_requires_current_schema_while_legacy_plans_remain_importable() {
    for schema in [2, 4, 6, 8] {
        check_new_plan_schema(schema, |_| {}, false, schema == 8);
    }
}

#[test]
fn new_plan_report_ignores_runtime_measurements_and_unprojected_source_paths() {
    check_new_plan_schema(
        8,
        |payload| {
            payload["output_root"] =
                json!(Path::new(payload["output_root"].as_str().unwrap()).join("unexposed"));
            payload["source"]["root"] =
                json!(Path::new(payload["source"]["root"].as_str().unwrap()).join("unexposed"));
            payload["measurements"] = json!({"elapsed_ms":123});
        },
        false,
        true,
    );
}

#[test]
fn new_plan_report_refuses_changed_private_output_paths_with_identical_public_labels() {
    check_new_plan_report(
        |payload| {
            let options = &mut payload["output_options"][0]["options"];
            options["layout"]["tracks"][0]["media_directory"] = json!("altered");
            options["layout"]["tracks"][0]["playlist"] = json!("altered/media.m3u8");
            let typed: OutputOptions = serde_json::from_value(options.clone()).unwrap();
            typed
                .validate(OutputFormat::HlsTs, ProcessingMode::Stitch)
                .unwrap();
        },
        false,
        false,
    );
}
