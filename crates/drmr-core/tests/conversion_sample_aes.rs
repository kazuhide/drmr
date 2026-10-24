//! Original encrypted transport samples remain the authority for every output proof.
mod common;
#[path = "support/sample_aes_fixture.rs"]
mod sample_aes_fixture;
use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, HlsInputKeys, HlsOutputKey, InputKeys,
        OutputKey, SegmentIv, VerificationRequest, VerificationSource,
    },
    events::{Collector, NoSink},
    inspection::{OutputFormat, Protection, TargetProtection},
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

// Public synthetic fixture material, independent of any operator configuration.
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const IV: &str = "303132333435363738393a3b3c3d3e3f";
const URI: &str = "https://operator.invalid/public-fixture";
const OUTPUT_KEY: &str = "202122232425262728292a2b2c2d2e2f";
const OUTPUT_URI: &str = "https://operator.invalid/result-fixture";

fn runner() -> Arc<ToolRunner> {
    Arc::new(ToolRunner::new(
        Arc::new(Secrets::with([KEY, IV, OUTPUT_KEY])),
        Arc::new(NoSink),
    ))
}
fn tool_path(path: &Path) -> String {
    drmr_core::manifest_support::path::fspath(path).unwrap()
}
fn shaka_video(root: &Path, encrypted: bool) -> PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut argv = vec![
        "packager".into(),
        format!(
            "input={},stream=video,segment_template={},playlist_name=media.m3u8",
            tool_path(&source),
            tool_path(&root.join("segment-$Number$.ts"))
        ),
        "--hls_master_playlist_output".into(),
        tool_path(&root.join("master.m3u8")),
        "--segment_duration".into(),
        "2".into(),
        "--fragment_duration".into(),
        "2".into(),
        "--transport_stream_timestamp_offset_ms".into(),
        "0".into(),
        "--mp4_reset_initial_composition_offset_to_zero=false".into(),
    ];
    if encrypted {
        argv.extend([
            "--enable_raw_key_encryption".into(),
            "--keys".into(),
            format!("label=:key_id=000102030405060708090a0b0c0d0e0f:key={KEY}"),
            "--iv".into(),
            IV.into(),
            "--protection_scheme".into(),
            "cbcs".into(),
            "--clear_lead".into(),
            "0".into(),
            "--hls_key_uri".into(),
            URI.into(),
        ]);
    }
    let shown = ["packager".into(), "[fixture arguments redacted]".into()];
    let result = runner()
        .run_tool(
            &argv,
            "packager",
            &RunOptions {
                timeout: Some(Duration::from_secs(30)),
                argv_redacted: Some(&shown),
                log_stdout: false,
                max_output_chars: 0,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        result.returncode,
        Some(0),
        "required Shaka fixture generation failed"
    );
    root.join("media.m3u8")
}
fn request<'a>(
    input: &'a Path,
    output: &'a Path,
    hls: &'a HlsInputKeys,
    empty: &'a InputKeys,
) -> ConversionRequest<'a> {
    ConversionRequest {
        input,
        output,
        format: OutputFormat::Hls,
        target: TargetProtection::Clear,
        input_keys: empty,
        hls_input_keys: hls,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    }
}
fn output_matrix(
    root: &Path,
    c: &Converter,
    encrypted: &Path,
    clear: &Path,
    hls: &HlsInputKeys,
    events: &Collector,
) {
    let empty = InputKeys::default();
    let output_key = OutputKey::from_hex(common::KID, OUTPUT_KEY).unwrap();
    let aes = HlsOutputKey::new(OUTPUT_URI, Aes128Key::from_hex(OUTPUT_KEY).unwrap()).unwrap();
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cbcs,
        TargetProtection::HlsAes128,
    ] {
        let output = root.join(format!("output-{}", target.as_str()));
        let mut selected = request(encrypted, &output, hls, &empty);
        selected.target = target;
        selected.output_key = (target == TargetProtection::Cbcs).then_some(&output_key);
        selected.hls_key_uri = (target == TargetProtection::Cbcs).then_some(OUTPUT_URI);
        selected.hls_output_key = (target == TargetProtection::HlsAes128).then_some(&aes);
        let report = c.convert(selected).unwrap();
        assert!(report.verification.passed);
        assert_eq!(report.input.protection, Protection::Unknown);
        assert_eq!(
            serde_json::to_value(&report).unwrap()["processing"]["route"],
            "mpeg-ts-sample-aes-hls"
        );
        assert_eq!(
            serde_json::to_value(&report).unwrap()["processing"]["input_preparation"],
            "mpeg-ts-sample-aes-decrypt"
        );
        let result_keys =
            InputKeys::new(vec![ContentKey::from_hex(common::KID, OUTPUT_KEY).unwrap()]).unwrap();
        let result_hls = HlsInputKeys::new(vec![(
            OUTPUT_URI.into(),
            Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
        )])
        .unwrap();
        for (source, keys) in [(encrypted, hls), (clear, &HlsInputKeys::default())] {
            let proof = c
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: source,
                        input_keys: &empty,
                        hls_keys: keys,
                    },
                    output: VerificationSource {
                        input: &output.join("master.m3u8"),
                        input_keys: &result_keys,
                        hls_keys: &result_hls,
                    },
                })
                .unwrap();
            assert!(proof.passed);
            assert!(
                proof.checks.packet_payloads_equal
                    && proof.checks.packet_timing_equal
                    && proof
                        .decode_evidence
                        .satisfies(drmr_core::conversion::ValidationDepth::Decoded, proof.scope)
            );
            assert!(!proof.license_playback_verified);
        }
        if target == TargetProtection::Cbcs {
            legacy_fmp4_declarations(c, encrypted, hls, &output, &result_keys, &result_hls);
        }
        let shown = format!(
            "{}{}",
            serde_json::to_string(&events.records()).unwrap(),
            serde_json::to_string(&report).unwrap()
        );
        for private in [
            KEY,
            IV,
            URI,
            OUTPUT_KEY,
            OUTPUT_URI,
            sample_aes_fixture::KEY_URI,
        ] {
            assert!(
                !shown.contains(private),
                "reports and tool events must not expose operator material"
            );
        }
    }
}

