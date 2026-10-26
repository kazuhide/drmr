//! Direct conversion, old plans and DRM all use the same resolved duration contract.
use super::*;
use drmr_core::conversion::{
    ConversionRequest, DrmSystem,
    key_file::{self, OperatorKeys},
    plan::{self, ExecutionPlanRequest, SavedPlan},
};
use sha2::{Digest as _, Sha256};
use std::io::Write as _;

fn audio_durations(root: &Path) -> Vec<f64> {
    let master = fs::read_to_string(root.join("master.m3u8")).unwrap();
    let declaration = master
        .lines()
        .find(|line| line.starts_with("#EXT-X-MEDIA:"))
        .unwrap();
    let uri = declaration
        .split("URI=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    hls_durations(&fs::read_to_string(root.join(uri)).unwrap())
}

fn older_plan(
    saved: &plan::ExecutionPlan,
    schema: u32,
    output: std::path::PathBuf,
) -> plan::ExecutionPlan {
    let mut legacy = saved.clone();
    legacy.schema_version = schema;
    legacy.output_options = None;
    legacy.intent.output = output;
    if schema == 3 {
        legacy.processing = None;
    } else {
        legacy.processing.as_mut().unwrap().segment_duration_ms = Some(2000);
    }
    // Simulate an earlier release's structural digest using its unchanged
    // serialized schema; the actual conversion below supplies independent proof.
    let mut identity = serde_json::to_value(&legacy).unwrap();
    identity.as_object_mut().unwrap().shift_remove("plan_id");
    legacy.plan_id = hex::encode(Sha256::digest(serde_json::to_vec(&identity).unwrap()));
    legacy
}

#[test]
fn direct_and_single_saved_conversion_inherit_while_older_plans_keep_their_target() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("source"), 48_000);
    let original = common::snapshot_tree(input.parent().unwrap());
    let command = converter();
    let input_keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let output = root.path().join("direct");
    let report = command
        .convert(ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::Hls,
            target: TargetProtection::Clear,
            input_keys: &input_keys,
            hls_input_keys: &hls_keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    assert!(report.verification.passed);
    assert_six_second_segments(&audio_durations(&output));

    let output = root.path().join("saved");
    let saved = command
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &input_keys,
                hls_keys: &hls_keys,
            },
            output: &output,
            format: Some(OutputFormat::Hls),
            target: TargetProtection::Clear,
        })
        .unwrap();
    assert_eq!(saved.processing.unwrap().segment_duration_ms, Some(6000));
    let file = root.path().join("saved.plan.json");
    plan::write_new(&file, &saved).unwrap();
    let SavedPlan::Single(saved) = plan::read_saved(&file).unwrap() else {
        panic!("single plan required")
    };
    let keys = OperatorKeys::default();
    assert!(
        command
            .execute_plan(&saved, &keys)
            .unwrap()
            .verification
            .passed
    );
    assert_six_second_segments(&audio_durations(&output));
    assert!(
        command
            .verify_plan(&saved, &keys)
            .unwrap()
            .verification
            .passed
    );

    for schema in [3, 5] {
        let legacy = older_plan(
            &saved,
            schema,
            root.path()
                .canonicalize()
                .unwrap()
                .join(format!("legacy-{schema}")),
        );
        let encoded = serde_json::to_vec(&legacy).unwrap();
        let file = root.path().join(format!("legacy-{schema}.plan.json"));
        plan::write_new(&file, &legacy).unwrap();
        let SavedPlan::Single(reopened) = plan::read_saved(&file).unwrap() else {
            panic!("single plan required")
        };
        assert_eq!(serde_json::to_vec(&reopened).unwrap(), encoded);
        assert!(
            command
                .execute_plan(&reopened, &keys)
                .unwrap()
                .verification
                .passed
        );
        let durations = audio_durations(&legacy.intent.output);
        assert!((10..=11).contains(&durations.len()));
        assert!(
            durations[1..durations.len() - 1]
                .iter()
                .all(|duration| (1.9..2.1).contains(duration))
        );
        assert!(
            command
                .verify_plan(&reopened, &keys)
                .unwrap()
                .verification
                .passed
        );
    }
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
}

