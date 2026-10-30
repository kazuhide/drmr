//! Generated HLS DRM records remain independently usable as known-key inputs.
mod common;
#[path = "support/hls_multi_drm_profiles.rs"]
mod profiles;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use drmr_core::{
    conversion::{
        ConversionRequest, Converter, DecodeEvidence, HlsInputKeys, InputKeys, ProcessingMode,
        SourceValidationPurpose, VerificationRequest, VerificationSource, key_file,
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const URI: &str = "skd://synthetic-hls-multi-drm";
const LICENSE: &str = "https://example.invalid/license?private=HLS-MULTI-DRM-MARKER";
fn converter() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    (
        Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
            "packager",
            "must-not-run-hls-drm-ffmpeg",
        )
        .with_ffprobe("must-not-run-hls-drm-ffprobe"),
        events,
    )
}
fn pssh(target: TargetProtection, kid: &str) -> String {
    let mut payload = hex::decode(format!("1210{kid}")).unwrap();
    payload.push(0x48);
    let mut value = u32::from_be_bytes(if target == TargetProtection::Cenc {
        *b"cenc"
    } else {
        *b"cbcs"
    });
    while value > 127 {
        payload.push(u8::try_from(value & 127).unwrap() | 128);
        value >>= 7;
    }
    payload.push(u8::try_from(value).unwrap());
    hex::encode(
        drmr_core::playready::build_pssh_box(&drmr_core::playready::WIDEVINE_SYSTEM_ID, &payload)
            .unwrap(),
    )
}
fn operator_keys(
    root: &Path,
    name: &str,
    target: TargetProtection,
    format: OutputFormat,
) -> key_file::OperatorKeys {
    let path = root.join(format!("{name}-keys.json"));
    let mut output = serde_json::json!({"kid":KID,"key":KEY,"signaling":{
        "system":"multi-drm","widevine_pssh":pssh(target,KID),"playready_license_url":LICENSE,
        "fairplay":target == TargetProtection::Cbcs && format == OutputFormat::Hls
    }});
    if target == TargetProtection::Cbcs && format == OutputFormat::Hls {
        output["hls_key_uri"] = URI.into();
    }
    drmr_core::native::create_private_new(&path)
        .unwrap()
        .write_all(
            serde_json::json!({"schema_version":1,"output":output})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    key_file::read(&path).unwrap()
}
fn source(root: &Path) -> PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(clip)
        .args([
            "-t",
            "4",
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-bf",
            "2",
            "-g",
            "48",
            "-c:a",
            "aac",
            "-ar",
            "44100",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(root.join("part-%d.ts"))
        .arg(root.join("media.m3u8"))
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "licensed fixture generation failed"
    );
    root.join("media.m3u8")
}
fn view<'a>(path: &'a Path, keys: &'a InputKeys, hls: &'a HlsInputKeys) -> VerificationSource<'a> {
    VerificationSource {
        input: path,
        input_keys: keys,
        hls_keys: hls,
    }
}
fn convert(
    command: &Converter,
    input: &Path,
    input_keys: &InputKeys,
    output: &Path,
    format: OutputFormat,
    target: TargetProtection,
    keys: &key_file::OperatorKeys,
) {
    let hls = HlsInputKeys::default();
    let report = command
        .convert(ConversionRequest {
            input,
            output,
            format,
            target,
            input_keys,
            hls_input_keys: &hls,
            output_key: keys.output.as_ref(),
            hls_key_uri: (format == OutputFormat::Hls)
                .then(|| keys.hls_output_uri())
                .flatten(),
            hls_output_key: None,
        })
        .unwrap_or_else(|error| panic!("{format:?}/{target:?}: {error}"));
    assert!(
        report.verification.passed && report.verification.checks.is_pass(),
        "{:?}",
        report.verification
    );
    assert_eq!(
        report.verification.decode_evidence,
        DecodeEvidence::not_requested()
    );
    assert!(!report.verification.license_playback_verified);
}
fn playlists(root: &Path) -> Vec<PathBuf> {
    common::snapshot_tree(root)
        .into_iter()
        .filter_map(|(name, bytes)| {
            (Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u8"))
                && bytes.windows(8).any(|part| part == b"#EXTINF:"))
            .then(|| root.join(name))
        })
        .collect()
}
fn fresh(
    _command: &Converter,
    original: &Path,
    output: &Path,
    keys: &key_file::OperatorKeys,
    target: TargetProtection,
    format: OutputFormat,
) {
    let (command, _) = converter();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let result_keys = keys.output_verification_keys().unwrap();
    let report = command
        .verify(VerificationRequest {
            source: view(original, &empty, &hls),
            output: view(output, &result_keys, &hls),
        })
        .unwrap();
    assert!(report.passed && report.checks.is_pass(), "{report:?}");
    assert_eq!(report.decode_evidence, DecodeEvidence::not_requested());
    command
        .verify_output_signaling(output, format, target, keys)
        .unwrap();
}
fn reject_declarations(
    command: &Converter,
    entrypoint: &Path,
    target: TargetProtection,
    keys: &key_file::OperatorKeys,
) {
    let playlist = playlists(entrypoint.parent().unwrap()).remove(0);
    let original = std::fs::read_to_string(&playlist).unwrap();
    let wrong = format!(
        "data:text/plain;base64,{}",
        STANDARD.encode(hex::decode(pssh(target, "100102030405060708090a0b0c0d0e0f")).unwrap())
    );
    let wv = format!(
        "data:text/plain;base64,{}",
        STANDARD.encode(hex::decode(pssh(target, KID)).unwrap())
    );
    let key_bytes: [u8; 16] = hex::decode(KEY).unwrap().try_into().unwrap();
    let make_pro = |kid: &str| {
        if target == TargetProtection::Cenc {
            drmr_core::playready::build_playready_v41(kid, LICENSE, &key_bytes).unwrap()
        } else {
            drmr_core::playready::build_playready_v43(kid, LICENSE).unwrap()
        }
    };
    let pro_uri = format!("data:text/plain;base64,{}", make_pro(KID).pro_b64);
    let wrong_pro_uri = format!(
        "data:text/plain;base64,{}",
        make_pro("100102030405060708090a0b0c0d0e0f").pro_b64
    );
    let method = if target == TargetProtection::Cenc {
        "SAMPLE-AES-CTR"
    } else {
        "SAMPLE-AES"
    };
    let other = if target == TargetProtection::Cenc {
        "SAMPLE-AES"
    } else {
        "SAMPLE-AES-CTR"
    };
    for changed in [
        original.replace(&wv, &wrong),
        original.replace(&pro_uri, &wrong_pro_uri),
        original.replace(&format!("METHOD={method},"), &format!("METHOD={other},")),
        original.replace("KEYFORMATVERSIONS=\"1\"", "KEYFORMATVERSIONS=\"2\""),
        original.replace("KEYFORMATVERSIONS=\"1\"", "KEYFORMATVERSIONS=\"1\",IV=0x00"),
    ] {
        assert_ne!(changed, original);
        std::fs::write(&playlist, changed).unwrap();
        assert!(
            command
                .verify_output_signaling(entrypoint, OutputFormat::Hls, target, keys)
                .is_err()
        );
        let result_keys = keys.output_verification_keys().unwrap();
        let hls = HlsInputKeys::default();
        assert_eq!(
            command
                .validate_source(
                    view(entrypoint, &result_keys, &hls),
                    SourceValidationPurpose::Conversion
                )
                .unwrap_err()
                .0,
            "hls-drm-signaling-invalid"
        );
    }
    std::fs::write(&playlist, &original).unwrap();
    let master = std::fs::read_to_string(entrypoint).unwrap();
    let session_master = master
        .lines()
        .filter(|line| !line.starts_with("#EXT-X-VERSION:"))
        .collect::<Vec<_>>()
        .join("\n")
        .replacen("#EXTM3U", "#EXTM3U\n#EXT-X-VERSION:6", 1);
    let value = original
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-KEY:"))
        .unwrap();
    std::fs::write(
        entrypoint,
        format!("{session_master}\n#EXT-X-SESSION-KEY:{value}\n"),
    )
    .unwrap();
    drmr_core::conversion::FrozenSource::capture(entrypoint, &drmr_core::concurrency::Stop::new())
        .unwrap();
    command
        .verify_output_signaling(entrypoint, OutputFormat::Hls, target, keys)
        .unwrap();
    std::fs::write(
        entrypoint,
        format!(
            "{session_master}\n#EXT-X-SESSION-KEY:{}\n",
            value.replace("KEYFORMATVERSIONS=\"1\"", "KEYFORMATVERSIONS=\"2\"")
        ),
    )
    .unwrap();
    assert!(
        command
            .verify_output_signaling(entrypoint, OutputFormat::Hls, target, keys)
            .is_err()
    );
    std::fs::write(entrypoint, master).unwrap();
}
#[test]
fn hls_provider_records_round_trip_in_stitch_and_native_segments_without_decoding() {
    let root = common::private_tempdir().unwrap();
    let original = source(&root.path().join("original"));
    let (command, _) = converter();
    let empty = InputKeys::default();
    let bare = key_file::OperatorKeys::default();
    let dash = root.path().join("clear-dash");
    convert(
        &command,
        &original,
        &empty,
        &dash,
        OutputFormat::Dash,
        TargetProtection::Clear,
        &bare,
    );
    for mode in [ProcessingMode::Stitch, ProcessingMode::Segments] {
        for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
            let name = format!("{mode:?}-{target:?}");
            let keys = operator_keys(root.path(), &name, target, OutputFormat::Hls);
            let output = root.path().join(&name);
            let (command, events) = converter();
            let command = command
                .with_processing(mode)
                .with_output_signaling(keys.signaling.clone());
            let input = if mode == ProcessingMode::Segments {
                dash.join("manifest.mpd")
            } else {
                original.clone()
            };
            convert(
                &command,
                &input,
                &empty,
                &output,
                OutputFormat::Hls,
                target,
                &keys,
            );
            let entrypoint = output.join("master.m3u8");
            fresh(
                &command,
                &original,
                &entrypoint,
                &keys,
                target,
                OutputFormat::Hls,
            );
            reject_declarations(&command, &entrypoint, target, &keys);
            let next = if target == TargetProtection::Cenc {
                TargetProtection::Cbcs
            } else {
                TargetProtection::Cenc
            };
            let next_keys = operator_keys(
                root.path(),
                &format!("{name}-next"),
                next,
                OutputFormat::Hls,
            );
            let input_keys = keys.output_verification_keys().unwrap();
            let next_output = root.path().join(format!("{name}-reinput"));
            let (next_command, _) = converter();
            let next_command = next_command
                .with_processing(mode)
                .with_output_signaling(next_keys.signaling.clone());
            convert(
                &next_command,
                &entrypoint,
                &input_keys,
                &next_output,
                OutputFormat::Hls,
                next,
                &next_keys,
            );
            fresh(
                &next_command,
                &original,
                &next_output.join("master.m3u8"),
                &next_keys,
                next,
                OutputFormat::Hls,
            );
            let records = serde_json::to_string(&events.records()).unwrap();
            for marker in provider_markers(target) {
                assert!(
                    !records.contains(&marker),
                    "operator metadata appeared in progress records"
                );
            }
        }
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}
#[test]
fn generic_hls_identity_uses_an_explicit_uri_and_keeps_both_encryption_schemes() {
    let root = common::private_tempdir().unwrap();
    let input = fixture("clip.mp4");
    let (command, _) = converter();
    let empty = InputKeys::default();
    let dash = root.path().join("clear-dash");
    let bare = key_file::OperatorKeys::default();
    convert(
        &command,
        &input,
        &empty,
        &dash,
        OutputFormat::Dash,
        TargetProtection::Clear,
        &bare,
    );
    for mode in [ProcessingMode::Stitch, ProcessingMode::Segments] {
        for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
            let name = format!("identity-{mode:?}-{target:?}");
            let path = root.path().join(format!("{name}.keys.json"));
            drmr_core::native::create_private_new(&path).unwrap().write_all(serde_json::json!({"schema_version":1,
                "output":{"kid":KID,"key":KEY,"hls_key_uri":"https://keys.example.invalid/identity-hls"}}).to_string().as_bytes()).unwrap();
            let keys = key_file::read(&path).unwrap();
            let output = root.path().join(&name);
            let source = if mode == ProcessingMode::Segments {
                dash.join("manifest.mpd")
            } else {
                input.clone()
            };
            let (command, _) = converter();
            let command = command.with_processing(mode);
            convert(
                &command,
                &source,
                &empty,
                &output,
                OutputFormat::Hls,
                target,
                &keys,
            );
            fresh(
                &command,
                &input,
                &output.join("master.m3u8"),
                &keys,
                target,
                OutputFormat::Hls,
            );
            for playlist in playlists(&output) {
                let text = std::fs::read_to_string(playlist).unwrap();
                assert!(text.contains("KEYFORMAT=\"identity\""));
                assert!(text.contains(if target == TargetProtection::Cenc {
                    "METHOD=SAMPLE-AES-CTR,"
                } else {
                    "METHOD=SAMPLE-AES,"
                }));
            }
        }
    }
    for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
        let output = root.path().join(format!("missing-uri-{target:?}"));
        let hls = HlsInputKeys::default();
        let key = drmr_core::conversion::OutputKey::from_hex(KID, KEY).unwrap();
        assert_eq!(
            command
                .convert(ConversionRequest {
                    input: &input,
                    output: &output,
                    format: OutputFormat::Hls,
                    target,
                    input_keys: &empty,
                    hls_input_keys: &hls,
                    output_key: Some(&key),
                    hls_key_uri: None,
                    hls_output_key: None
                })
                .unwrap_err()
                .0,
            "hls-key-uri-required"
        );
        assert!(!output.exists());
    }
}

