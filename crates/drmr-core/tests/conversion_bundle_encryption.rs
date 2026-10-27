//! Named outputs keep sample encryption and transport encryption in separate key roles.
mod common;

use drmr_core::{
    conversion::{
        Converter, DrmSystem, HlsInputKeys, InputKeys, VerificationSource,
        conformance::ManifestValidationTools,
        key_file,
        plan::{
            self, ExecutionPlanRequest,
            bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
        },
    },
    events::{Collector, NoSink, Record, Sink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use serde_json::json;
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const IV: &str = "202122232425262728292a2b2c2d2e2f";
const HLS_KID: &str = "303132333435363738393a3b3c3d3e3f";
const HLS_KEY: &str = "404142434445464748494a4b4c4d4e4f";
const TS_KEY: &str = "505152535455565758595a5b5c5d5e5f";
const AES_KEY: &str = "606162636465666768696a6b6c6d6e6f";

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
    .with_decode(true)
}

fn transport(source: &Path) -> PathBuf {
    std::fs::create_dir(source).unwrap();
    let input = source.join("media.m3u8");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(clip)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(source.join("segment-%d.ts"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    input
}

fn keys(root: &Path) -> key_file::BundleOperatorKeys {
    let pssh = hex::encode(
        drmr_core::playready::build_pssh_box(
            &drmr_core::playready::WIDEVINE_SYSTEM_ID,
            &hex::decode(format!("1210{KID}")).unwrap(),
        )
        .unwrap(),
    );
    let file = root.join("keys.json");
    drmr_core::native::create_private_new(&file)
        .unwrap()
        .write_all(
            json!({
                "schema_version":2,
                "outputs":{
                    "dash":{"kid":KID,"key":KEY,"signaling":{
                        "system":"multi-drm","widevine_pssh":pssh,
                        "playready_license_url":"https://license.example.invalid/playready"
                    }},
                    "hls":{"kid":HLS_KID,"key":HLS_KEY,"hls_key_uri":"skd://fixture-fmp4",
                        "signaling":{"system":"multi-drm","fairplay":true}},
                    "hls-ts":{"key":TS_KEY,"iv":IV,"hls_key_uri":"skd://fixture-ts",
                        "signaling":{"system":"multi-drm","fairplay":true}},
                    "aes-ts":{"key":AES_KEY,"hls_key_uri":"https://keys.example.invalid/transport.key"}
                }
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    key_file::read_bundle(&file).unwrap()
}

fn outputs() -> Vec<BundleOutputIntent> {
    [
        (
            "dash",
            OutputFormat::Dash,
            TargetProtection::Cbcs,
            vec![DrmSystem::Widevine, DrmSystem::PlayReady],
        ),
        (
            "hls",
            OutputFormat::Hls,
            TargetProtection::Cbcs,
            vec![DrmSystem::FairPlay],
        ),
        (
            "hls-ts",
            OutputFormat::HlsTs,
            TargetProtection::HlsSampleAes,
            vec![DrmSystem::FairPlay],
        ),
        (
            "aes-ts",
            OutputFormat::HlsTs,
            TargetProtection::HlsAes128,
            vec![],
        ),
    ]
    .into_iter()
    .map(|(id, format, target, drms)| BundleOutputIntent {
        dash_addressing: None,
        processing: None,
        id: id.into(),
        format,
        target,
        drms,
        conformance: ManifestValidation::None,
    })
    .collect()
}

fn direct_sample_bundle() -> (
    Arc<Collector>,
    Vec<drmr_core::conversion::plan::bundle::BundleConversionOutput>,
) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let input = transport(&source);
    let original = common::snapshot_tree(&source);
    let mut keys = keys(root.path());
    keys.outputs
        .retain(|id, _| matches!(id.as_str(), "dash" | "hls"));
    let outputs = outputs().into_iter().take(2).collect::<Vec<_>>();
    let records = Arc::new(Collector::default());
    let command = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), records.clone())),
        "packager",
        "missing-decoder",
    )
    .with_activity_sink(records.clone());
    let result = command
        .convert_bundle(
            &input,
            &outputs,
            &root.path().join("result"),
            &keys,
            &ManifestValidationTools::default(),
        )
        .unwrap();
    assert_eq!(result.outputs.len(), 2);
    for output in &result.outputs {
        assert!(output.conversion.verification.passed);
        assert!(output.conversion.verification.checks.is_pass());
        assert!(output.signaling.verified);
        assert!(
            output
                .conversion
                .verification
                .native_manifest
                .as_ref()
                .unwrap()
                .is_clean_for_scope()
        );
        assert!(!output.conversion.verification.license_playback_verified);
    }
    assert_eq!(common::snapshot_tree(&source), original);
    assert_eq!(
        common::names_with_prefix(root.path(), ".drmr-convert-"),
        Vec::<String>::new()
    );
    assert!(!records.records().iter().any(|record| matches!(
        record.get("tool").and_then(serde_json::Value::as_str),
        Some("ffmpeg" | "ffprobe")
    )));
    (records, result.outputs)
}

#[test]
fn direct_sample_bundle_qualifies_one_common_source() {
    let (records, _) = direct_sample_bundle();
    let admitted = records
        .records()
        .iter()
        .filter(|record| {
            record.get("event").and_then(serde_json::Value::as_str) == Some("source-qualified")
                && record.get("purpose").and_then(serde_json::Value::as_str) == Some("conversion")
        })
        .count();
    assert_eq!(
        admitted, 1,
        "common source admission must not repeat per DRM output or plan replay"
    );
}

#[test]
fn direct_sample_bundle_normalizes_clear_tracks_once() {
    let (records, _) = direct_sample_bundle();
    let invocations = records
        .records()
        .iter()
        .filter(|record| {
            record.get("event").and_then(serde_json::Value::as_str) == Some("command")
                && record.get("tool").and_then(serde_json::Value::as_str) == Some("packager")
        })
        .count();
    assert_eq!(
        invocations, 6,
        "two input tracks demux and normalize once, then package independently for each of two outputs"
    );
}

#[test]
fn sample_and_transport_encrypted_outputs_publish_and_verify_as_one_bundle() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let input = transport(&source);
    let original = common::snapshot_tree(&source);
    let keys = keys(root.path());
    let outputs = outputs();
    let plan = converter()
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output_root: &root.path().join("result"),
            outputs: &outputs,
        })
        .unwrap();
    let tools = ManifestValidationTools::default();
    let converted = converter()
        .execute_bundle_plan(&plan, &keys, &tools)
        .unwrap();
    assert_eq!(converted.outputs.len(), outputs.len());
    let verified = converter()
        .verify_bundle_plan(&plan, &keys, &tools)
        .unwrap();
    assert!(verified.passed && verified.source_unchanged);
    assert_eq!(verified.outputs.len(), outputs.len());
    for (result, intent) in converted.outputs.iter().zip(&outputs) {
        assert_eq!(result.id, intent.id);
        assert!(
            result.conversion.verification.passed
                && (result.conversion.verification.checks.is_pass()
                    && result.conversion.verification.decode_evidence.satisfies(
                        drmr_core::conversion::ValidationDepth::Decoded,
                        result.conversion.verification.scope
                    ))
        );
        assert_eq!(result.signaling.systems, intent.drms);
        assert_eq!(result.signaling.verified, !intent.drms.is_empty());
        assert!(result.conformance.is_none());
    }
    for result in verified.outputs {
        assert!(
            result.output_matches_plan
                && result.verification.passed
                && (result.verification.checks.is_pass()
                    && result.verification.decode_evidence.satisfies(
                        drmr_core::conversion::ValidationDepth::Decoded,
                        result.verification.scope
                    ))
        );
        assert!(!result.verification.license_playback_verified);
    }
    assert_eq!(common::snapshot_tree(&source), original);
    assert_eq!(
        common::names_with_prefix(root.path(), ".drmr-convert-"),
        Vec::<String>::new()
    );
}

