//! Legacy plans retain exact packaging requests even when an HLS bound matches them.
mod common;
#[path = "support/legacy_hls_source_policy.rs"]
mod source_policy;

use drmr_core::{
    conversion::{
        ConversionRequest, Converter, HlsInputKeys, InputKeys, ProcessingMode, VerificationRequest,
        VerificationSource,
        conformance::ManifestValidationTools,
        key_file::{BundleOperatorKeys, OperatorKeys},
        plan::{
            self, ExecutionPlan, ExecutionPlanRequest, SavedPlan,
            bundle::{BundleOutputIntent, BundlePlan, BundlePlanRequest, ManifestValidation},
        },
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
}

#[test]
fn main10_boundary_fragments_do_not_change_the_saved_packaging_cadence() {
    let root = common::private_tempdir().unwrap();
    let seed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/hevc-main10.mp4");
    let keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let native = || {
        Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "packager",
            "must-not-run-ffmpeg",
        )
        .with_ffprobe("must-not-run-ffprobe")
    };
    let command = native();
    let source_root = root.path().join("source");
    let source = source_root.join("master.m3u8");
    let first = command
        .convert(ConversionRequest {
            input: &seed,
            output: &source_root,
            format: OutputFormat::Hls,
            target: TargetProtection::Clear,
            input_keys: &keys,
            hls_input_keys: &hls_keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    assert!(first.verification.passed);
    let original = common::snapshot_tree(&source_root);
    for mode in [ProcessingMode::Auto, ProcessingMode::Stitch] {
        for format in [OutputFormat::Dash, OutputFormat::Hls] {
            let output = root
                .path()
                .join(format!("{}-{}", mode.as_str(), format.as_str()));
            let command = native().with_processing(mode);
            let saved = command
                .create_execution_plan(ExecutionPlanRequest {
                    input: VerificationSource {
                        input: &source,
                        input_keys: &keys,
                        hls_keys: &hls_keys,
                    },
                    output: &output,
                    format: Some(format),
                    target: TargetProtection::Clear,
                })
                .unwrap();
            assert_eq!(
                saved.processing.as_ref().unwrap().segment_duration_ms,
                Some(2000)
            );
            let path = root
                .path()
                .join(format!("{}-{}.json", mode.as_str(), format.as_str()));
            plan::write_new(&path, &saved).unwrap();
            let reopened = plan::read(&path).unwrap();
            assert!(
                command
                    .execute_plan(&reopened, &OperatorKeys::default())
                    .unwrap()
                    .verification
                    .passed
            );
            assert!(
                command
                    .verify_plan(&reopened, &OperatorKeys::default())
                    .unwrap()
                    .verification
                    .passed
            );
            let final_path = output.join(if format == OutputFormat::Dash {
                "manifest.mpd"
            } else {
                "master.m3u8"
            });
            assert!(
                command
                    .verify(VerificationRequest {
                        source: VerificationSource {
                            input: &seed,
                            input_keys: &keys,
                            hls_keys: &hls_keys
                        },
                        output: VerificationSource {
                            input: &final_path,
                            input_keys: &keys,
                            hls_keys: &hls_keys
                        },
                    })
                    .unwrap()
                    .passed
            );
            assert_eq!(common::snapshot_tree(&source_root), original);
        }
    }
}

fn source(root: &Path) -> PathBuf {
    fs::create_dir(root).unwrap();
    let input = root.join("media.m3u8");
    let result = std::process::Command::new("ffmpeg")
        .current_dir(root)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x48:rate=24",
            "-t",
            "4",
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-threads",
            "1",
            "-g",
            "24",
            "-keyint_min",
            "24",
            "-sc_threshold",
            "0",
            "-bf",
            "0",
            "-hls_time",
            "1",
            "-hls_segment_type",
            "fmp4",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg("segment-%d.m4s")
        .arg("media.m3u8")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "one-second fixture generation failed"
    );
    let text = fs::read_to_string(&input).unwrap();
    assert!(text.contains("#EXT-X-MAP:URI=\"init.mp4\""));
    let initialization =
        fs::symlink_metadata(root.join("init.mp4")).expect("fixture initialization is missing");
    assert!(
        initialization.is_file() && initialization.len() > 0,
        "fixture initialization must be a nonempty regular file"
    );
    assert!(text.contains("#EXT-X-TARGETDURATION:1\n"));
    fs::write(
        &input,
        text.replace("#EXT-X-TARGETDURATION:1\n", "#EXT-X-TARGETDURATION:2\n"),
    )
    .unwrap();
    input
}

fn with_identity<T: serde::Serialize + serde::de::DeserializeOwned>(plan: &T) -> T {
    let mut value = serde_json::to_value(plan).unwrap();
    value.as_object_mut().unwrap().shift_remove("plan_id");
    let identity = hex::encode(Sha256::digest(serde_json::to_vec(&value).unwrap()));
    value["plan_id"] = identity.into();
    serde_json::from_value(value).unwrap()
}

fn legacy_single(saved: &ExecutionPlan, schema: u32, output: PathBuf) -> ExecutionPlan {
    let mut old = saved.clone();
    old.schema_version = schema;
    old.intent.output = output;
    old.output_options = None;
    if schema == 1 {
        old.required_depth = None;
    }
    if schema < 5 {
        old.processing = None;
    } else {
        old.processing.as_mut().unwrap().segment_duration_ms = Some(2000);
    }
    with_identity(&old)
}

fn legacy_bundle(saved: &BundlePlan, schema: u32, output: PathBuf) -> BundlePlan {
    let mut old = saved.clone();
    old.schema_version = schema;
    old.output_root = output;
    old.output_options = None;
    if schema == 2 {
        old.required_depth = None;
    }
    if schema < 6 {
        old.processing = None;
    } else {
        old.processing.as_mut().unwrap()[0]
            .selection
            .segment_duration_ms = Some(2000);
    }
    with_identity(&old)
}

fn assert_two_second_output(root: &Path) {
    let master = fs::read_to_string(root.join("master.m3u8")).unwrap();
    let child = master
        .lines()
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    let text = fs::read_to_string(root.join(child)).unwrap();
    let durations: Vec<f64> = text
        .lines()
        .filter_map(|line| line.strip_prefix("#EXTINF:"))
        .map(|line| line.split(',').next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(
        durations,
        [2.0, 2.0],
        "a saved two-second request must not become one second"
    );
}

fn legacy_single_keeps_two_seconds(schema: u32) {
    let root = common::private_tempdir().unwrap();
    let source = source(&root.path().join("source"));
    let original = common::snapshot_tree(source.parent().unwrap());
    let command = converter();
    let input_keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let saved = command
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input: &source,
                input_keys: &input_keys,
                hls_keys: &hls_keys,
            },
            output: &root.path().join("new"),
            format: Some(OutputFormat::Hls),
            target: TargetProtection::Clear,
        })
        .unwrap();
    let old = legacy_single(
        &saved,
        schema,
        root.path()
            .canonicalize()
            .unwrap()
            .join(format!("legacy-{schema}")),
    );
    let path = root.path().join(format!("legacy-{schema}.json"));
    plan::write_new(&path, &old).unwrap();
    let reopened = plan::read(&path).unwrap();
    assert_eq!(
        serde_json::to_vec(&old).unwrap(),
        serde_json::to_vec(&reopened).unwrap()
    );
    assert!(
        command
            .execute_plan(&reopened, &OperatorKeys::default())
            .unwrap()
            .verification
            .passed
    );
    assert_two_second_output(&old.intent.output);
    assert!(
        command
            .verify_plan(&reopened, &OperatorKeys::default())
            .unwrap()
            .verification
            .passed
    );
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), original);
}

