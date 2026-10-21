//! The private bridge executes ordinary handlers without exposing their result on stdout.
use drmr_core::native::{self, PrivateTempDir};
use drmr_server::common_protocol::{self, ChildProgress, ChildResult, OperationName};
use serde_json::{Value, json};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn setup() -> (PrivateTempDir, PathBuf) {
    let root = native::private_tempdir().unwrap();
    let request = root.path().join("request.json");
    for name in ["result.json", "progress.json"] {
        drop(native::create_private_new(&root.path().join(name)).unwrap());
    }
    let value = json!({
        "protocol_version":1,
        "nonce":"a".repeat(32),
        "operation":{"kind":"verify","plan_file":root.path().join("missing-private-marker.plan.json")},
        "runtime":{"keys_file":null,"max_workers":2,"tools":{"packager_bin":"packager","hls_validator_bin":"mediastreamvalidator","dash_validator_bin":"php"}},
        "channels":{"result_file":root.path().join("result.json"),"progress_file":root.path().join("progress.json")}
    });
    let mut file = native::create_private_new(&request).unwrap();
    serde_json::to_writer(&mut file, &value).unwrap();
    file.flush().unwrap();
    (root, request)
}
fn launch(path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg(common_protocol::JOB_ARGUMENT)
        .arg(path)
        .output()
        .unwrap()
}

fn assert_private_stdout(output: &Output) {
    let byte_count = output.stdout.len();
    assert_eq!(byte_count, 0, "private results must not appear on stdout");
}

fn prepare_fragmented_video(source: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let prepared = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&fixture)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-an",
            "-map_metadata",
            "-1",
            "-bitexact",
            "-movflags",
            "delay_moov+default_base_moof+frag_keyframe",
        ])
        .arg(source)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
}

#[test]
fn negative_handler_report_is_private_and_bound_to_the_attempt() {
    let (root, request) = setup();
    let output = launch(&request);
    assert_eq!(output.status.code(), Some(2));
    assert_private_stdout(&output);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("missing-private-marker"));
    let result: ChildResult = common_protocol::decode(
        &std::fs::read(root.path().join("result.json")).unwrap(),
        common_protocol::RESULT_LIMIT,
    )
    .unwrap();
    assert_eq!(result.nonce, "a".repeat(32));
    assert_eq!(result.operation, OperationName::Verify);
    assert_eq!(result.payload["status"], "failed");
    assert_eq!(result.payload["error"]["code"], "execution-plan-invalid");
    let progress: ChildProgress = common_protocol::decode(
        &std::fs::read(root.path().join("progress.json")).unwrap(),
        common_protocol::PROGRESS_LIMIT,
    )
    .unwrap();
    assert!(progress.valid(&"a".repeat(32), OperationName::Verify));
    assert!(progress.sequence > 0);
    let original = std::fs::read(root.path().join("result.json")).unwrap();
    assert_eq!(launch(&request).status.code(), Some(2));
    assert_eq!(
        std::fs::read(root.path().join("result.json")).unwrap(),
        original
    );
}

