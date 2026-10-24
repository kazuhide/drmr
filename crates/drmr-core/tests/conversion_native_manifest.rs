//! Final generated manifest bytes are native-checked before publication and fresh verification.
use drmr_core::{
    conversion::{
        ConversionRequest, Converter, HlsInputKeys, InputKeys, VerificationRequest,
        VerificationSource,
    },
    events::{NoSink, Record, Sink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
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
fn source(root: &Path) -> PathBuf {
    let path = root.join("source.mp4");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
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
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    path
}
fn transport(root: &Path, input: &Path) -> PathBuf {
    let directory = root.join("source-ts");
    std::fs::create_dir(&directory).unwrap();
    let playlist = directory.join("media.m3u8");
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(input)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(directory.join("segment-%d.ts"))
        .arg(&playlist)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    playlist
}

fn request<'a>(
    input: &'a Path,
    output: &'a Path,
    format: OutputFormat,
    keys: &'a InputKeys,
    hls_keys: &'a HlsInputKeys,
) -> ConversionRequest<'a> {
    ConversionRequest {
        input,
        output,
        format,
        target: TargetProtection::Clear,
        input_keys: keys,
        hls_input_keys: hls_keys,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    }
}
fn assert_native(value: &serde_json::Value) {
    assert_eq!(
        value["scope"], "manifest-only",
        "native output evidence is required: {value}"
    );
    assert_eq!(value["errors"], 0);
    assert_eq!(value["incomplete"], false);
    assert_eq!(value["truncated"], false);
    assert!(
        value["document_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
}

#[test]
fn standard_output_and_independent_verification_include_native_manifest_evidence() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = source(root.path());
    let keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let ts = transport(root.path(), &input);
    for format in [OutputFormat::Hls, OutputFormat::Dash, OutputFormat::HlsTs] {
        let input = if format == OutputFormat::HlsTs {
            &ts
        } else {
            &input
        };
        let output = root.path().join(format.as_str());
        let converter = converter();
        let report = converter
            .convert(request(input, &output, format, &keys, &hls_keys))
            .unwrap();
        assert!(report.verification.passed);
        assert_native(&serde_json::to_value(&report).unwrap()["verification"]["native_manifest"]);
        let manifest = output.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        let report = converter
            .verify(VerificationRequest {
                source: VerificationSource {
                    input,
                    input_keys: &keys,
                    hls_keys: &hls_keys,
                },
                output: VerificationSource {
                    input: &manifest,
                    input_keys: &keys,
                    hls_keys: &hls_keys,
                },
            })
            .unwrap();
        assert!(report.passed);
        assert_native(&serde_json::to_value(report).unwrap()["native_manifest"]);
    }
}

fn media_manifest(root: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(!metadata.is_symlink());
        if metadata.is_dir() {
            if let Some(found) = media_manifest(&path) {
                return Some(found);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "m3u8")
            && std::fs::read_to_string(&path)
                .unwrap()
                .contains("#EXT-X-MAP:")
        {
            return Some(path);
        }
    }
    None
}
fn downgrade(path: &Path) {
    let original = std::fs::read_to_string(path).unwrap();
    let changed = original
        .lines()
        .map(|line| {
            if line.starts_with("#EXT-X-VERSION:") {
                "#EXT-X-VERSION:5"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_ne!(original, changed);
    std::fs::write(path, changed).unwrap();
}
struct Downgrade {
    root: PathBuf,
    activity: &'static str,
    fired: AtomicBool,
}
impl Sink for Downgrade {
    fn emit(&self, record: &Record) {
        if self.fired.load(Ordering::SeqCst)
            || record["event"] != "activity"
            || record["activity"] != self.activity
            || record["state"] != "started"
        {
            return;
        }
        if let Some(manifest) = media_manifest(&self.root) {
            downgrade(&manifest);
            self.fired.store(true, Ordering::SeqCst);
        }
    }
}
fn refuse_changed_generated_manifest(activity: &'static str, expected: &'static str) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = source(root.path());
    let original = std::fs::read(&input).unwrap();
    let foreign = root.path().join("foreign");
    std::fs::write(&foreign, b"must remain").unwrap();
    let output = root.path().join("result");
    let sink = Arc::new(Downgrade {
        root: root.path().into(),
        activity,
        fired: AtomicBool::new(false),
    });
    let result = converter()
        .with_activity_sink(sink.clone())
        .convert(request(
            &input,
            &output,
            OutputFormat::Hls,
            &InputKeys::default(),
            &HlsInputKeys::default(),
        ));
    assert!(
        sink.fired.load(Ordering::SeqCst),
        "the requested manifest mutation window was reached"
    );
    assert_eq!(result.unwrap_err().0, expected);
    assert!(!output.exists());
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert_eq!(std::fs::read(foreign).unwrap(), b"must remain");
    for entry in std::fs::read_dir(root.path()).unwrap() {
        assert!(
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".drmr-convert-")
        );
    }
}
#[test]
fn invalid_generated_manifest_cannot_pass_media_checks_and_be_published() {
    refuse_changed_generated_manifest("verification", "native-manifest-lint-failed");
}
#[test]
fn native_checked_manifest_cannot_change_in_the_publication_window() {
    refuse_changed_generated_manifest("publication", "manifest-changed-during-validation");
}

#[test]
fn full_verification_keeps_media_evidence_when_raw_output_manifest_fails_native_rules() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = source(root.path());
    let output = root.path().join("result");
    let keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let converter = converter();
    converter
        .convert(request(
            &input,
            &output,
            OutputFormat::Hls,
            &keys,
            &hls_keys,
        ))
        .unwrap();
    downgrade(&media_manifest(&output).unwrap());
    let report = converter
        .verify(VerificationRequest {
            source: VerificationSource {
                input: &input,
                input_keys: &keys,
                hls_keys: &hls_keys,
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &keys,
                hls_keys: &hls_keys,
            },
        })
        .unwrap();
    assert!(
        !report.passed,
        "raw manifest errors cannot become a full verification pass"
    );
    assert!(
        (report.checks.is_pass()
            && report.decode_evidence.satisfies(
                drmr_core::conversion::ValidationDepth::Decoded,
                report.scope
            )),
        "manifest findings must preserve independently checked media evidence"
    );
    let report = serde_json::to_value(report).unwrap();
    assert!(
        report["native_manifest"]["errors"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(!report["license_playback_verified"].as_bool().unwrap());
}

#[test]
fn finite_audio_video_hls_uses_one_target_bound_and_verifies_the_raw_final_graph() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let original = std::fs::read(&input).unwrap();
    let output = root.path().join("result");
    let keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let command = converter();
    let conversion = command
        .convert(request(
            &input,
            &output,
            OutputFormat::Hls,
            &keys,
            &hls_keys,
        ))
        .unwrap();
    assert!(conversion.verification.passed);
    assert_native(&serde_json::to_value(&conversion).unwrap()["verification"]["native_manifest"]);
    let video = output.join("track-0000/media.m3u8");
    let audio = output.join("track-0001/media.m3u8");
    let video_bytes = std::fs::read_to_string(&video).unwrap();
    let audio_bytes = std::fs::read_to_string(&audio).unwrap();
    let target = |text: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    assert_eq!(target(&video_bytes), target(&audio_bytes));
    assert_eq!(
        target(&video_bytes),
        3,
        "the retained AAC boundary exceeds two seconds"
    );
    let binary = media_bytes(&output);
    let manifest = output.join("master.m3u8");
    let verify = || {
        command
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &keys,
                    hls_keys: &hls_keys,
                },
                output: VerificationSource {
                    input: &manifest,
                    input_keys: &keys,
                    hls_keys: &hls_keys,
                },
            })
            .unwrap()
    };
    let report = verify();
    assert!(
        report.passed
            && (report.checks.is_pass()
                && report.decode_evidence.satisfies(
                    drmr_core::conversion::ValidationDepth::Decoded,
                    report.scope
                ))
    );
    assert_native(&serde_json::to_value(report).unwrap()["native_manifest"]);
    std::fs::write(
        &video,
        video_bytes.replace("#EXT-X-TARGETDURATION:3", "#EXT-X-TARGETDURATION:2"),
    )
    .unwrap();
    let mismatch = verify();
    assert!(!mismatch.passed);
    assert!(
        (mismatch.checks.is_pass()
            && mismatch.decode_evidence.satisfies(
                drmr_core::conversion::ValidationDepth::Decoded,
                mismatch.scope
            )),
        "changing this bound does not change the original media"
    );
    assert!(
        mismatch
            .native_manifest
            .unwrap()
            .diagnostics
            .iter()
            .any(|issue| issue.rule_id == "HLS-GRAPH-TARGET-DURATIONS")
    );
    assert_eq!(media_bytes(&output), binary);
    assert_eq!(std::fs::read_to_string(&audio).unwrap(), audio_bytes);
    std::fs::write(&video, video_bytes).unwrap();
    assert!(verify().passed);
    assert_eq!(media_bytes(&output), binary);
    assert_eq!(std::fs::read(input).unwrap(), original);
}

