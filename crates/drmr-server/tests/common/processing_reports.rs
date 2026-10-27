use super::*;
use drmr_core::{
    conversion::plan::{ExecutionPlan, OutputIntent},
    inspection::{ActualProcessing, InputPreparation, ProcessingRouteId, RequiredTool},
};

#[path = "plan_report_intent.rs"]
mod plan_intent;
#[path = "plan_report_depth.rs"]
mod report_depth;

fn choice(requested: ProcessingMode) -> ProcessingSelection {
    let segments = requested == ProcessingMode::Segments;
    ProcessingSelection {
        requested,
        resolved: if segments {
            requested
        } else {
            ProcessingMode::Stitch
        },
        boundaries: if segments {
            BoundaryHandling::Preserve
        } else {
            BoundaryHandling::Resegment
        },
        segment_duration_ms: (!segments).then_some(2000),
    }
}

fn assert_result(h: &support::Harness, ended: &Value, expected: &Value, accepted: bool) {
    assert_eq!(ended["cleanup_confirmed"], true, "{ended}");
    assert_eq!(ended["supervision_confirmed"], true, "{ended}");
    if accepted {
        assert_eq!(ended["state"], "succeeded", "{ended}");
        let report = h.client.get(&format!(
            "/api/common/reports/{}",
            ended["report_ref"].as_str().unwrap()
        ));
        assert_eq!(report.status, 200, "{}", report.text());
        assert_eq!(
            report.json()["report"]["processing"],
            expected["processing"]
        );
        if let Some(outputs) = expected["outputs"].as_array()
            && expected["kind"] == "bundle-conversion"
        {
            for (index, output) in outputs.iter().enumerate() {
                assert_eq!(
                    report.json()["report"]["outputs"][index]["conversion"]["processing"],
                    output["conversion"]["processing"]
                );
            }
        }
        assert_eq!(report.json()["plan_id"], expected["plan_id"]);
    } else {
        assert_eq!(ended["state"], "failed", "{ended}");
        assert_eq!(ended["error"]["code"], "result-invalid", "{ended}");
        assert!(ended["report_ref"].is_null(), "{ended}");
    }
}

fn check_new_plan_report(mutate: impl FnOnce(&mut Value), mixed: bool, accepted: bool) {
    check_new_plan_schema(10, mutate, mixed, accepted);
}

fn check_new_plan_schema(
    schema: u32,
    mutate: impl FnOnce(&mut Value),
    mixed: bool,
    accepted: bool,
) {
    check_new_plan_minimum(
        schema,
        ValidationDepth::Native,
        false,
        mutate,
        mixed,
        accepted,
    );
}

fn check_new_plan_minimum(
    schema: u32,
    minimum: ValidationDepth,
    decode: bool,
    mutate: impl FnOnce(&mut Value),
    mixed: bool,
    accepted: bool,
) {
    let h = support::Harness::new();
    let analyzed = analyze_source(&h);
    let mut outputs = json!([
        {"id":"first","format":"hls-ts","target":"clear","drms":[]},
        {"id":"second","format":"hls-ts","target":"clear","drms":[]}]);
    let second = if mixed {
        ProcessingMode::Segments
    } else {
        ProcessingMode::Auto
    };
    if mixed {
        outputs[1]["processing"] = json!(second);
    }
    let started = start_plan_decode(&h, &analyzed, &outputs, decode);
    let mut plan = returned_plan(&h, outputs, [ProcessingMode::Auto, second]);
    let request = child_request(&h.cli, 1);
    assert_eq!(request["runtime"]["decode"], decode);
    let path = Path::new(request["operation"]["save_file"].as_str().unwrap());
    if schema != 10 || minimum != ValidationDepth::Native {
        plan.schema_version = schema;
        plan.required_depth = (schema > 2).then_some(minimum);
        if schema < 6 {
            plan.processing = None;
        }
        if schema < 8 {
            plan.output_options = None;
        } else if schema == 8 {
            for ((setting, output), choice) in plan
                .output_options
                .as_mut()
                .unwrap()
                .iter_mut()
                .zip(&plan.outputs)
                .zip(plan.processing.as_ref().unwrap())
            {
                setting.options.layout = Some(fixture_layout(
                    &plan.source,
                    output.format,
                    choice.selection.resolved,
                    false,
                ));
            }
        }
        let identity = BundleIdentity {
            kind: &plan.kind,
            schema_version: schema,
            source: &plan.source,
            source_format: plan.source_format,
            output_root: &plan.output_root,
            outputs: &plan.outputs,
            required_depth: plan.required_depth,
            processing: plan.processing.as_deref(),
            output_options: plan.output_options.as_deref(),
        };
        plan.plan_id = hex::encode(Sha256::digest(serde_json::to_vec(&identity).unwrap()));
        support::write_private(path, serde_json::to_vec(&plan).unwrap()).unwrap();
        drmr_core::conversion::plan::read_saved(path).unwrap();
    }
    let before = std::fs::read(path).unwrap();
    let mut payload = serde_json::to_value(&plan).unwrap();
    mutate(&mut payload);
    let ended = finish_child(&h, 1, &started, &payload);
    assert_result(&h, &ended, &payload, accepted);
    if !accepted {
        assert!(ended["plan_ref"].is_null(), "{ended}");
    }
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(
        drmr_core::conversion::plan::read_saved(path).unwrap(),
        drmr_core::conversion::plan::SavedPlan::Bundle(plan)
    );
}