fn single_transport_plan(id: &str, target: TargetProtection) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let input = transport(&source);
    let original = common::snapshot_tree(&source);
    let keys = keys(root.path()).outputs.remove(id).unwrap();
    let plan = converter()
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output: &root.path().join("result"),
            format: Some(OutputFormat::HlsTs),
            target,
        })
        .unwrap();
    let saved = root.path().join("single.plan.json");
    plan::write_new(&saved, &plan).unwrap();
    let loaded = plan::read(&saved).unwrap();
    assert_eq!(loaded.schema_version, 9);
    assert_eq!(
        loaded.processing_mode(),
        drmr_core::conversion::ProcessingMode::Stitch
    );
    assert_eq!(
        loaded.minimum_depth(),
        drmr_core::conversion::ValidationDepth::Decoded
    );
    let converted = converter().execute_plan(&loaded, &keys).unwrap();
    assert!(
        converted.verification.passed
            && (converted.verification.checks.is_pass()
                && converted.verification.decode_evidence.satisfies(
                    drmr_core::conversion::ValidationDepth::Decoded,
                    converted.verification.scope
                ))
    );
    let checked = converter().verify_plan(&loaded, &keys).unwrap();
    assert!(checked.source_unchanged && checked.output_matches_plan);
    assert!(
        checked.verification.passed
            && (checked.verification.checks.is_pass()
                && checked.verification.decode_evidence.satisfies(
                    drmr_core::conversion::ValidationDepth::Decoded,
                    checked.verification.scope
                ))
    );
    assert!(!checked.verification.license_playback_verified);
    assert_eq!(common::snapshot_tree(&source), original);
}