fn media_bytes(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut result = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            result.extend(media_bytes(&path));
        } else if path
            .extension()
            .is_some_and(|value| value == "mp4" || value == "m4s")
        {
            let bytes = std::fs::read(&path).unwrap();
            result.insert(path, bytes);
        }
    }
    result
}

#[test]
fn named_dash_common_system_records_match_operator_and_actual_initialization_keys() {
    use drmr_core::conversion::{
        OutputKey,
        conformance::ManifestValidationTools,
        key_file::{BundleOperatorKeys, OperatorKeys},
        plan::bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
    };
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = source(root.path());
    let converter = converter();
    for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
        let mut operator = OperatorKeys::default();
        operator.output = Some(
            OutputKey::from_hex(
                "a0a1a2a3a4a5a6a7a8a9aaabacadaeaf",
                "202122232425262728292a2b2c2d2e2f",
            )
            .unwrap(),
        );
        let mut keys = BundleOperatorKeys::default();
        keys.outputs.insert("dash".into(), operator);
        let plan = converter
            .create_bundle_plan(BundlePlanRequest {
                input: VerificationSource {
                    input: &input,
                    input_keys: &InputKeys::default(),
                    hls_keys: &HlsInputKeys::default(),
                },
                output_root: &root.path().join(target.as_str()),
                outputs: &[BundleOutputIntent {
                    processing: None,
                    id: "dash".into(),
                    format: OutputFormat::Dash,
                    target,
                    drms: Vec::new(),
                    conformance: ManifestValidation::None,
                }],
            })
            .unwrap();
        let executed = converter
            .execute_bundle_plan(&plan, &keys, &ManifestValidationTools::default())
            .unwrap();
        assert!(executed.outputs[0].conversion.verification.passed);
        let checked = converter
            .verify_bundle_plan(&plan, &keys, &ManifestValidationTools::default())
            .unwrap();
        assert!(checked.passed);
        let output = root.path().join(target.as_str()).join("dash/manifest.mpd");
        let operator = &keys.outputs["dash"];
        converter
            .verify_output_signaling(&output, OutputFormat::Dash, target, operator)
            .unwrap();
        assert!(
            converter
                .verify_output_signaling(
                    &output,
                    OutputFormat::Dash,
                    TargetProtection::Clear,
                    &OperatorKeys::default()
                )
                .is_err()
        );
        mutate_common_record_and_initialization(&converter, &output, target, operator);
    }
}