#[test]
fn new_plan_report_refuses_a_different_valid_resolution_with_the_saved_id() {
    check_new_plan_report(
        |payload| {
            payload["processing"][1]["selection"] = json!(choice(ProcessingMode::Segments));
        },
        false,
        false,
    );
}

#[test]
fn new_plan_report_refuses_a_different_requested_mode_with_the_same_resolution() {
    check_new_plan_report(
        |payload| {
            payload["processing"][1]["selection"]["requested"] = json!("stitch");
        },
        false,
        false,
    );
}

#[test]
fn new_plan_report_refuses_a_different_processing_order() {
    check_new_plan_report(
        |payload| {
            payload["processing"].as_array_mut().unwrap().reverse();
        },
        false,
        false,
    );
}

#[test]
fn new_plan_report_accepts_frozen_auto_stitch_and_segments_choices() {
    check_new_plan_report(|_| {}, true, true);
}

#[derive(Serialize)]
struct SingleIdentity<'a> {
    kind: &'a str,
    schema_version: u32,
    source: &'a SourceInventory,
    source_format: OutputFormat,
    intent: &'a OutputIntent,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_depth: Option<ValidationDepth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    processing: Option<ProcessingSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_options: Option<&'a OutputOptions>,
}

fn fixture_output_options(
    source: &SourceInventory,
    format: OutputFormat,
    processing: ProcessingMode,
    current: bool,
) -> OutputOptions {
    OutputOptions {
        layout: Some(fixture_layout(source, format, processing, current)),
        dash_addressing: None,
    }
}