fn legacy_fmp4_declarations(
    c: &Converter,
    source: &Path,
    keys: &HlsInputKeys,
    output: &Path,
    result_keys: &InputKeys,
    result_hls: &HlsInputKeys,
) {
    let mut originals = Vec::new();
    for (name, bytes) in common::snapshot_tree(output) {
        if !Path::new(&name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u8"))
        {
            continue;
        }
        let text = std::str::from_utf8(&bytes).unwrap();
        if !text.contains("#EXT-X-MAP:") {
            continue;
        }
        let tag = text
            .lines()
            .find(|line| line.starts_with("#EXT-X-KEY:"))
            .unwrap();
        // Legacy fMP4 reads IVs from its validated container. New TS declaration
        // restrictions must not reinterpret unrelated manifest IV text as media IVs.
        let declarations = format!(
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{OUTPUT_URI}\",KEYFORMAT=\"unsupported-format\",IV=not-a-vector\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{OUTPUT_URI}\",KEYFORMAT=\"identity\",IV=not-a-vector"
        );
        std::fs::write(output.join(&name), text.replace(tag, &declarations)).unwrap();
        originals.push((name, bytes));
    }
    assert_ne!(originals, []);
    let verify = || {
        c.verify(VerificationRequest {
            source: VerificationSource {
                input: source,
                input_keys: &InputKeys::default(),
                hls_keys: keys,
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: result_keys,
                hls_keys: result_hls,
            },
        })
        .unwrap()
    };
    let proof = verify();
    // Container IVs still establish complete legacy media proof; malformed manifest
    // declarations independently fail the public verification's native rule scope.
    assert!(
        (proof.checks.is_pass()
            && proof
                .decode_evidence
                .satisfies(drmr_core::conversion::ValidationDepth::Decoded, proof.scope))
    );
    assert!(
        proof.checks.packet_payloads_equal
            && proof.checks.packet_timing_equal
            && proof
                .decode_evidence
                .satisfies(drmr_core::conversion::ValidationDepth::Decoded, proof.scope)
    );
    assert_eq!(
        serde_json::to_value(&proof.checks).unwrap()["stream_metadata"],
        "pass"
    );
    assert!(!proof.passed);
    let native = proof.native_manifest.as_ref().unwrap();
    assert_eq!(native.errors, originals.len() * 2);
    assert!(!native.is_clean_for_scope());
    assert!(
        native
            .diagnostics
            .iter()
            .all(|issue| issue.rule_id == "HLS-KEY"
                && issue.message_code == "hls-key-declaration-invalid")
    );
    for (name, bytes) in originals {
        std::fs::write(output.join(name), bytes).unwrap();
    }
    let restored = verify();
    assert!(
        restored.passed
            && (restored.checks.is_pass()
                && restored.decode_evidence.satisfies(
                    drmr_core::conversion::ValidationDepth::Decoded,
                    restored.scope
                ))
    );
    assert!(restored.native_manifest.unwrap().is_clean_for_scope());
}
#[test]
fn independently_encrypted_video_including_the_final_sample_preserves_original_media() {
    let root = common::private_tempdir().unwrap();
    let encrypted = shaka_video(&root.path().join("encrypted"), true);
    let clear = shaka_video(&root.path().join("clear"), false);
    let before = common::snapshot_tree(&root.path().join("encrypted"));
    let events = Arc::new(Collector::default());
    let c = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        "packager",
        "ffmpeg",
    )
    .with_decode(true);
    let hls = HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap();
    output_matrix(root.path(), &c, &encrypted, &clear, &hls, &events);
    assert_eq!(
        common::snapshot_tree(&root.path().join("encrypted")),
        before
    );
}