#[test]
fn single_sample_aes_transport_plan_uses_the_hls_key_domain() {
    single_transport_plan("hls-ts", TargetProtection::HlsSampleAes);
}

#[test]
fn single_aes128_transport_plan_uses_the_hls_key_domain() {
    single_transport_plan("aes-ts", TargetProtection::HlsAes128);
}

struct DirectInterruption {
    fired: AtomicBool,
    stop: drmr_core::concurrency::Stop,
    source: Option<PathBuf>,
}

impl Sink for DirectInterruption {
    fn emit(&self, record: &Record) {
        let text = |key| record.get(key).and_then(serde_json::Value::as_str);
        if text("stage") == Some("pipeline")
            && text("scope") == Some("output")
            && text("state") == Some("finished")
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            if let Some(source) = &self.source {
                let mut bytes = std::fs::read(source).unwrap();
                let last = bytes.len() - 1;
                bytes[last] ^= 1;
                std::fs::write(source, bytes).unwrap();
            } else {
                self.stop.set();
            }
        }
    }
}

fn interrupt_direct_bundle(change_source: bool) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let input = transport(&source);
    let segment = std::fs::read_dir(&source)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "ts"))
        .unwrap();
    let original = std::fs::read(&segment).unwrap();
    let mut keys = keys(root.path());
    keys.outputs
        .retain(|id, _| matches!(id.as_str(), "dash" | "hls"));
    let outputs = outputs().into_iter().take(2).collect::<Vec<_>>();
    let observed = Arc::new(DirectInterruption {
        fired: AtomicBool::new(false),
        stop: drmr_core::concurrency::Stop::new(),
        source: change_source.then(|| segment.clone()),
    });
    let foreign = root.path().join("foreign");
    std::fs::write(&foreign, b"must remain").unwrap();
    let output = root.path().join("result");
    let error = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "missing-decoder",
    )
    .with_stop(observed.stop.clone())
    .with_activity_sink(observed.clone())
    .convert_bundle(
        &input,
        &outputs,
        &output,
        &keys,
        &ManifestValidationTools::default(),
    )
    .unwrap_err();
    assert!(observed.fired.load(Ordering::SeqCst));
    assert_eq!(
        error.0,
        if change_source {
            "execution-plan-source-changed"
        } else {
            "cancelled"
        }
    );
    assert!(!output.exists());
    assert!(input.exists() && segment.exists());
    assert_eq!(std::fs::read(&foreign).unwrap(), b"must remain");
    assert_eq!(
        common::names_with_prefix(root.path(), ".drmr-convert-"),
        Vec::<String>::new()
    );
    if change_source {
        assert_ne!(std::fs::read(segment).unwrap(), original);
    } else {
        assert_eq!(std::fs::read(segment).unwrap(), original);
    }
}

#[test]
fn direct_shared_preparation_cannot_publish_after_a_completed_child_changes_the_source() {
    interrupt_direct_bundle(true);
}

#[test]
fn direct_shared_preparation_cancellation_after_a_completed_child_publishes_nothing() {
    interrupt_direct_bundle(false);
}