fn saved_plan(
    h: &support::Harness,
    schema: u32,
    depth: ValidationDepth,
    single_mode: ProcessingMode,
) -> Value {
    let source = FrozenSource::capture(&h.input.join("source.m3u8"), &Stop::default()).unwrap();
    let required_depth = (schema > 2).then_some(depth);
    let value = if schema % 2 == 1 {
        let mut plan = ExecutionPlan {
            kind: "execution-plan".into(),
            schema_version: schema,
            plan_id: String::new(),
            source: source.inventory().clone(),
            source_format: OutputFormat::HlsTs,
            intent: OutputIntent {
                format: OutputFormat::HlsTs,
                target: drmr_core::inspection::TargetProtection::Clear,
                output: h.output.canonicalize().unwrap().join("master.m3u8"),
            },
            required_depth,
            processing: (schema >= 5).then(|| choice(single_mode)),
            output_options: (schema >= 7).then(|| {
                fixture_output_options(
                    source.inventory(),
                    OutputFormat::HlsTs,
                    choice(single_mode).resolved,
                    schema >= 9,
                )
            }),
        };
        let identity = SingleIdentity {
            kind: &plan.kind,
            schema_version: schema,
            source: &plan.source,
            source_format: plan.source_format,
            intent: &plan.intent,
            required_depth,
            processing: plan.processing,
            output_options: plan.output_options.as_ref(),
        };
        plan.plan_id = hex::encode(Sha256::digest(serde_json::to_vec(&identity).unwrap()));
        serde_json::to_value(plan).unwrap()
    } else {
        let outputs: Vec<BundleOutputIntent> = serde_json::from_value(json!([
            {"id":"first","format":"hls-ts","target":"clear","drms":[]},
            {"id":"second","format":"hls-ts","target":"clear","drms":[]}]))
        .unwrap();
        let processing: Option<Vec<OutputProcessing>> = (schema >= 6).then(|| {
            outputs
                .iter()
                .zip([ProcessingMode::Auto, ProcessingMode::Segments])
                .map(|(output, requested)| OutputProcessing {
                    id: output.id.clone(),
                    selection: choice(requested),
                })
                .collect()
        });
        let output_options = (schema >= 8).then(|| {
            outputs
                .iter()
                .zip(processing.as_ref().unwrap())
                .map(|(output, choice)| OutputSettings {
                    id: output.id.clone(),
                    options: fixture_output_options(
                        source.inventory(),
                        output.format,
                        choice.selection.resolved,
                        schema >= 10,
                    ),
                })
                .collect()
        });
        let mut plan = BundlePlan {
            kind: "execution-plan".into(),
            schema_version: schema,
            plan_id: String::new(),
            source: source.inventory().clone(),
            source_format: OutputFormat::HlsTs,
            output_root: h.output.canonicalize().unwrap().join("result"),
            outputs,
            required_depth,
            processing,
            output_options,
        };
        let identity = BundleIdentity {
            kind: &plan.kind,
            schema_version: schema,
            source: &plan.source,
            source_format: plan.source_format,
            output_root: &plan.output_root,
            outputs: &plan.outputs,
            required_depth,
            processing: plan.processing.as_deref(),
            output_options: plan.output_options.as_deref(),
        };
        plan.plan_id = hex::encode(Sha256::digest(serde_json::to_vec(&identity).unwrap()));
        serde_json::to_value(plan).unwrap()
    };
    let path = h.input.join("plan.json");
    support::write_private(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    drmr_core::conversion::plan::read_saved(&path).unwrap();
    value
}

fn start_saved_operation(h: &support::Harness, operation: &str) -> Value {
    start_saved_operation_decode(h, operation, false)
}

fn start_saved_operation_decode(h: &support::Harness, operation: &str, decode: bool) -> Value {
    let cap = h.client.get("/api/common/capabilities").json();
    let root = cap["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|root| root["label"] == "Input folder")
        .unwrap();
    let listed = h
        .client
        .get(&format!(
            "/api/common/files?root_ref={}&role=saved-plan",
            root["root_ref"].as_str().unwrap()
        ))
        .json();
    let file = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["label"] == "plan.json")
        .unwrap();
    let imported = h.client.post_json(
        "/api/common/sessions",
        &json!({"schema_version":1,
        "server_id":cap["server_id"],"creation_request_id":"c".repeat(32),
        "source":{"kind":"saved-plan","plan_file_ref":file["file_ref"]}}),
    );
    assert_eq!(imported.status, 201, "{}", imported.text());
    let session = imported.json();
    let started = h.client.post_json(
        "/api/common/jobs",
        &json!({"schema_version":1,
        "server_id":cap["server_id"],"session_id":session["session_id"],"request_id":"d".repeat(32),
        "operation":{"kind":operation,"plan_ref":session["plan_refs"][0]},
        "runtime":{"keys_ref":null,"max_workers":1,"decode":decode}}),
    );
    assert_eq!(started.status, 202, "{}", started.text());
    started.json()
}

fn actual(segments: bool, depth: ValidationDepth) -> Value {
    let mut actual = if segments {
        ActualProcessing::new(
            ProcessingRouteId::MpegTsSegments,
            InputPreparation::OriginalSegments,
        )
    } else {
        ActualProcessing::new(
            ProcessingRouteId::MpegTsHlsTs,
            InputPreparation::MpegTsDemux,
        )
    };
    if depth == ValidationDepth::Decoded {
        actual.required_tools = vec![
            RequiredTool::Ffmpeg,
            RequiredTool::Ffprobe,
            RequiredTool::ShakaPackager,
        ];
    }
    serde_json::to_value(actual).unwrap()
}

fn conversion(h: &support::Harness, segments: bool, depth: ValidationDepth, legacy: bool) -> Value {
    let mut input = analysis_payload(&h.input)["input"].clone();
    input["format"] = json!("hls");
    let verification = if legacy {
        json!({"schema_version":1,"passed":true,"media_verified":true,
            "checks":{"stream_metadata":"pass","packet_payloads_equal":true,"packet_timing_equal":true,"decoded_content_equal":true},
            "license_playback_verified":false})
    } else {
        json!({"schema_version":2,"required_depth":depth,"scope":"comparison","passed":true,
            "checks":{"stream_metadata":"pass","packet_payloads_equal":true,"packet_timing_equal":true},
            "decode_evidence":if depth == ValidationDepth::Decoded {json!({"status":"content-compared","equal":true})}
                else {json!({"status":"not-checked","reason":"not-requested"})},"license_playback_verified":false})
    };
    json!({"schema_version":if legacy {1} else {2},"input":input,"output":input,
        "verification":verification,"processing":actual(segments, depth)})
}

fn conversion_report(h: &support::Harness, plan: &Value, depth: ValidationDepth) -> Value {
    let schema = plan["schema_version"].as_u64().unwrap();
    if schema % 2 == 1 {
        let mut report = conversion(
            h,
            plan["processing"]["resolved"] == "segments",
            depth,
            schema == 1,
        );
        report["plan_id"] = plan["plan_id"].clone();
        report
    } else {
        let outputs: Vec<Value> = plan["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(index, output)| {
                let segments = plan["processing"][index]["selection"]["resolved"] == "segments";
                json!({"id":output["id"],"conversion":conversion(h, segments, depth, schema == 2),
                "signaling":{"systems":[],"verified":true},"conformance":null})
            })
            .collect();
        json!({"kind":"bundle-conversion","schema_version":if schema == 2 {1} else {2},
            "plan_id":plan["plan_id"],"outputs":outputs})
    }
}