#[test]
fn independently_encrypted_joint_video_and_aac_preserve_all_samples_and_pcm() {
    let root = common::private_tempdir().unwrap();
    let fixture = sample_aes_fixture::source(root.path(), KEY, IV);
    let before_clear = common::snapshot_tree(&root.path().join("clear"));
    let before_cipher = common::snapshot_tree(&root.path().join("encrypted"));
    let events = Arc::new(Collector::default());
    let c = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        "packager",
        "ffmpeg",
    )
    .with_decode(true);
    let hls = HlsInputKeys::new(vec![(
        sample_aes_fixture::KEY_URI.into(),
        Aes128Key::from_hex(KEY).unwrap(),
    )])
    .unwrap();
    output_matrix(
        root.path(),
        &c,
        &fixture.encrypted,
        &fixture.clear,
        &hls,
        &events,
    );
    assert_eq!(
        common::snapshot_tree(&root.path().join("clear")),
        before_clear
    );
    assert_eq!(
        common::snapshot_tree(&root.path().join("encrypted")),
        before_cipher
    );
}

#[test]
fn associated_iv_is_required_and_must_agree_with_the_playlist() {
    let root = common::private_tempdir().unwrap();
    let encrypted = shaka_video(&root.path().join("encrypted"), true);
    let original = std::fs::read_to_string(&encrypted).unwrap();
    assert!(original.contains(&format!(",IV=0x{IV}")));
    let c = Converter::new(runner(), "packager", "ffmpeg").with_decode(true);
    let empty = InputKeys::default();
    let keys = |iv: Option<&str>| {
        HlsInputKeys::new_with_ivs(vec![(
            URI.into(),
            Aes128Key::from_hex(KEY).unwrap(),
            iv.map(|s| SegmentIv::from_hex(s).unwrap()),
        )])
        .unwrap()
    };
    let output = root.path().join("result");
    let conflict = keys(Some("00"));
    assert_eq!(
        c.convert(request(&encrypted, &output, &conflict, &empty))
            .unwrap_err()
            .0,
        "hls-key-iv-conflict"
    );
    assert!(!output.exists());
    std::fs::write(&encrypted, original.replace(&format!(",IV=0x{IV}"), "")).unwrap();
    let before = common::snapshot_tree(&root.path().join("encrypted"));
    assert_eq!(
        c.convert(request(&encrypted, &output, &keys(None), &empty))
            .unwrap_err()
            .0,
        "hls-sample-explicit-iv-required"
    );
    assert!(!output.exists());
    let config = root.path().join("keys.json");
    drmr_core::native::create_private_new(&config).unwrap().write_all(format!(
        "{{\"schema_version\":1,\"input\":{{\"hls_keys\":[{{\"uri\":\"{URI}\",\"key\":\"{KEY}\",\"iv\":\"{IV}\"}}]}}}}"
    ).as_bytes()).unwrap();
    let operator = drmr_core::conversion::key_file::read(&config).unwrap();
    let configured = operator.hls_input;
    assert!(!format!("{configured:?}").contains(IV));
    assert!(
        c.convert(request(&encrypted, &output, &configured, &empty))
            .unwrap()
            .verification
            .passed
    );
    assert!(
        c.verify(VerificationRequest {
            source: VerificationSource {
                input: &encrypted,
                input_keys: &empty,
                hls_keys: &configured
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &empty,
                hls_keys: &HlsInputKeys::default()
            },
        })
        .unwrap()
        .passed
    );
    assert_eq!(
        common::snapshot_tree(&root.path().join("encrypted")),
        before
    );
}