fn mutate_common_record_and_initialization(
    converter: &Converter,
    manifest: &Path,
    target: TargetProtection,
    keys: &drmr_core::conversion::key_file::OperatorKeys,
) {
    use base64::Engine as _;
    let original = std::fs::read_to_string(manifest).unwrap();
    let document = roxmltree::Document::parse(&original).unwrap();
    let record = document
        .descendants()
        .find(|node| node.has_tag_name(("urn:mpeg:cenc:2013", "pssh")))
        .unwrap();
    let encoded = record.text().unwrap();
    let mut bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    bytes[32] ^= 1;
    std::fs::write(
        manifest,
        original.replacen(
            encoded,
            &base64::engine::general_purpose::STANDARD.encode(bytes),
            1,
        ),
    )
    .unwrap();
    assert!(
        converter
            .verify_output_signaling(manifest, OutputFormat::Dash, target, keys)
            .is_err(),
        "a mismatched CommonSystem key identifier must be refused"
    );
    std::fs::write(manifest, &original).unwrap();
    let init = initialization(manifest.parent().unwrap()).unwrap();
    let original = std::fs::read(&init).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&original).unwrap();
    let nodes = drmr_core::mp4::walk(&boxes);
    let tenc = nodes.iter().find(|node| node.kind() == *b"tenc").unwrap();
    let mut changed = original.clone();
    changed[tenc.header.offset + tenc.header.header_size + 8] ^= 1;
    std::fs::write(&init, changed).unwrap();
    assert!(
        converter
            .verify_output_signaling(manifest, OutputFormat::Dash, target, keys)
            .is_err(),
        "CommonSystem metadata must match the actual track encryption key identifier"
    );
    std::fs::write(&init, &original).unwrap();
    let pssh = nodes.iter().find(|node| node.kind() == *b"pssh").unwrap();
    let mut duplicate = original.clone();
    duplicate.extend_from_slice(pssh.header.bytes(&original));
    std::fs::write(&init, duplicate).unwrap();
    assert!(
        converter
            .verify_output_signaling(manifest, OutputFormat::Dash, target, keys)
            .is_err(),
        "duplicate CommonSystem initialization records must be refused"
    );
    std::fs::write(&init, original).unwrap();
    converter
        .verify_output_signaling(manifest, OutputFormat::Dash, target, keys)
        .unwrap();
}

fn initialization(root: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if let Some(found) = initialization(&path) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|name| name == "init.mp4") {
            return Some(path);
        }
    }
    None
}