fn legacy_bundle_keeps_two_seconds(schema: u32) {
    let root = common::private_tempdir().unwrap();
    let source = source(&root.path().join("source"));
    let original = common::snapshot_tree(source.parent().unwrap());
    let command = converter();
    let outputs = [BundleOutputIntent {
        id: "hls".into(),
        format: OutputFormat::Hls,
        target: TargetProtection::Clear,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
        processing: None,
        dash_addressing: None,
    }];
    let input_keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let saved = command
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &source,
                input_keys: &input_keys,
                hls_keys: &hls_keys,
            },
            output_root: &root.path().join("new"),
            outputs: &outputs,
        })
        .unwrap();
    let keys = BundleOperatorKeys::default();
    let tools = ManifestValidationTools::default();
    let old = legacy_bundle(
        &saved,
        schema,
        root.path()
            .canonicalize()
            .unwrap()
            .join(format!("legacy-{schema}")),
    );
    let path = root.path().join(format!("legacy-{schema}.json"));
    plan::write_bundle(&path, &old).unwrap();
    let SavedPlan::Bundle(reopened) = plan::read_saved(&path).unwrap() else {
        panic!("bundle plan required")
    };
    assert_eq!(
        serde_json::to_vec(&old).unwrap(),
        serde_json::to_vec(&reopened).unwrap()
    );
    assert!(
        command
            .execute_bundle_plan(&reopened, &keys, &tools)
            .unwrap()
            .outputs[0]
            .conversion
            .verification
            .passed
    );
    assert_two_second_output(&old.output_root.join("hls"));
    assert!(
        command
            .verify_bundle_plan(&reopened, &keys, &tools)
            .unwrap()
            .passed
    );
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), original);
}