#[test]
fn native_decryption_uses_the_custom_pool_and_releases_it_before_tools() {
    use drmr_core::resources::Limiter;
    let root = common::private_tempdir().unwrap();
    let source = shaka_video(&root.path().join("source"), true);
    let hls = HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap();
    let empty = InputKeys::default();
    let limited = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_limiter(Arc::new(Limiter::new(1, 64 * 1024 * 1024))),
    );
    let output = root.path().join("insufficient");
    assert_eq!(
        Converter::new(limited, "packager", "ffmpeg")
            .with_decode(true)
            .convert(request(&source, &output, &hls, &empty))
            .unwrap_err()
            .0,
        "resource-budget"
    );
    assert!(!output.exists());
    let pool = Arc::new(Limiter::new(1, 512 * 1024 * 1024));
    let custom = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)).with_limiter(Arc::clone(&pool)),
    );
    let c = Converter::new(custom, "packager", "ffmpeg").with_decode(true);
    let output = root.path().join("output");
    assert!(
        c.convert(request(&source, &output, &hls, &empty))
            .unwrap()
            .verification
            .passed
    );
    // A single CPU slot permits this conversion only when the native phase has
    // returned its reservation before each independently admitted media tool.
    let stop = drmr_core::concurrency::Stop::new();
    stop.set();
    let stopped =
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)).with_limiter(pool));
    let cancelled = root.path().join("cancelled");
    assert_eq!(
        Converter::new(stopped, "packager", "ffmpeg")
            .with_decode(true)
            .with_stop(stop)
            .convert(request(&source, &cancelled, &hls, &empty))
            .unwrap_err()
            .0,
        "cancelled"
    );
    assert!(!cancelled.exists());
}

