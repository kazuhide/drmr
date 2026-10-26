//! Replay keeps both source admission and final HLS headers on the saved generation's policy.
use super::*;
use drmr_core::{
    concurrency::Stop,
    conversion::{ConversionRequest, FrozenSource},
};

fn audio_source(root: &Path) -> PathBuf {
    fs::create_dir(root).unwrap();
    let input = root.join("media.m3u8");
    let output = std::process::Command::new("ffmpeg")
        .current_dir(root)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=997:sample_rate=44100:duration=14",
            "-c:a",
            "aac",
            "-threads",
            "1",
            "-hls_segment_type",
            "fmp4",
            "-hls_time",
            "6",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg("segment-%d.m4s")
        .arg("media.m3u8")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fractional AAC fixture generation failed"
    );
    let text = fs::read_to_string(&input).unwrap();
    assert!(text.contains("#EXT-X-MAP:URI=\"init.mp4\""));
    let initialization =
        fs::symlink_metadata(root.join("init.mp4")).expect("fixture initialization is missing");
    assert!(
        initialization.is_file() && initialization.len() > 0,
        "fixture initialization must be a nonempty regular file"
    );
    assert!(text.contains("#EXT-X-TARGETDURATION:6\n"));
    assert!(
        text.lines()
            .filter_map(|line| line.strip_prefix("#EXTINF:"))
            .any(|value| {
                let duration = value.split(',').next().unwrap().parse::<f64>().unwrap();
                duration > 6.0 && duration < 6.5
            })
    );
    input
}

fn plans(input: &Path, root: &Path, mode: ProcessingMode) -> (ExecutionPlan, BundlePlan) {
    let command = converter().with_processing(mode);
    let keys = OperatorKeys::default();
    let source = VerificationSource {
        input,
        input_keys: &keys.input,
        hls_keys: &keys.hls_input,
    };
    let single = command
        .create_execution_plan(ExecutionPlanRequest {
            input: source,
            output: &root.join("new-single"),
            format: Some(OutputFormat::Hls),
            target: TargetProtection::Clear,
        })
        .unwrap();
    let outputs = [BundleOutputIntent {
        id: "hls".into(),
        format: OutputFormat::Hls,
        target: TargetProtection::Clear,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
        processing: None,
        dash_addressing: None,
    }];
    let bundle = command
        .create_bundle_plan(BundlePlanRequest {
            input: source,
            output_root: &root.join("new-bundle"),
            outputs: &outputs,
        })
        .unwrap();
    (single, bundle)
}

fn old_single(saved: &ExecutionPlan, input: &Path, output: &Path) -> ExecutionPlan {
    let mut old = saved.clone();
    old.schema_version = 5;
    old.output_options = None;
    output.clone_into(&mut old.intent.output);
    old.source = FrozenSource::capture(input, &Stop::new())
        .unwrap()
        .inventory()
        .clone();
    with_identity(&old)
}

fn old_bundle(saved: &BundlePlan, input: &Path, output: &Path, schema: u32) -> BundlePlan {
    let mut old = saved.clone();
    old.schema_version = schema;
    old.output_options = None;
    output.clone_into(&mut old.output_root);
    old.source = FrozenSource::capture(input, &Stop::new())
        .unwrap()
        .inventory()
        .clone();
    if schema == 2 {
        old.required_depth = None;
    }
    if schema < 6 {
        old.processing = None;
    }
    with_identity(&old)
}

fn assert_targets(root: &Path, expected: u64) {
    let mut pending = vec![root.to_path_buf()];
    let mut found = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "m3u8")
            {
                let text = fs::read_to_string(path).unwrap();
                if let Some(target) = text
                    .lines()
                    .find_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
                {
                    assert_eq!(target.parse::<u64>().unwrap(), expected);
                    found += 1;
                }
            }
        }
    }
    assert!(found > 0);
}

fn execute_single(plan: &ExecutionPlan, file: &Path, expected: u64) {
    plan::write_new(file, plan).unwrap();
    let loaded = plan::read(file).unwrap();
    let keys = OperatorKeys::default();
    let command = converter();
    assert!(
        command
            .execute_plan(&loaded, &keys)
            .unwrap()
            .verification
            .passed
    );
    assert_targets(&loaded.intent.output, expected);
    assert!(
        command
            .verify_plan(&loaded, &keys)
            .unwrap()
            .verification
            .passed
    );
}

fn execute_bundle(plan: &BundlePlan, file: &Path, expected: u64) {
    plan::write_bundle(file, plan).unwrap();
    let SavedPlan::Bundle(loaded) = plan::read_saved(file).unwrap() else {
        panic!("bundle required")
    };
    let keys = BundleOperatorKeys::default();
    let tools = ManifestValidationTools::default();
    let command = converter();
    assert!(
        command
            .execute_bundle_plan(&loaded, &keys, &tools)
            .unwrap()
            .outputs[0]
            .conversion
            .verification
            .passed
    );
    assert_targets(&loaded.output_root, expected);
    assert!(
        command
            .verify_bundle_plan(&loaded, &keys, &tools)
            .unwrap()
            .passed
    );
}