#[test]
fn reused_drm_admission_keeps_the_complete_mp4_opaque_box_refusal() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut bytes = std::fs::read(clip).unwrap();
    bytes.extend_from_slice(b"\0\0\0\x08skip");
    drmr_core::native::create_private_new(&input)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    let command = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "must-not-start-packager",
        "must-not-start-decoder",
    );
    let keys = keys(root.path());
    // Native sample validity does not grant permission for opaque DRM metadata.
    assert!(
        command
            .validate_source(
                VerificationSource {
                    input: &input,
                    input_keys: &keys.input,
                    hls_keys: &keys.hls_input
                },
                drmr_core::conversion::SourceValidationPurpose::Conversion
            )
            .unwrap()
            .verification
            .passed
    );
    let mut keys = keys;
    keys.outputs.retain(|id, _| id == "dash");
    let error = command
        .convert_bundle(
            &input,
            &outputs()[..1],
            &root.path().join("result"),
            &keys,
            &ManifestValidationTools::default(),
        )
        .unwrap_err();
    assert_eq!(error.0, "output-signaling-profile-not-supported");
    assert!(!root.path().join("result").exists());
    assert_eq!(std::fs::read(input).unwrap(), bytes);
}

#[test]
fn shared_bundle_preparation_leaves_one_cpu_and_releases_retained_memory() {
    use drmr_core::resources::{Claim, Limiter};
    use std::time::Duration;
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let input = transport(&source);
    let original = common::snapshot_tree(&source);
    let mut keys = keys(root.path());
    keys.outputs
        .retain(|id, _| matches!(id.as_str(), "dash" | "hls"));
    let outputs = outputs().into_iter().take(2).collect::<Vec<_>>();
    let memory = 1024 * 1024 * 1024;
    let limiter = Arc::new(Limiter::new(1, memory));
    let runner =
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)).with_limiter(limiter.clone());
    let command = Converter::new(Arc::new(runner), "packager", "missing-decoder")
        .with_max_workers(Some(4))
        .unwrap()
        .with_timeout(Duration::from_secs(5));
    let result = command
        .convert_bundle(
            &input,
            &outputs,
            &root.path().join("result"),
            &keys,
            &ManifestValidationTools::default(),
        )
        .unwrap();
    assert_eq!(result.outputs.len(), 2);
    assert!(
        result
            .outputs
            .iter()
            .all(|output| output.conversion.verification.passed
                && output.conversion.verification.checks.is_pass())
    );
    assert_eq!(common::snapshot_tree(&source), original);
    assert_eq!(
        common::names_with_prefix(root.path(), ".drmr-convert-"),
        Vec::<String>::new()
    );
    drop(command);
    drop(
        limiter
            .acquire(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: memory,
                },
                None,
                Some(Duration::from_secs(1)),
            )
            .unwrap(),
    );
}

#[test]
fn shared_bundle_preparation_keeps_native_and_complete_decode_evidence() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let input = transport(&source);
    let original = common::snapshot_tree(&source);
    let mut keys = keys(root.path());
    keys.outputs
        .retain(|id, _| matches!(id.as_str(), "dash" | "hls"));
    let outputs = outputs().into_iter().take(2).collect::<Vec<_>>();
    let result = converter()
        .convert_bundle(
            &input,
            &outputs,
            &root.path().join("result"),
            &keys,
            &ManifestValidationTools::default(),
        )
        .unwrap();
    assert_eq!(result.outputs.len(), 2);
    for output in &result.outputs {
        let verification = &output.conversion.verification;
        assert!(verification.passed && verification.checks.is_pass());
        assert_eq!(
            verification.required_depth,
            drmr_core::conversion::ValidationDepth::Decoded
        );
        assert!(
            verification
                .decode_evidence
                .satisfies(verification.required_depth, verification.scope)
        );
        assert!(
            verification
                .native_manifest
                .as_ref()
                .unwrap()
                .is_clean_for_scope()
        );
        assert!(output.signaling.verified);
    }
    assert_eq!(common::snapshot_tree(&source), original);
    assert_eq!(
        common::names_with_prefix(root.path(), ".drmr-convert-"),
        Vec::<String>::new()
    );
}
