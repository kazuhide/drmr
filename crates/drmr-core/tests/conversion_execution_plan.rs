//! Saved intent is independently revalidated against original bytes and actual output protection.
use drmr_core::{
    conversion::{
        Converter, HlsInputKeys, InputKeys, VerificationSource,
        key_file::OperatorKeys,
        plan::{self, ExecutionPlanRequest},
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{io::Write as _, path::Path, sync::Arc};

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
}
fn video(path: &Path) {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(clip)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-an",
            "-movflags",
            "+frag_keyframe+empty_moov",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn saved(input: &Path, output: &Path, target: TargetProtection) -> plan::ExecutionPlan {
    converter()
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output,
            format: Some(OutputFormat::Mp4),
            target,
        })
        .unwrap()
}

#[test]
fn saved_plan_reopens_offline_and_verifies_original_media() {
    let dir = drmr_core::native::private_tempdir().unwrap();
    let input = dir.path().join("source.mp4");
    let output = dir.path().join("result.mp4");
    video(&input);
    let original = std::fs::read(&input).unwrap();
    let plan = saved(&input, &output, TargetProtection::Clear);
    let path = dir.path().join("conversion.plan.json");
    plan::write_new(&path, &plan).unwrap();
    let loaded = plan::read(&path).unwrap();
    assert_eq!(loaded.plan_id, plan.plan_id);
    assert!(!output.exists());
    let converted = converter()
        .execute_plan(&loaded, &OperatorKeys::default())
        .unwrap();
    assert!(converted.verification.passed);
    let verified = converter()
        .verify_plan(&loaded, &OperatorKeys::default())
        .unwrap();
    assert!(
        verified.source_unchanged && verified.output_matches_plan && verified.verification.passed
    );
    assert_eq!(verified.plan_id, plan.plan_id);
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert!(
        converter()
            .execute_plan(&loaded, &OperatorKeys::default())
            .is_err()
    );
}

#[test]
fn changed_source_and_changed_plan_cannot_publish_a_result() {
    let dir = drmr_core::native::private_tempdir().unwrap();
    let input = dir.path().join("source.mp4");
    let output = dir.path().join("result.mp4");
    video(&input);
    let plan = saved(&input, &output, TargetProtection::Clear);
    let mut changed = plan.clone();
    changed.intent.target = TargetProtection::Cbcs;
    assert_eq!(
        converter()
            .execute_plan(&changed, &OperatorKeys::default())
            .unwrap_err()
            .0,
        "execution-plan-invalid"
    );
    let mut bytes = std::fs::read(&input).unwrap();
    let index = bytes.len() - 1;
    bytes[index] ^= 1;
    std::fs::write(&input, bytes).unwrap();
    assert_eq!(
        converter()
            .execute_plan(&plan, &OperatorKeys::default())
            .unwrap_err()
            .0,
        "execution-plan-source-changed"
    );
    assert!(!output.exists());
    assert!(input.exists());
}

#[test]
fn same_content_clear_replacement_fails_a_cbcs_plan_even_when_media_matches() {
    let dir = drmr_core::native::private_tempdir().unwrap();
    let input = dir.path().join("source.mp4");
    let output = dir.path().join("result.mp4");
    video(&input);
    let encrypted = saved(&input, &output, TargetProtection::Cbcs);
    let clear = saved(&input, &output, TargetProtection::Clear);
    converter()
        .execute_plan(&clear, &OperatorKeys::default())
        .unwrap();
    let report = converter()
        .verify_plan(&encrypted, &OperatorKeys::default())
        .unwrap();
    assert!(report.source_unchanged && report.verification.passed);
    assert!(!report.output_matches_plan);
}

#[test]
fn strict_private_plan_read_and_no_replace_save_keep_existing_files() {
    let dir = drmr_core::native::private_tempdir().unwrap();
    let input = dir.path().join("source.mp4");
    let output = dir.path().join("result.mp4");
    video(&input);
    let plan = saved(&input, &output, TargetProtection::Clear);
    let path = dir.path().join("conversion.plan.json");
    plan::write_new(&path, &plan).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        plan::write_new(&path, &plan).unwrap_err().0,
        "execution-plan-exists"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut untrusted = serde_json::to_value(&plan).unwrap();
    untrusted["command"] = serde_json::json!("confidential-sentinel");
    let bad = dir.path().join("invalid.json");
    drmr_core::native::create_private_new(&bad)
        .unwrap()
        .write_all(&serde_json::to_vec(&untrusted).unwrap())
        .unwrap();
    assert_eq!(plan::read(&bad).unwrap_err().0, "execution-plan-invalid");
    assert!(!output.exists());
    assert!(!format!("{plan:?}").contains(input.to_str().unwrap()));
}