#[test]
fn legacy_segment_plans_keep_generated_seven_while_new_plans_inherit_six() {
    let root = common::private_tempdir().unwrap();
    let input = audio_source(&root.path().join("source"));
    let before = common::snapshot_tree(input.parent().unwrap());
    let (single, bundle) = plans(&input, root.path(), ProcessingMode::Segments);
    let old_single = old_single(
        &single,
        &input,
        &single.intent.output.with_file_name("old-single"),
    );
    let old_bundle = old_bundle(
        &bundle,
        &input,
        &bundle.output_root.with_file_name("old-bundle"),
        6,
    );
    execute_single(&old_single, &root.path().join("old-single.json"), 7);
    execute_bundle(&old_bundle, &root.path().join("old-bundle.json"), 7);
    execute_single(&single, &root.path().join("new-single.json"), 6);
    execute_bundle(&bundle, &root.path().join("new-bundle.json"), 6);
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}

fn separate_source(root: &Path) -> PathBuf {
    fs::create_dir(root).unwrap();
    let dash = root.join("dash");
    fs::create_dir(&dash).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut packager = std::process::Command::new("packager");
    for role in ["video", "audio"] {
        let directory = dash.join(role);
        fs::create_dir(&directory).unwrap();
        packager.arg(format!(
            "in={},stream={role},init_segment={}/init.mp4,segment_template={}/seg_$Number$.m4s",
            clip.display(),
            directory.display(),
            directory.display()
        ));
    }
    let manifest = dash.join("manifest.mpd");
    let result = packager
        .args([
            "--segment_duration",
            "2",
            "--fragment_duration",
            "2",
            "--mp4_reset_initial_composition_offset_to_zero=false",
            "--generate_static_live_mpd",
            "--mpd_output",
        ])
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "separate BBB fixture generation failed"
    );
    let output = root.join("hls");
    converter()
        .convert(ConversionRequest {
            input: &manifest,
            output: &output,
            format: OutputFormat::Hls,
            target: TargetProtection::Clear,
            input_keys: &InputKeys::default(),
            hls_input_keys: &HlsInputKeys::default(),
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    assert_targets(&output, 3);
    output.join("master.m3u8")
}

fn change_video_bound(input: &Path) {
    let master = fs::read_to_string(input).unwrap();
    let video = master
        .lines()
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    let path = input.parent().unwrap().join(video);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("#EXT-X-TARGETDURATION:3\n"));
    fs::write(
        path,
        text.replace("#EXT-X-TARGETDURATION:3\n", "#EXT-X-TARGETDURATION:2\n"),
    )
    .unwrap();
}

#[test]
fn legacy_segment_plans_admit_distinct_source_bounds_and_generate_one_common_bound() {
    let root = common::private_tempdir().unwrap();
    let input = separate_source(&root.path().join("source"));
    let (single, bundle) = plans(&input, root.path(), ProcessingMode::Segments);
    change_video_bound(&input);
    let before = common::snapshot_tree(input.parent().unwrap());
    let old_single = old_single(
        &single,
        &input,
        &single.intent.output.with_file_name("old-single"),
    );
    let old_bundle = old_bundle(
        &bundle,
        &input,
        &bundle.output_root.with_file_name("old-bundle"),
        6,
    );
    execute_single(&old_single, &root.path().join("old-single.json"), 3);
    execute_bundle(&old_bundle, &root.path().join("old-bundle.json"), 3);
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}

fn legacy_bundle_stitch_admission(schema: u32) {
    let root = common::private_tempdir().unwrap();
    let input = separate_source(&root.path().join("source"));
    let (_, bundle) = plans(&input, root.path(), ProcessingMode::Stitch);
    change_video_bound(&input);
    let before = common::snapshot_tree(input.parent().unwrap());
    let plan = old_bundle(
        &bundle,
        &input,
        &bundle.output_root.with_file_name(format!("old-{schema}")),
        schema,
    );
    execute_bundle(&plan, &root.path().join(format!("old-{schema}.json")), 3);
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}

#[test]
fn legacy_bundle_stitch_admission_keeps_schema_two_source_policy() {
    legacy_bundle_stitch_admission(2);
}
#[test]
fn legacy_bundle_stitch_admission_keeps_schema_four_source_policy() {
    legacy_bundle_stitch_admission(4);
}
#[test]
fn legacy_bundle_stitch_admission_keeps_schema_six_source_policy() {
    legacy_bundle_stitch_admission(6);
}