macro_rules! legacy_regression {
    ($name:ident, $run:ident, $schema:literal) => {
        #[test]
        fn $name() {
            $run($schema);
        }
    };
}
legacy_regression!(
    single_schema_one_keeps_two_seconds,
    legacy_single_keeps_two_seconds,
    1
);
legacy_regression!(
    single_schema_three_keeps_two_seconds,
    legacy_single_keeps_two_seconds,
    3
);
legacy_regression!(
    single_schema_five_keeps_two_seconds,
    legacy_single_keeps_two_seconds,
    5
);
legacy_regression!(
    bundle_schema_two_keeps_two_seconds,
    legacy_bundle_keeps_two_seconds,
    2
);
legacy_regression!(
    bundle_schema_four_keeps_two_seconds,
    legacy_bundle_keeps_two_seconds,
    4
);
legacy_regression!(
    bundle_schema_six_keeps_two_seconds,
    legacy_bundle_keeps_two_seconds,
    6
);

fn assert_one_second_output(root: &Path) {
    let mut pending = vec![root.to_path_buf()];
    let mut media = 0;
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "m3u8")
            {
                let text = fs::read_to_string(path).unwrap();
                if text.contains("#EXT-X-TARGETDURATION:") {
                    assert!(text.contains("#EXT-X-TARGETDURATION:2\n"));
                    let durations: Vec<_> = text
                        .lines()
                        .filter_map(|line| line.strip_prefix("#EXTINF:"))
                        .collect();
                    assert_eq!(durations.len(), 4);
                    assert!(durations.iter().all(|line| {
                        (line.split(',').next().unwrap().parse::<f64>().unwrap() - 1.0).abs()
                            < f64::EPSILON
                    }));
                    media += 1;
                }
            }
        }
    }
    assert_eq!(media, 1);
}