#[test]
fn malformed_private_request_never_falls_back_to_stdout() {
    let (root, request) = setup();
    let mut file = native::create_private_or_truncate(&request).unwrap();
    file.write_all(br#"{"protocol_version":1,"protocol_version":1}"#)
        .unwrap();
    drop(file);
    let output = launch(&request);
    assert_eq!(output.status.code(), Some(2));
    assert_private_stdout(&output);
    assert_eq!(
        std::fs::metadata(root.path().join("result.json"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn ordinary_json_output_keeps_the_public_cli_contract() {
    let root = native::private_tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["verify", "--plan"])
        .arg(root.path().join("missing.plan.json"))
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["command"], "verify");
    assert!(result.get("nonce").is_none());
    assert!(result.get("payload").is_none());
}

#[cfg(unix)]
#[test]
fn public_request_permissions_are_refused_without_repair() {
    use std::os::unix::fs::PermissionsExt as _;
    let (root, request) = setup();
    std::fs::set_permissions(&request, std::fs::Permissions::from_mode(0o644)).unwrap();
    let output = launch(&request);
    assert_eq!(output.status.code(), Some(2));
    assert_private_stdout(&output);
    assert_eq!(
        std::fs::metadata(&request).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(
        std::fs::metadata(root.path().join("result.json"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn all_four_handlers_share_one_saved_bundle_plan_and_fresh_media_evidence() {
    let root = native::private_tempdir().unwrap();
    let source = root.path().join("source.mp4");
    prepare_fragmented_video(&source);
    let original_source = std::fs::read(&source).unwrap();
    let specification = root.path().join("outputs.json");
    let mut file = native::create_private_new(&specification).unwrap();
    serde_json::to_writer(
        &mut file,
        &json!({"schema_version":1,"outputs":[
            {"id":"dash","format":"dash","target":"clear","drms":[]},
            {"id":"hls","format":"hls","target":"clear","drms":[]}
        ]}),
    )
    .unwrap();
    drop(file);
    let plan = root.path().join("conversion.plan.json");
    let destination = root.path().join("result");
    let mut ordinal = 0;
    let mut invoke = |operation: Value, expected_exit: i32| -> Value {
        ordinal += 1;
        let job = root.path().join(format!("job-{ordinal}"));
        native::create_private_dir_new(&job).unwrap();
        let request = job.join("request.json");
        for name in ["result.json", "progress.json"] {
            drop(native::create_private_new(&job.join(name)).unwrap());
        }
        let name = operation["kind"].as_str().unwrap().to_owned();
        let nonce = format!("{ordinal:032x}");
        let mut request_file = native::create_private_new(&request).unwrap();
        serde_json::to_writer(&mut request_file, &json!({
            "protocol_version":1,"nonce":nonce,"operation":operation,
            "runtime":{"keys_file":null,"max_workers":2,"tools":{"packager_bin":"packager","hls_validator_bin":"mediastreamvalidator","dash_validator_bin":"php"}},
            "channels":{"result_file":job.join("result.json"),"progress_file":job.join("progress.json")}
        })).unwrap();
        drop(request_file);
        let output = launch(&request);
        assert_eq!(
            output.status.code(),
            Some(expected_exit),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_private_stdout(&output);
        let report: ChildResult = common_protocol::decode(
            &std::fs::read(job.join("result.json")).unwrap(),
            common_protocol::RESULT_LIMIT,
        )
        .unwrap();
        assert_eq!(report.nonce, nonce);
        assert_eq!(report.operation.as_str(), name);
        report.payload
    };
    let analyzed = invoke(
        json!({"kind":"analyze","source":{"kind":"local","input_file":source}}),
        0,
    );
    assert_eq!(analyzed["input_admitted"], true);
    let planned = invoke(
        json!({"kind":"plan","input_file":source,"output_root":destination,"save_file":plan,"output_spec_file":specification}),
        0,
    );
    assert_eq!(planned["schema_version"], 2);
    assert_eq!(planned["outputs"].as_array().unwrap().len(), 2);
    let saved_bytes = std::fs::read(&plan).unwrap();
    let converted = invoke(json!({"kind":"convert","plan_file":plan}), 0);
    let verified = invoke(json!({"kind":"verify","plan_file":plan}), 0);
    assert_eq!(converted["plan_id"], planned["plan_id"]);
    assert_eq!(verified["plan_id"], planned["plan_id"]);
    assert_eq!(verified["passed"], true);
    for output in verified["outputs"].as_array().unwrap() {
        let proof = &output["verification"];
        assert_eq!(proof["passed"], true);
        assert_eq!(proof["media_verified"], true);
        assert_eq!(proof["license_playback_verified"], false);
        assert_eq!(proof["checks"]["stream_metadata"], "pass");
        for name in [
            "packet_payloads_equal",
            "packet_timing_equal",
            "decoded_content_equal",
        ] {
            assert_eq!(proof["checks"][name], true);
        }
    }
    let manifest = destination.join("dash/manifest.mpd");
    let bytes = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"invalid output manifest").unwrap();
    let failed = invoke(json!({"kind":"verify","plan_file":plan}), 1);
    assert_eq!(failed["status"], "failed");
    assert_ne!(failed["passed"], true);
    std::fs::write(&manifest, bytes).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), original_source);
    assert_eq!(std::fs::read(&plan).unwrap(), saved_bytes);
}
