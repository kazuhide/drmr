//! One retained source and one atomic publication cover every requested result.
use drmr_core::{
    conversion::{
        Converter, HlsInputKeys, InputKeys, VerificationSource,
        conformance::ManifestValidationTools,
        key_file::BundleOperatorKeys,
        plan::{
            self, SavedPlan,
            bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
        },
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
    .with_decode(true)
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
            "-map_metadata",
            "-1",
            "-bitexact",
            "-movflags",
            "delay_moov+default_base_moof+frag_keyframe",
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

fn outputs() -> Vec<BundleOutputIntent> {
    [OutputFormat::Dash, OutputFormat::Hls]
        .into_iter()
        .map(|format| BundleOutputIntent {
            id: format.as_str().to_owned(),
            format,
            target: TargetProtection::Clear,
            drms: Vec::new(),
            conformance: ManifestValidation::None,
        })
        .collect()
}

fn saved(input: &Path, output: &Path, outputs: &[BundleOutputIntent]) -> plan::bundle::BundlePlan {
    converter()
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output_root: output,
            outputs,
        })
        .unwrap()
}

fn assert_no_stages(root: &Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        assert!(
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".drmr-convert-")
        );
    }
}

enum AfterFirstPublication {
    Cancel(drmr_core::concurrency::Stop),
    ChangeSource(PathBuf),
    #[cfg(unix)]
    RedirectOutputParent {
        parent: PathBuf,
        displaced: PathBuf,
    },
}

struct PublicationAction {
    completed: AtomicUsize,
    action: AfterFirstPublication,
}

impl drmr_core::events::Sink for PublicationAction {
    fn emit(&self, record: &drmr_core::events::Record) {
        if record.get("activity").and_then(serde_json::Value::as_str) == Some("publication")
            && record.get("state").and_then(serde_json::Value::as_str) == Some("closed")
            && self.completed.fetch_add(1, Ordering::SeqCst) == 0
        {
            match &self.action {
                AfterFirstPublication::Cancel(stop) => stop.set(),
                AfterFirstPublication::ChangeSource(path) => {
                    let mut bytes = std::fs::read(path).unwrap();
                    let last = bytes.len() - 1;
                    bytes[last] ^= 1;
                    std::fs::write(path, bytes).unwrap();
                }
                #[cfg(unix)]
                AfterFirstPublication::RedirectOutputParent { parent, displaced } => {
                    std::fs::rename(parent, displaced).unwrap();
                    std::fs::create_dir(parent).unwrap();
                    std::fs::create_dir(parent.join("result")).unwrap();
                    std::fs::write(parent.join("result/foreign"), b"must remain").unwrap();
                }
            }
        }
    }
}

#[test]
fn one_private_plan_publishes_and_independently_verifies_both_presentations() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let output = root.path().join("result");
    video(&input);
    let original = std::fs::read(&input).unwrap();
    let plan = converter()
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output_root: &output,
            outputs: &outputs(),
        })
        .unwrap();
    assert_eq!(plan.schema_version, 4);
    assert_eq!(
        plan.minimum_depth(),
        drmr_core::conversion::ValidationDepth::Decoded
    );
    let saved = root.path().join("bundle.plan.json");
    plan::write_bundle(&saved, &plan).unwrap();
    let SavedPlan::Bundle(loaded) = plan::read_saved(&saved).unwrap() else {
        panic!("version two plan was not dispatched to the bundle schema");
    };
    assert_eq!(loaded, plan);
    assert!(!output.exists());
    let keys = BundleOperatorKeys::default();
    let tools = ManifestValidationTools::default();
    let report = converter()
        .execute_bundle_plan(&loaded, &keys, &tools)
        .unwrap();
    assert_eq!(report.outputs.len(), 2);
    assert!(
        report
            .outputs
            .iter()
            .all(|result| result.conversion.verification.passed)
    );
    assert_no_stages(root.path());
    assert!(output.join("dash/manifest.mpd").is_file());
    assert!(output.join("hls/master.m3u8").is_file());
    let checked = converter()
        .verify_bundle_plan(&loaded, &keys, &tools)
        .unwrap();
    assert!(checked.passed && checked.source_unchanged);
    assert_eq!(checked.outputs.len(), 2);
    assert!(
        checked
            .outputs
            .iter()
            .all(|result| result.output_matches_plan && result.verification.passed)
    );
    assert_eq!(std::fs::read(input).unwrap(), original);
    assert!(
        converter()
            .execute_bundle_plan(&loaded, &keys, &tools)
            .is_err()
    );
    let master = output.join("hls/master.m3u8");
    std::fs::remove_file(&master).unwrap();
    assert!(
        converter()
            .verify_bundle_plan(&loaded, &keys, &tools)
            .is_err()
    );
    assert!(output.join("dash/manifest.mpd").is_file());
}

#[test]
fn failure_of_the_second_required_manifest_check_publishes_neither_result() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let output = root.path().join("result");
    video(&input);
    let original = std::fs::read(&input).unwrap();
    let mut outputs = outputs();
    outputs[1].conformance = ManifestValidation::Required;
    let plan = saved(&input, &output, &outputs);
    let observed = Arc::new(drmr_core::events::Collector::default());
    let error = converter()
        .with_activity_sink(observed.clone())
        .execute_bundle_plan(
            &plan,
            &BundleOperatorKeys::default(),
            &ManifestValidationTools::default(),
        )
        .unwrap_err();
    assert!(error.0.starts_with("manifest-"), "{error}");
    let completed = observed
        .records()
        .iter()
        .filter(|record| {
            record.get("activity").and_then(serde_json::Value::as_str) == Some("publication")
                && record.get("state").and_then(serde_json::Value::as_str) == Some("closed")
        })
        .count();
    assert!(
        completed >= 2,
        "both private children must reach publication before the required check fails"
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert_no_stages(root.path());
}