fn provider_markers(target: TargetProtection) -> Vec<String> {
    let key: [u8; 16] = hex::decode(KEY).unwrap().try_into().unwrap();
    let playready = if target == TargetProtection::Cenc {
        drmr_core::playready::build_playready_v41(KID, LICENSE, &key).unwrap()
    } else {
        drmr_core::playready::build_playready_v43(KID, LICENSE).unwrap()
    };
    let widevine = pssh(target, KID);
    let encoded = STANDARD.encode(hex::decode(&widevine).unwrap());
    vec![
        KEY.into(),
        URI.into(),
        LICENSE.into(),
        widevine,
        encoded.clone(),
        format!("data:text/plain;base64,{encoded}"),
        playready.pro_b64.clone(),
        format!("data:text/plain;base64,{}", playready.pro_b64),
        hex::encode(playready.pro_bytes),
        playready.pssh_b64,
        playready.pssh_hex,
        playready.wrmheader_xml,
    ]
}
#[cfg(unix)]
#[test]
fn failed_tools_redact_plain_and_encoded_provider_metadata_before_packaging() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = common::private_tempdir().unwrap();
    let input = fixture("clip.mp4");
    let keys = operator_keys(
        root.path(),
        "failure",
        TargetProtection::Cbcs,
        OutputFormat::Hls,
    );
    let markers = provider_markers(TargetProtection::Cbcs);
    let script = root.path().join("private-packager");
    let text = markers.join("\n").replace('\'', "'\\''");
    drmr_core::native::create_private_new(&script)
        .unwrap()
        .write_all(format!("#!/bin/sh\nprintf '%s\\n' '{text}' >&2\nexit 1\n").as_bytes())
        .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let events = Arc::new(Collector::default());
    let secrets = Arc::new(Secrets::new());
    let command = Converter::new(
        Arc::new(ToolRunner::new(secrets.clone(), events.clone())),
        script.to_string_lossy(),
        "must-not-run-ffmpeg",
    )
    .with_ffprobe("must-not-run-ffprobe")
    .with_output_signaling(keys.signaling.clone());
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let output = root.path().join("result");
    assert_eq!(
        command
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::Hls,
                target: TargetProtection::Cbcs,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: keys.output.as_ref(),
                hls_key_uri: keys.hls_output_uri(),
                hls_output_key: None
            })
            .unwrap_err()
            .0,
        "media-tool-failed"
    );
    assert!(!output.exists());
    let records = serde_json::to_string(&events.records()).unwrap();
    for (index, marker) in markers.into_iter().enumerate() {
        if index < 11 {
            assert!(
                secrets.scrub(&marker) == drmr_core::toolrun::REDACTED,
                "operator encoding index {index} was not registered before the failing tool"
            );
        } else {
            assert!(!secrets.scrub(&marker).contains(LICENSE));
        }
        assert!(
            !records.contains(&marker),
            "encoded provider metadata leaked from a failed tool"
        );
    }
    // Conversion omits external output text. The registry also covers every
    // complete encoding for any consumer that explicitly requests tool text.
    assert!(
        events
            .records()
            .iter()
            .any(|record| record.get("returncode") == Some(&serde_json::json!(1)))
    );
}