#[test]
fn unsupported_declarations_wrong_keys_and_protection_transitions_publish_nothing() {
    let root = common::private_tempdir().unwrap();
    let source = shaka_video(&root.path().join("source"), true);
    let original = std::fs::read_to_string(&source).unwrap();
    let c = Converter::new(runner(), "packager", "ffmpeg").with_decode(true);
    let keys = HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap();
    let empty = InputKeys::default();
    for (name, changed, token) in [
        (
            "ctr",
            original.replace("METHOD=SAMPLE-AES", "METHOD=SAMPLE-AES-CTR"),
            "ts-sample-key-declaration-not-supported",
        ),
        (
            "format",
            original.replace("KEYFORMAT=\"identity\"", "KEYFORMAT=\"unsupported\""),
            "ts-sample-key-declaration-not-supported",
        ),
        (
            "transition",
            original.replace("segment-2.ts", "#EXT-X-KEY:METHOD=NONE\nsegment-2.ts"),
            "ts-mixed-protection-not-supported",
        ),
    ] {
        assert_ne!(changed, original, "negative fixture must change {name}");
        std::fs::write(&source, changed).unwrap();
        let before = common::snapshot_tree(&root.path().join("source"));
        let output = root.path().join(name);
        assert_eq!(
            c.convert(request(&source, &output, &keys, &empty))
                .unwrap_err()
                .0,
            token
        );
        assert!(!output.exists());
        assert_eq!(common::snapshot_tree(&root.path().join("source")), before);
    }
    std::fs::write(&source, original).unwrap();
    let before = common::snapshot_tree(&root.path().join("source"));
    let wrong =
        HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(OUTPUT_KEY).unwrap())]).unwrap();
    let output = root.path().join("wrong-key");
    let error = c
        .convert(request(&source, &output, &wrong, &empty))
        .unwrap_err();
    assert!(!error.to_string().contains(KEY) && !error.to_string().contains(OUTPUT_KEY));
    assert!(!output.exists());
    assert_eq!(common::snapshot_tree(&root.path().join("source")), before);
}

#[test]
fn simultaneously_active_key_formats_cannot_hide_an_unsupported_or_conflicting_declaration() {
    let root = common::private_tempdir().unwrap();
    let source = shaka_video(&root.path().join("source"), true);
    let original = std::fs::read_to_string(&source).unwrap();
    let identity = original
        .lines()
        .find(|line| line.starts_with("#EXT-X-KEY:"))
        .unwrap();
    let unknown = identity.replace("KEYFORMAT=\"identity\"", "KEYFORMAT=\"unsupported-format\"");
    let fairplay = identity
        .replace(
            "KEYFORMAT=\"identity\"",
            "KEYFORMAT=\"com.apple.streamingkeydelivery\"",
        )
        .replace(IV, "00000000000000000000000000000000");
    let sample_fairplay = identity.replace(
        "KEYFORMAT=\"identity\"",
        "KEYFORMAT=\"com.apple.streamingkeydelivery\"",
    );
    let aes = identity.replace("METHOD=SAMPLE-AES", "METHOD=AES-128");
    let implicit_aes = aes.replace(",KEYFORMAT=\"identity\"", "");
    let c = Converter::new(runner(), "packager", "ffmpeg").with_decode(true);
    let keys = HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap();
    let empty = InputKeys::default();
    for (name, declarations) in [
        ("unknown-first", format!("{unknown}\n{identity}")),
        ("unknown-last", format!("{identity}\n{unknown}")),
        ("conflicting-supported", format!("{fairplay}\n{identity}")),
        ("aes-before-sample", format!("{aes}\n{sample_fairplay}")),
        (
            "sample-before-aes",
            format!("{sample_fairplay}\n{aes}\n{sample_fairplay}"),
        ),
        (
            "implicit-aes-before-sample",
            format!("{implicit_aes}\n{sample_fairplay}"),
        ),
    ] {
        std::fs::write(&source, original.replace(identity, &declarations)).unwrap();
        let before = common::snapshot_tree(&root.path().join("source"));
        let output = root.path().join(name);
        assert_eq!(
            c.convert(request(&source, &output, &keys, &empty))
                .unwrap_err()
                .0,
            "ts-sample-key-declaration-not-supported"
        );
        assert!(!output.exists());
        assert_eq!(common::snapshot_tree(&root.path().join("source")), before);
    }
    // Replacing a declaration of the same format before media is a valid
    // replacement, even when its previous method was different.
    std::fs::write(
        &source,
        original.replace(identity, &format!("{aes}\n{identity}")),
    )
    .unwrap();
    let before = common::snapshot_tree(&root.path().join("source"));
    let output = root.path().join("same-format-replacement");
    assert!(
        c.convert(request(&source, &output, &keys, &empty))
            .unwrap()
            .verification
            .passed
    );
    assert!(
        c.verify(VerificationRequest {
            source: VerificationSource {
                input: &source,
                input_keys: &empty,
                hls_keys: &keys
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &empty,
                hls_keys: &HlsInputKeys::default()
            },
        })
        .unwrap()
        .passed
    );
    assert_eq!(common::snapshot_tree(&root.path().join("source")), before);
}