fn verification_report(mut report: Value) -> Value {
    if report.get("outputs").is_some() {
        report["kind"] = json!("bundle-verification");
        report["passed"] = json!(true);
        for output in report["outputs"].as_array_mut().unwrap() {
            output["verification"] = output["conversion"]["verification"].clone();
            output["output_matches_plan"] = json!(true);
            output.as_object_mut().unwrap().remove("conversion");
        }
    } else {
        report = json!({"kind":"plan-verification","schema_version":report["schema_version"],
            "plan_id":report["plan_id"],"output_matches_plan":true,"verification":report["verification"]});
    }
    report["source_unchanged"] = json!(true);
    report
}

fn selected_conversion(report: &mut Value) -> &mut Value {
    if report["kind"] == "bundle-conversion" {
        &mut report["outputs"][1]["conversion"]
    } else {
        report
    }
}

fn check_saved_report(
    schema: u32,
    depth: ValidationDepth,
    mutate: impl FnOnce(&mut Value),
    accepted: bool,
) {
    check_saved_single_mode(schema, depth, ProcessingMode::Segments, mutate, accepted);
}

fn check_saved_single_mode(
    schema: u32,
    depth: ValidationDepth,
    mode: ProcessingMode,
    mutate: impl FnOnce(&mut Value),
    accepted: bool,
) {
    let h = support::Harness::new();
    analyze_source(&h);
    let plan = saved_plan(&h, schema, depth, mode);
    let before = std::fs::read(h.input.join("plan.json")).unwrap();
    let started = start_saved_operation(&h, "convert");
    let mut report = conversion_report(&h, &plan, depth);
    mutate(&mut report);
    let ended = finish_child(&h, 1, &started, &report);
    assert_result(&h, &ended, &report, accepted);
    assert_eq!(std::fs::read(h.input.join("plan.json")).unwrap(), before);
    drmr_core::conversion::plan::read_saved(&h.input.join("plan.json")).unwrap();
}

fn check_changed_mode(schema: u32) {
    for depth in [ValidationDepth::Native, ValidationDepth::Decoded] {
        check_saved_report(
            schema,
            depth,
            |report| {
                selected_conversion(report)["processing"] = actual(false, depth);
            },
            false,
        );
    }
}

#[test]
fn saved_single_conversion_refuses_an_independently_valid_different_actual_mode() {
    check_changed_mode(5);
}

#[test]
fn saved_bundle_conversion_refuses_an_independently_valid_different_actual_mode() {
    check_changed_mode(6);
}

#[test]
fn saved_conversion_refuses_a_different_boundary_guarantee_for_the_same_actual_mode() {
    check_saved_report(
        6,
        ValidationDepth::Native,
        |report| {
            report["outputs"][0]["conversion"]["processing"] = json!(ActualProcessing::new(
                ProcessingRouteId::ProgressiveMp4,
                InputPreparation::CompleteFile
            ));
        },
        false,
    );
}

fn check_missing_actual(schema: u32) {
    check_saved_report(
        schema,
        ValidationDepth::Native,
        |report| {
            selected_conversion(report)["processing"]
                .as_object_mut()
                .unwrap()
                .remove("mode");
            selected_conversion(report)["processing"]
                .as_object_mut()
                .unwrap()
                .remove("boundaries");
        },
        false,
    );
    check_saved_report(
        schema,
        ValidationDepth::Native,
        |report| {
            selected_conversion(report)
                .as_object_mut()
                .unwrap()
                .remove("processing");
        },
        false,
    );
}

#[test]
fn saved_single_conversion_requires_actual_mode_and_boundary_guarantees() {
    check_missing_actual(5);
}

#[test]
fn saved_bundle_conversion_requires_actual_mode_and_boundary_guarantees() {
    check_missing_actual(6);
}

