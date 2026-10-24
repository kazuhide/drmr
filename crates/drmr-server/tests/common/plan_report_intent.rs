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
fn new_plan_report_keeps_current_and_legacy_visible_intent_bound_to_the_saved_file() {
    for schema in [2, 4, 6] {
        check_new_plan_schema(schema, |_| {}, false, true);
    }
}

#[test]
fn new_plan_report_ignores_runtime_measurements_and_hidden_paths_in_each_schema() {
    for schema in [2, 4, 6] {
        check_new_plan_schema(
            schema,
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
}