#[test]
fn cancellation_after_a_complete_child_does_not_publish_a_partial_bundle() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let output = root.path().join("result");
    video(&input);
    let plan = saved(&input, &output, &outputs());
    let stop = drmr_core::concurrency::Stop::new();
    let observed = Arc::new(PublicationAction {
        completed: AtomicUsize::new(0),
        action: AfterFirstPublication::Cancel(stop.clone()),
    });
    let error = converter()
        .with_stop(stop)
        .with_activity_sink(observed.clone())
        .execute_bundle_plan(
            &plan,
            &BundleOperatorKeys::default(),
            &ManifestValidationTools::default(),
        )
        .unwrap_err();
    assert_eq!(error.0, "cancelled");
    assert!(observed.completed.load(Ordering::SeqCst) >= 1);
    assert!(!output.exists());
    assert_no_stages(root.path());
}

#[test]
fn changed_original_after_a_complete_child_cannot_publish_either_result() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let output = root.path().join("result");
    video(&input);
    let plan = saved(&input, &output, &outputs());
    let observed = Arc::new(PublicationAction {
        completed: AtomicUsize::new(0),
        action: AfterFirstPublication::ChangeSource(input.clone()),
    });
    let error = converter()
        .with_activity_sink(observed.clone())
        .execute_bundle_plan(
            &plan,
            &BundleOperatorKeys::default(),
            &ManifestValidationTools::default(),
        )
        .unwrap_err();
    assert_eq!(error.0, "execution-plan-source-changed");
    assert!(observed.completed.load(Ordering::SeqCst) >= 1);
    assert!(input.exists());
    assert!(!output.exists());
    assert_no_stages(root.path());
}

#[test]
fn an_existing_bundle_and_foreign_entries_are_never_replaced_or_removed() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let output = root.path().join("result");
    video(&input);
    let plan = saved(&input, &output, &outputs());
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("foreign"), b"must remain").unwrap();
    assert!(
        converter()
            .execute_bundle_plan(
                &plan,
                &BundleOperatorKeys::default(),
                &ManifestValidationTools::default(),
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read(output.join("foreign")).unwrap(),
        b"must remain"
    );
    assert_no_stages(root.path());
}

#[test]
#[cfg(unix)]
fn a_redirected_parent_after_one_child_cannot_publish_or_remove_foreign_entries() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let parent = root.path().join("output-parent");
    let displaced = root.path().join("displaced-parent");
    std::fs::create_dir(&parent).unwrap();
    let output = parent.join("result");
    video(&input);
    let plan = saved(&input, &output, &outputs());
    let observed = Arc::new(PublicationAction {
        completed: AtomicUsize::new(0),
        action: AfterFirstPublication::RedirectOutputParent {
            parent: parent.clone(),
            displaced: displaced.clone(),
        },
    });
    let error = converter()
        .with_activity_sink(observed.clone())
        .execute_bundle_plan(
            &plan,
            &BundleOperatorKeys::default(),
            &ManifestValidationTools::default(),
        )
        .unwrap_err();
    assert_eq!(error.0, "execution-plan-destination-unsafe");
    assert!(observed.completed.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        std::fs::read(output.join("foreign")).unwrap(),
        b"must remain"
    );
    assert!(!displaced.join("result").exists());
    // Restore the original namespace so the exclusively owned test root can be removed.
    std::fs::rename(&parent, root.path().join("foreign-parent")).unwrap();
    std::fs::rename(&displaced, &parent).unwrap();
}

#[test]
fn legacy_saved_plan_verification_checks_explicit_playready_metadata_again() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let output = root.path().join("result");
    video(&input);
    let plan = converter()
        .create_execution_plan(plan::ExecutionPlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output: &output,
            format: Some(OutputFormat::Dash),
            target: TargetProtection::Cbcs,
        })
        .unwrap();
    let keys = |name: &str, license: &str| {
        let path = root.path().join(name);
        let bytes = serde_json::json!({"schema_version":1,"output":{
            "kid":"000102030405060708090a0b0c0d0e0f", "key":"101112131415161718191a1b1c1d1e1f",
            "signaling":{"system":"playready","license_url":license}
        }})
        .to_string();
        drmr_core::native::create_private_new(&path)
            .unwrap()
            .write_all(bytes.as_bytes())
            .unwrap();
        drmr_core::conversion::key_file::read(&path).unwrap()
    };
    let original = keys("original.json", "https://example.invalid/license-one");
    let changed = keys("changed.json", "https://example.invalid/license-two");
    converter().execute_plan(&plan, &original).unwrap();
    let checked = converter().verify_plan(&plan, &original).unwrap();
    assert!(checked.source_unchanged && checked.output_matches_plan && checked.verification.passed);
    assert_eq!(
        converter().verify_plan(&plan, &changed).unwrap_err().0,
        "output-signaling-verification-failed"
    );
}
