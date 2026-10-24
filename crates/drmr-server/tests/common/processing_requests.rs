use super::{analysis_payload, support};
use drmr_core::{
    concurrency::Stop,
    conversion::{
        FrozenSource, SourceInventory, ValidationDepth,
        plan::bundle::{BundleOutputIntent, BundlePlan, OutputProcessing},
        processing::{BoundaryHandling, ProcessingMode, ProcessingSelection},
    },
    inspection::OutputFormat,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::path::Path;

#[path = "processing_reports.rs"]
mod report_binding;

fn child_request(cli: &support::FakeCli, index: usize) -> Value {
    let args = cli.argv(index);
    let request = &args
        .windows(2)
        .find(|args| args[0] == drmr_server::common_protocol::JOB_ARGUMENT)
        .unwrap()[1];
    serde_json::from_slice(&std::fs::read(request).unwrap()).unwrap()
}

fn finish_child(h: &support::Harness, index: usize, job: &Value, payload: &Value) -> Value {
    let request = child_request(&h.cli, index);
    let result = Path::new(request["channels"]["result_file"].as_str().unwrap());
    let envelope = json!({"protocol_version":1,"nonce":request["nonce"],
        "operation":request["operation"]["kind"],"payload":payload});
    support::write_private(result, serde_json::to_vec(&envelope).unwrap()).unwrap();
    h.cli.release(0);
    support::terminal(h, job["job_id"].as_str().unwrap())
}

fn analyze_source(h: &support::Harness) -> Value {
    let payload = source_analysis_payload(h);
    h.cli.hold();
    let job = support::start_analysis(h, 'a');
    let ended = finish_child(h, 0, &job, &payload);
    assert_eq!(ended["state"], "succeeded", "{ended}");
    ended
}

fn source_analysis_payload(h: &support::Harness) -> Value {
    // The supervised child is synthetic: this exercises plan admission, not media qualification.
    let source = h.input.join("source.m3u8");
    std::fs::write(
        &source,
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    std::fs::write(h.input.join("segment.ts"), b"synthetic transport resource").unwrap();
    let frozen = FrozenSource::capture(&source, &Stop::default()).unwrap();
    let mut payload = analysis_payload(&h.input);
    payload["source"] = serde_json::to_value(frozen.inventory()).unwrap();
    payload["input"]["format"] = json!("hls");
    payload["actual_format"] = json!("hls-ts");
    payload["eligible_formats"] = json!(["hls-ts"]);
    payload
}

fn start_plan(h: &support::Harness, analyzed: &Value, outputs: &Value) -> Value {
    start_plan_decode(h, analyzed, outputs, false)
}

fn start_plan_decode(
    h: &support::Harness,
    analyzed: &Value,
    outputs: &Value,
    decode: bool,
) -> Value {
    let capabilities = h.client.get("/api/common/capabilities").json();
    let root = capabilities["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|root| root["label"] == "Output folder")
        .unwrap();
    let parent = h
        .client
        .get(&format!(
            "/api/common/files?root_ref={}&role=output-parent",
            root["root_ref"].as_str().unwrap()
        ))
        .json();
    let reply = h.client.post_json(
        "/api/common/jobs",
        &json!({
        "schema_version":1,"server_id":capabilities["server_id"],
        "session_id":analyzed["session_id"],"request_id":"b".repeat(32),
        "operation":{"kind":"plan","analysis_ref":analyzed["analysis_ref"],
            "output_parent_ref":parent["output_parent_ref"],"destination_name":"results",
            "outputs":outputs},"runtime":{"keys_ref":null,"max_workers":1,"decode":decode}}),
    );
    assert_eq!(reply.status, 202, "{}", reply.text());
    reply.json()
}

#[derive(Serialize)]
struct BundleIdentity<'a> {
    kind: &'a str,
    schema_version: u32,
    source: &'a SourceInventory,
    source_format: OutputFormat,
    output_root: &'a Path,
    outputs: &'a [BundleOutputIntent],
    #[serde(skip_serializing_if = "Option::is_none")]
    required_depth: Option<ValidationDepth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    processing: Option<&'a [OutputProcessing]>,
}

fn returned_plan(
    h: &support::Harness,
    outputs: Value,
    requested: [ProcessingMode; 2],
) -> BundlePlan {
    let request = child_request(&h.cli, 1);
    assert!(request["operation"].get("processing").is_none());
    let input = Path::new(request["operation"]["input_file"].as_str().unwrap());
    let source = FrozenSource::capture(input, &Stop::default()).unwrap();
    let outputs: Vec<BundleOutputIntent> = serde_json::from_value(outputs).unwrap();
    let processing = outputs
        .iter()
        .zip(requested)
        .map(|(output, requested)| {
            let segments = requested == ProcessingMode::Segments;
            OutputProcessing {
                id: output.id.clone(),
                selection: ProcessingSelection {
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
                },
            }
        })
        .collect();
    let mut plan = BundlePlan {
        kind: "execution-plan".into(),
        schema_version: 6,
        plan_id: String::new(),
        source: source.inventory().clone(),
        source_format: OutputFormat::HlsTs,
        output_root: request["operation"]["output_root"].as_str().unwrap().into(),
        outputs,
        required_depth: Some(ValidationDepth::Native),
        processing: Some(processing),
    };
    let identity = BundleIdentity {
        kind: &plan.kind,
        schema_version: plan.schema_version,
        source: &plan.source,
        source_format: plan.source_format,
        output_root: &plan.output_root,
        outputs: &plan.outputs,
        required_depth: plan.required_depth,
        processing: plan.processing.as_deref(),
    };
    plan.plan_id = hex::encode(Sha256::digest(serde_json::to_vec(&identity).unwrap()));
    let path = Path::new(request["operation"]["save_file"].as_str().unwrap());
    support::write_private(path, serde_json::to_vec(&plan).unwrap()).unwrap();
    // The returned plan has a valid ID and is valid independently of this server request.
    drmr_core::conversion::plan::read_saved(path).unwrap();
    plan
}

fn check_request_binding(
    requested: Option<ProcessingMode>,
    returned: ProcessingMode,
    accepted: bool,
) {
    let h = support::Harness::new();
    let analyzed = analyze_source(&h);
    let mut outputs = json!([
        {"id":"first","format":"hls-ts","target":"clear","drms":[]},
        {"id":"second","format":"hls-ts","target":"clear","drms":[]}]);
    if let Some(requested) = requested {
        outputs[1]["processing"] = json!(requested);
    }
    let started = start_plan(&h, &analyzed, &outputs);
    let plan = returned_plan(&h, outputs, [ProcessingMode::Auto, returned]);
    let ended = finish_child(&h, 1, &started, &serde_json::to_value(&plan).unwrap());
    assert_eq!(ended["cleanup_confirmed"], true);
    assert_eq!(ended["supervision_confirmed"], true);
    if accepted {
        assert_eq!(ended["state"], "succeeded", "{ended}");
        let view = h.client.get(&format!(
            "/api/common/plans/{}",
            ended["plan_ref"].as_str().unwrap()
        ));
        assert_eq!(view.status, 200, "{}", view.text());
        assert_eq!(view.json()["plan_id"], plan.plan_id);
        assert_eq!(
            view.json()["processing"][1]["selection"]["requested"],
            json!(returned)
        );
    } else {
        assert_eq!(ended["state"], "failed", "{ended}");
        assert_eq!(ended["error"]["code"], "result-invalid", "{ended}");
        assert!(ended["plan_ref"].is_null(), "{ended}");
        assert!(ended["report_ref"].is_null(), "{ended}");
    }
}

#[test]
fn omitted_processing_request_refuses_a_valid_segments_selection_from_the_child() {
    check_request_binding(None, ProcessingMode::Segments, false);
}

#[test]
fn omitted_processing_request_refuses_a_valid_stitch_selection_from_the_child() {
    check_request_binding(None, ProcessingMode::Stitch, false);
}

#[test]
fn omitted_processing_request_accepts_the_automatic_selection_from_the_child() {
    check_request_binding(None, ProcessingMode::Auto, true);
}

#[test]
fn explicit_processing_requests_accept_the_same_selection_from_the_child() {
    for mode in [
        ProcessingMode::Auto,
        ProcessingMode::Segments,
        ProcessingMode::Stitch,
    ] {
        check_request_binding(Some(mode), mode, true);
    }
}