#[test]
fn new_single_plan_saves_the_actual_request_and_replays_the_source_bound_and_layout() {
    let root = common::private_tempdir().unwrap();
    let source = source(&root.path().join("source"));
    let original = common::snapshot_tree(source.parent().unwrap());
    let keys = OperatorKeys::default();
    let saved = converter()
        .with_new_output_defaults()
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input: &source,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output: &root.path().join("result"),
            format: Some(OutputFormat::Hls),
            target: TargetProtection::Clear,
        })
        .unwrap();
    assert_eq!(saved.schema_version, 9);
    assert_eq!(saved.processing.unwrap().segment_duration_ms, Some(1000));
    assert!(saved.output_options.as_ref().unwrap().layout.is_some());
    let file = root.path().join("new.plan.json");
    plan::write_new(&file, &saved).unwrap();
    let reopened = plan::read(&file).unwrap();
    assert_eq!(
        serde_json::to_vec(&saved).unwrap(),
        serde_json::to_vec(&reopened).unwrap()
    );
    assert!(
        converter()
            .execute_plan(&reopened, &keys)
            .unwrap()
            .verification
            .passed
    );
    assert_one_second_output(&saved.intent.output);
    assert!(
        converter()
            .verify_plan(&reopened, &keys)
            .unwrap()
            .verification
            .passed
    );
    let mut fixed = reopened;
    fixed.intent.output = root.path().canonicalize().unwrap().join("fixed-request");
    fixed.processing.as_mut().unwrap().segment_duration_ms = Some(2000);
    let fixed = with_identity(&fixed);
    assert!(
        converter()
            .execute_plan(&fixed, &keys)
            .unwrap()
            .verification
            .passed
    );
    assert_two_second_output(&fixed.intent.output);
    assert!(
        converter()
            .verify_plan(&fixed, &keys)
            .unwrap()
            .verification
            .passed
    );
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), original);
}

#[test]
fn a_mixed_new_bundle_uses_segment_admission_facts_for_its_stitch_request() {
    let root = common::private_tempdir().unwrap();
    let source = source(&root.path().join("source"));
    let original = common::snapshot_tree(source.parent().unwrap());
    let outputs =
        [ProcessingMode::Segments, ProcessingMode::Stitch].map(|mode| BundleOutputIntent {
            id: mode.as_str().into(),
            format: OutputFormat::Hls,
            target: TargetProtection::Clear,
            drms: Vec::new(),
            conformance: ManifestValidation::None,
            processing: Some(mode),
            dash_addressing: None,
        });
    let keys = BundleOperatorKeys::default();
    let saved = converter()
        .with_new_output_defaults()
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &source,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output_root: &root.path().join("result"),
            outputs: &outputs,
        })
        .unwrap();
    assert_eq!(saved.schema_version, 10);
    assert_eq!(
        saved.processing.as_ref().unwrap()[0]
            .selection
            .segment_duration_ms,
        None
    );
    assert_eq!(
        saved.processing.as_ref().unwrap()[1]
            .selection
            .segment_duration_ms,
        Some(1000)
    );
    assert!(
        saved
            .output_options
            .as_ref()
            .unwrap()
            .iter()
            .all(|item| item.options.layout.is_some())
    );
    let file = root.path().join("new.bundle.json");
    plan::write_bundle(&file, &saved).unwrap();
    let SavedPlan::Bundle(reopened) = plan::read_saved(&file).unwrap() else {
        panic!("bundle plan required")
    };
    let tools = ManifestValidationTools::default();
    let result = converter()
        .execute_bundle_plan(&reopened, &keys, &tools)
        .unwrap();
    assert!(
        result
            .outputs
            .iter()
            .all(|output| output.conversion.verification.passed)
    );
    for output in &saved.outputs {
        assert_one_second_output(&saved.output_root.join(&output.id));
    }
    assert!(
        converter()
            .verify_bundle_plan(&reopened, &keys, &tools)
            .unwrap()
            .passed
    );
    let mut fixed = reopened;
    fixed.output_root = root.path().canonicalize().unwrap().join("fixed-request");
    fixed.processing.as_mut().unwrap()[1]
        .selection
        .segment_duration_ms = Some(2000);
    let fixed = with_identity(&fixed);
    let result = converter()
        .execute_bundle_plan(&fixed, &keys, &tools)
        .unwrap();
    assert!(
        result
            .outputs
            .iter()
            .all(|output| output.conversion.verification.passed)
    );
    assert_one_second_output(&fixed.output_root.join("segments"));
    assert_two_second_output(&fixed.output_root.join("stitch"));
    assert!(
        converter()
            .verify_bundle_plan(&fixed, &keys, &tools)
            .unwrap()
            .passed
    );
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), original);
}