#[test]
fn fairplay_widevine_and_playready_keep_the_target_with_an_additional_decode_oracle() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("source"), 44_100);
    let original = common::snapshot_tree(input.parent().unwrap());
    let kid = "000102030405060708090a0b0c0d0e0f";
    let pssh = hex::encode(
        drmr_core::playready::build_pssh_box(
            &drmr_core::playready::WIDEVINE_SYSTEM_ID,
            &hex::decode(format!("1210{kid}")).unwrap(),
        )
        .unwrap(),
    );
    let key_file = root.path().join("keys.json");
    drmr_core::native::create_private_new(&key_file).unwrap().write_all(serde_json::to_string(&serde_json::json!({
        "schema_version":2,"outputs":{
            "hls":{"kid":kid,"key":"101112131415161718191a1b1c1d1e1f","hls_key_uri":"skd://segment-target-test",
                "signaling":{"system":"multi-drm","fairplay":true}},
            "dash":{"kid":kid,"key":"101112131415161718191a1b1c1d1e1f",
                "signaling":{"system":"multi-drm","widevine_pssh":pssh,"playready_license_url":"https://license.example.invalid/playready"}}
        }
    })).unwrap().as_bytes()).unwrap();
    let keys = key_file::read_bundle(&key_file).unwrap();
    let outputs = [
        BundleOutputIntent {
            dash_addressing: None,
            id: "hls".into(),
            format: OutputFormat::Hls,
            target: TargetProtection::Cbcs,
            drms: vec![DrmSystem::FairPlay],
            conformance: ManifestValidation::None,
            processing: None,
        },
        BundleOutputIntent {
            dash_addressing: None,
            id: "dash".into(),
            format: OutputFormat::Dash,
            target: TargetProtection::Cbcs,
            drms: vec![DrmSystem::Widevine, DrmSystem::PlayReady],
            conformance: ManifestValidation::None,
            processing: None,
        },
    ];
    let command = converter();
    let destination = root.path().join("result");
    let saved = command
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output_root: &destination,
            outputs: &outputs,
        })
        .unwrap();
    let tools = ManifestValidationTools::default();
    let result = command.execute_bundle_plan(&saved, &keys, &tools).unwrap();
    assert!(
        result
            .outputs
            .iter()
            .all(|output| output.conversion.verification.passed && output.signaling.verified)
    );
    assert_outputs(&destination, &["hls"]);
    let native = command.verify_bundle_plan(&saved, &keys, &tools).unwrap();
    assert!(native.passed && native.source_unchanged);
    let decoded = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
    .with_decode(true)
    .verify_bundle_plan(&saved, &keys, &tools)
    .unwrap();
    assert!(decoded.passed && decoded.source_unchanged);
    assert!(
        decoded
            .outputs
            .iter()
            .all(|output| output.verification.required_depth == ValidationDepth::Decoded)
    );
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
}

#[test]
fn an_mp4_source_with_six_second_access_points_keeps_its_generated_hls_bound() {
    let root = common::private_tempdir().unwrap();
    let hls = source(&root.path().join("source"), 48_000);
    let input = root.path().join("source.mp4");
    let generated = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&hls)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-c",
            "copy",
            "-map_metadata",
            "-1",
            "-bitexact",
            "-movflags",
            "delay_moov+default_base_moof+frag_keyframe",
        ])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "MP4 fixture generation failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let original = common::snapshot_tree(root.path());
    let command = converter();
    let saved = command
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output: &root.path().join("result"),
            format: Some(OutputFormat::Hls),
            target: TargetProtection::Clear,
        })
        .unwrap();
    assert_eq!(saved.processing.unwrap().segment_duration_ms, Some(2000));
    let keys = OperatorKeys::default();
    assert!(
        command
            .execute_plan(&saved, &keys)
            .unwrap()
            .verification
            .passed
    );
    let output = &saved.intent.output;
    let master = fs::read_to_string(output.join("master.m3u8")).unwrap();
    let video = master
        .lines()
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    let text = fs::read_to_string(output.join(video)).unwrap();
    assert!(hls_durations(&text).iter().any(|duration| *duration >= 5.9));
    let target: u64 = text
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        target >= 6,
        "the two-second fallback is not an inherited HLS bound"
    );
    assert!(
        command
            .verify_plan(&saved, &keys)
            .unwrap()
            .verification
            .passed
    );
    let after = common::snapshot_tree(root.path());
    for original in original {
        assert!(after.contains(&original), "fixture bytes changed");
    }
}