#[test]
fn saved_conversion_accepts_resolved_auto_stitch_and_explicit_segments_at_each_depth() {
    for schema in [5, 6, 7, 8, 9, 10] {
        for depth in [ValidationDepth::Native, ValidationDepth::Decoded] {
            check_saved_report(schema, depth, |_| {}, true);
        }
    }
}

#[test]
fn saved_single_conversion_accepts_auto_and_stitch_as_actual_stitch_at_each_depth() {
    for mode in [ProcessingMode::Auto, ProcessingMode::Stitch] {
        for depth in [ValidationDepth::Native, ValidationDepth::Decoded] {
            check_saved_single_mode(5, depth, mode, |_| {}, true);
        }
    }
}

#[test]
fn legacy_saved_conversion_refuses_present_modes_that_contradict_frozen_stitch_behavior() {
    for schema in [1, 2, 3, 4] {
        check_saved_report(
            schema,
            ValidationDepth::Decoded,
            |report| {
                selected_conversion(report)["processing"] = actual(true, ValidationDepth::Decoded);
            },
            false,
        );
    }
}

#[test]
fn saved_verification_keeps_absent_processing_and_the_closed_report_schema() {
    for schema in [5, 6, 7, 8, 9, 10] {
        for unexpected_processing in [false, true] {
            let h = support::Harness::new();
            analyze_source(&h);
            let plan = saved_plan(
                &h,
                schema,
                ValidationDepth::Native,
                ProcessingMode::Segments,
            );
            let started = start_saved_operation(&h, "verify");
            let mut report =
                verification_report(conversion_report(&h, &plan, ValidationDepth::Native));
            if unexpected_processing {
                report["processing"] = actual(true, ValidationDepth::Native);
            }
            let ended = finish_child(&h, 1, &started, &report);
            assert_result(&h, &ended, &report, !unexpected_processing);
        }
    }
}

#[test]
fn legacy_saved_conversion_accepts_present_stitch_mode_and_boundary_guarantees() {
    for schema in [1, 2, 3, 4] {
        check_saved_report(schema, ValidationDepth::Decoded, |_| {}, true);
    }
}

#[test]
fn legacy_saved_conversions_keep_absent_actual_processing_semantics() {
    for schema in [1, 2, 3, 4] {
        check_saved_report(
            schema,
            ValidationDepth::Decoded,
            |report| {
                if let Some(outputs) = report.get_mut("outputs").and_then(Value::as_array_mut) {
                    for output in outputs {
                        let processing =
                            output["conversion"]["processing"].as_object_mut().unwrap();
                        processing.remove("mode");
                        processing.remove("boundaries");
                    }
                } else {
                    report.as_object_mut().unwrap().remove("processing");
                }
            },
            true,
        );
    }
}

fn check_legacy_segment_route(schema: u32, route: ProcessingRouteId) {
    check_saved_report(
        schema,
        ValidationDepth::Native,
        |report| {
            let mut processing = json!(ActualProcessing::new(
                route,
                InputPreparation::OriginalSegments
            ));
            let fields = processing.as_object_mut().unwrap();
            fields.remove("mode");
            fields.remove("boundaries");
            selected_conversion(report)["processing"] = processing;
        },
        false,
    );
}

#[test]
fn legacy_single_native_plan_refuses_mpeg_ts_segments_without_guarantees() {
    check_legacy_segment_route(3, ProcessingRouteId::MpegTsSegments);
}

#[test]
fn legacy_bundle_native_plan_refuses_mpeg_ts_segments_without_guarantees() {
    check_legacy_segment_route(4, ProcessingRouteId::MpegTsSegments);
}

#[test]
fn legacy_single_native_plan_refuses_fmp4_segments_without_guarantees() {
    check_legacy_segment_route(3, ProcessingRouteId::Fmp4Segments);
}

#[test]
fn legacy_bundle_native_plan_refuses_fmp4_segments_without_guarantees() {
    check_legacy_segment_route(4, ProcessingRouteId::Fmp4Segments);
}

#[test]
fn genuine_legacy_routes_keep_absent_guarantees_at_native_and_decoded_depths() {
    for schema in [1, 2, 3, 4] {
        let depth = if schema <= 2 {
            ValidationDepth::Decoded
        } else {
            ValidationDepth::Native
        };
        check_saved_report(
            schema,
            depth,
            |report| {
                let mut processing = actual(false, depth);
                let fields = processing.as_object_mut().unwrap();
                fields.remove("mode");
                fields.remove("boundaries");
                if let Some(outputs) = report.get_mut("outputs").and_then(Value::as_array_mut) {
                    for output in outputs {
                        output["conversion"]["processing"] = processing.clone();
                    }
                } else {
                    report["processing"] = processing;
                }
            },
            true,
        );
    }
}
