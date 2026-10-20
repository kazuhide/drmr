//! The admitted metadata bound must also be safe for diagnostic redaction.
use drmr_core::{
    conversion::{
        ConversionRequest, Converter, OutputKey, OutputSignaling, VerificationRequest,
        VerificationSource, key_file::OperatorKeys,
    },
    events::{Collector, NoSink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{REDACTED, RunOptions, Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn largest_pssh() -> Vec<u8> {
    let mut payload = hex::decode("1210000102030405060708090a0b0c0d0e0f").unwrap();
    payload.push(0x1a);
    let mut size = 65_482u32;
    while size >= 128 {
        payload.push(u8::try_from(size & 127).unwrap() | 128);
        size >>= 7;
    }
    payload.push(u8::try_from(size).unwrap());
    payload.resize(65_504, b'a');
    let bytes =
        drmr_core::playready::build_pssh_box(&drmr_core::playready::WIDEVINE_SYSTEM_ID, &payload)
            .unwrap();
    assert_eq!(bytes.len(), 65_536);
    bytes
}
#[test]
fn maximum_admitted_pssh_has_bounded_panic_free_private_redaction() {
    let bytes = largest_pssh();
    let secret = hex::encode(&bytes);
    OutputSignaling::multi_drm(Some(&secret), None, false).unwrap();
    let secrets = Secrets::with([secret.as_str()]);
    assert_eq!(
        secrets.scrub("ordinary unrelated diagnostic"),
        "ordinary unrelated diagnostic"
    );
    assert_eq!(secrets.scrub(&secret), REDACTED);
    assert_eq!(secrets.scrub(&secret.to_uppercase()), REDACTED);
    let mut different = secret.clone();
    different.replace_range(80..81, if &secret[80..81] == "0" { "1" } else { "0" });
    assert_eq!(secrets.scrub(&different), different);
    assert_eq!(
        secrets.scrub(&format!("before {secret} after")),
        format!("before {REDACTED} after")
    );
    let separated = secret
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap())
        .collect::<Vec<_>>()
        .join(" : 0x");
    assert_eq!(secrets.scrub(&separated), REDACTED);
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    assert_eq!(secrets.scrub(&encoded), REDACTED);
}

#[test]
fn maximum_pssh_per_call_masks_tool_output_without_a_registry_or_provider_request() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let secret = hex::encode(largest_pssh());
    let tool = drmr_test_support::install(
        &owner.path().join("response"),
        &drmr_test_support::ToolSpec {
            stdout: secret.as_bytes().to_vec(),
            stderr: secret.to_uppercase().into_bytes(),
            ..drmr_test_support::ToolSpec::default()
        },
    )
    .unwrap();
    let events = Arc::new(Collector::default());
    let runner = ToolRunner::new(Arc::new(Secrets::new()), events.clone());
    let result = runner
        .run_tool(
            &[tool.to_str().unwrap().to_owned()],
            "synthetic-metadata-response",
            &RunOptions {
                secrets: &[&secret],
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    let rendered = serde_json::to_string(&events.records()).unwrap();
    assert!(rendered.contains(REDACTED));
    assert!(!rendered.contains(&secret));
    assert!(!rendered.contains(&secret.to_uppercase()));
}

fn fragmented_source(root: &Path) -> PathBuf {
    let input = root.join("source.mp4");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let source = runner
        .run_tool(
            &[
                "ffmpeg",
                "-v",
                "error",
                "-nostdin",
                "-i",
                clip.to_str().unwrap(),
                "-map",
                "0:v:0",
                "-c",
                "copy",
                "-map_metadata",
                "-1",
                "-bitexact",
                "-movflags",
                "delay_moov+default_base_moof+frag_keyframe",
                input.to_str().unwrap(),
            ]
            .map(str::to_owned),
            "ffmpeg",
            &RunOptions::default(),
        )
        .unwrap();
    assert_eq!(source.returncode, Some(0));
    input
}

#[test]
fn maximum_widevine_metadata_preserves_complete_media_alone_and_with_playready() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let input = fragmented_source(owner.path());
    let original = std::fs::read(&input).unwrap();
    let metadata = hex::encode(largest_pssh());
    let events = Arc::new(Collector::default());
    let converter = || {
        Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
            "packager",
            "ffmpeg",
        )
    };
    for dual in [false, true] {
        let output = owner.path().join(format!("result-{dual}"));
        let mut keys = OperatorKeys::default();
        keys.output = Some(
            OutputKey::from_hex(
                "000102030405060708090a0b0c0d0e0f",
                "101112131415161718191a1b1c1d1e1f",
            )
            .unwrap(),
        );
        keys.signaling = Some(
            OutputSignaling::multi_drm(
                Some(&metadata),
                dual.then_some("https://example.invalid/license"),
                false,
            )
            .unwrap(),
        );
        let report = converter()
            .with_output_signaling(keys.signaling.clone())
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::Dash,
                target: TargetProtection::Cbcs,
                input_keys: &keys.input,
                hls_input_keys: &keys.hls_input,
                output_key: keys.output.as_ref(),
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap();
        assert!(report.verification.passed);
        assert_eq!(report.output.drm_systems.len(), if dual { 2 } else { 1 });
        let manifest = output.join("manifest.mpd");
        converter()
            .verify_output_signaling(&manifest, OutputFormat::Dash, TargetProtection::Cbcs, &keys)
            .unwrap();
        let output_keys = keys.output_verification_keys().unwrap();
        let checked = converter()
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &keys.input,
                    hls_keys: &keys.hls_input,
                },
                output: VerificationSource {
                    input: &manifest,
                    input_keys: &output_keys,
                    hls_keys: &keys.hls_input,
                },
            })
            .unwrap();
        assert!(checked.passed && checked.media_verified);
        assert!(checked.checks.packet_payloads_equal);
        assert!(checked.checks.packet_timing_equal);
        assert!(checked.checks.decoded_content_equal);
        assert!(!checked.license_playback_verified);
        assert_no_flagfiles(&output);
        assert_eq!(std::fs::read(&input).unwrap(), original);
    }
    let records = serde_json::to_string(&events.records()).unwrap();
    assert!(!records.contains(&metadata));
    assert!(!records.contains(&metadata.to_uppercase()));
    assert!(!records.contains("protection.flags"));
    assert!(!records.contains("https://example.invalid/license"));
}
fn assert_no_flagfiles(root: &Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        assert!(!path.is_symlink());
        if path.is_dir() {
            assert_no_flagfiles(&path);
        } else {
            assert_ne!(
                path.extension().and_then(|value| value.to_str()),
                Some("flags")
            );
        }
    }
}