fn manifest_rows(entrypoint: &Path) -> Vec<(u64, Vec<String>)> {
    let text = fs::read_to_string(entrypoint).unwrap();
    if text.lines().any(|line| line.starts_with("#EXTINF:")) {
        let target = text
            .lines()
            .find_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
            .unwrap()
            .parse()
            .unwrap();
        let durations = text
            .lines()
            .filter_map(|line| line.strip_prefix("#EXTINF:"))
            .map(str::to_string)
            .collect();
        return vec![(target, durations)];
    }
    let mut paths = Vec::new();
    for line in text.lines() {
        if line.starts_with("#EXT-X-MEDIA:") {
            paths.push(
                line.split("URI=\"")
                    .nth(1)
                    .unwrap()
                    .split('"')
                    .next()
                    .unwrap()
                    .to_string(),
            );
        } else if !line.is_empty() && !line.starts_with('#') {
            paths.push(line.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    let mut rows = Vec::new();
    for path in paths {
        rows.extend(manifest_rows(&entrypoint.parent().unwrap().join(path)));
    }
    rows.sort();
    rows
}

#[test]
fn segment_processing_preserves_original_six_second_headers_and_each_duration() {
    let root = common::private_tempdir().unwrap();
    let directory = root.path().join("source");
    fs::create_dir(&directory).unwrap();
    let input = directory.join("media.m3u8");
    let generated = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=997:sample_rate=48000:duration=20",
            "-c:a",
            "aac",
            "-threads",
            "1",
            "-hls_time",
            "6",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(directory.join("seg-%d.ts"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "AAC segment fixture generation failed"
    );
    let input_keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let command = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "must-not-run",
        "must-not-run",
    )
    .with_ffprobe("must-not-run")
    .with_processing(ProcessingMode::Segments);
    let entrypoint = &input;
    let format = OutputFormat::HlsTs;
    let name = "ts-segments";
    let original = common::snapshot_tree(entrypoint.parent().unwrap());
    let before = manifest_rows(entrypoint);
    assert!(before.iter().all(|(target, _)| *target == 6));
    assert!(
        before.iter().flat_map(|(_, rows)| rows).any(|row| row
            .split(',')
            .next()
            .unwrap()
            .parse::<f64>()
            .unwrap()
            > 6.0),
        "the regression needs a real AAC segment longer than six seconds"
    );
    let output = root.path().join(name);
    let report = command
        .convert(ConversionRequest {
            input: entrypoint,
            output: &output,
            format,
            target: TargetProtection::Clear,
            input_keys: &input_keys,
            hls_input_keys: &hls_keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap_or_else(|error| panic!("{name}: {}", error.0));
    assert!(report.verification.passed);
    assert_eq!(manifest_rows(&output.join("master.m3u8")), before);
    let verified = command
        .verify(drmr_core::conversion::VerificationRequest {
            source: VerificationSource {
                input: entrypoint,
                input_keys: &input_keys,
                hls_keys: &hls_keys,
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &input_keys,
                hls_keys: &hls_keys,
            },
        })
        .unwrap();
    assert!(verified.passed);
    assert_eq!(
        common::snapshot_tree(entrypoint.parent().unwrap()),
        original
    );
}