#[test]
fn same_format_key_and_associated_iv_rotation_preserve_every_original_track() {
    let root = common::private_tempdir().unwrap();
    let fixture = sample_aes_fixture::source(root.path(), KEY, IV);
    let second_uri = "https://operator.invalid/rotated-sample-key";
    let second_iv = "404142434445464748494a4b4c4d4e4f";
    let key_bytes: [u8; 16] = hex::decode(OUTPUT_KEY).unwrap().try_into().unwrap();
    let iv_bytes: [u8; 16] = hex::decode(second_iv).unwrap().try_into().unwrap();
    let original = std::fs::read_to_string(&fixture.encrypted).unwrap();
    let clear = fixture.clear.parent().unwrap();
    let encrypted = fixture.encrypted.parent().unwrap();
    let mut text = String::new();
    let mut cohort = Vec::new();
    let mut ordinal = 0;
    for line in original.lines() {
        if !line.starts_with('#') && !line.is_empty() {
            if ordinal >= 2 {
                let name = format!("rotated-{ordinal}.ts");
                sample_aes_fixture::encrypt_segment(
                    &clear.join(line),
                    &encrypted.join(&name),
                    &key_bytes,
                    &iv_bytes,
                );
                writeln!(text, "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{second_uri}\",KEYFORMAT=\"identity\"\n{name}").unwrap();
                cohort.push(encrypted.join(name));
            } else {
                text.push_str(line);
                text.push('\n');
                cohort.push(encrypted.join(line));
            }
            ordinal += 1;
        } else {
            text.push_str(line);
            text.push('\n');
        }
    }
    assert_eq!(ordinal, 5);
    sample_aes_fixture::normalize_continuity(&cohort);
    std::fs::write(&fixture.encrypted, text).unwrap();
    let before = common::snapshot_tree(root.path());
    let hls = HlsInputKeys::new_with_ivs(vec![
        (
            sample_aes_fixture::KEY_URI.into(),
            Aes128Key::from_hex(KEY).unwrap(),
            None,
        ),
        (
            second_uri.into(),
            Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
            Some(SegmentIv::from_hex(second_iv).unwrap()),
        ),
    ])
    .unwrap();
    let c = Converter::new(runner(), "packager", "ffmpeg").with_decode(true);
    let output = root.path().join("output");
    let empty = InputKeys::default();
    assert!(
        c.convert(request(&fixture.encrypted, &output, &hls, &empty))
            .unwrap()
            .verification
            .passed
    );
    for (source, keys) in [
        (&fixture.encrypted, &hls),
        (&fixture.clear, &HlsInputKeys::default()),
    ] {
        assert!(
            c.verify(VerificationRequest {
                source: VerificationSource {
                    input: source,
                    input_keys: &empty,
                    hls_keys: keys
                },
                output: VerificationSource {
                    input: &output.join("master.m3u8"),
                    input_keys: &empty,
                    hls_keys: &HlsInputKeys::default()
                },
            })
            .unwrap()
            .passed
        );
    }
    for (name, bytes) in before {
        assert_eq!(std::fs::read(root.path().join(name)).unwrap(), bytes);
    }
}
