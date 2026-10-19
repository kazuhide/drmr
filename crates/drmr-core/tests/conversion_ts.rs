//! Actual local transport-stream inputs retain absolute clocks and complete media.
mod common;
use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, HlsInputKeys, HlsOutputKey, InputKeys,
        OutputKey, VerificationRequest, VerificationSource,
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{path::Path, sync::Arc};
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
}
fn source(root: &Path) -> std::path::PathBuf {
    source_profile(root, "av")
}
fn source_profile(root: &Path, profile: &str) -> std::path::PathBuf {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let manifest = root.join("media.m3u8");
    let mut command = std::process::Command::new("ffmpeg");
    command
        .args(["-v", "error", "-threads", "1", "-i"])
        .arg(input);
    if profile != "audio" {
        command.args(["-map", "0:v:0"]);
    }
    if !matches!(profile, "video" | "interlaced-video") {
        command.args(["-map", "0:a:0"]);
    }
    command.args(["-c", "copy"]);
    if matches!(profile, "bframes" | "interlaced" | "interlaced-video") {
        command.args(["-c:v", "libx264", "-threads", "1", "-bf", "3", "-g", "48"]);
    }
    if matches!(profile, "interlaced" | "interlaced-video") {
        command.args(["-flags", "+ilme+ildct", "-x264-params", "tff=1"]);
    }
    if profile == "44100" {
        command.args(["-c:a", "aac", "-ar", "44100"]);
    }
    let result = command
        .args([
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-start_number",
            "7",
            "-hls_segment_filename",
        ])
        .arg(root.join("seg-%02d.ts"))
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(result.status.success(), "TS fixture generation failed");
    manifest
}
#[test]
fn transport_stream_to_each_supported_hls_protection_preserves_all_original_tracks() {
    let root = common::private_tempdir().unwrap();
    let input = source(root.path());
    let before = common::snapshot_tree(root.path());
    let c = converter();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    let aes = HlsOutputKey::new(
        "https://operator.invalid/output",
        Aes128Key::from_hex(KEY).unwrap(),
    )
    .unwrap();
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cbcs,
        TargetProtection::HlsAes128,
    ] {
        let output = root.path().join(target.as_str());
        let report = c
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::Hls,
                target,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: (target == TargetProtection::Cbcs).then_some(&key),
                hls_key_uri: (target == TargetProtection::Cbcs)
                    .then_some("https://operator.invalid/output"),
                hls_output_key: (target == TargetProtection::HlsAes128).then_some(&aes),
            })
            .unwrap();
        assert_eq!(
            serde_json::to_value(&report).unwrap()["processing"]["route"],
            "mpeg-ts-hls"
        );
        assert_eq!(
            serde_json::to_value(&report).unwrap()["processing"]["input_preparation"],
            "mpeg-ts-demux"
        );
        assert!(report.verification.passed);
        assert!(!report.verification.license_playback_verified);
        let result_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
        let result_hls = HlsInputKeys::new(vec![(
            "https://operator.invalid/output".into(),
            Aes128Key::from_hex(KEY).unwrap(),
        )])
        .unwrap();
        let verified = c
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &output.join("master.m3u8"),
                    input_keys: &result_keys,
                    hls_keys: &result_hls,
                },
            })
            .unwrap();
        assert!(verified.passed);
    }
    for (name, bytes) in before {
        assert_eq!(std::fs::read(root.path().join(name)).unwrap(), bytes);
    }
}

fn clear_request<'a>(
    input: &'a Path,
    output: &'a Path,
    keys: &'a InputKeys,
    hls: &'a HlsInputKeys,
) -> ConversionRequest<'a> {
    ConversionRequest {
        input,
        output,
        format: OutputFormat::Hls,
        target: TargetProtection::Clear,
        input_keys: keys,
        hls_input_keys: hls,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    }
}
#[test]
fn single_track_and_b_frame_sources_preserve_their_absolute_clocks() {
    for profile in ["video", "bframes"] {
        let root = common::private_tempdir().unwrap();
        let input = source_profile(root.path(), profile);
        let output = root.path().join("result");
        let keys = InputKeys::default();
        let hls = HlsInputKeys::default();
        let c = converter();
        assert!(
            c.convert(clear_request(&input, &output, &keys, &hls))
                .unwrap_or_else(|e| panic!("{profile}: {e}"))
                .verification
                .passed
        );
        assert!(
            c.verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &keys,
                    hls_keys: &hls
                },
                output: VerificationSource {
                    input: &output.join("master.m3u8"),
                    input_keys: &keys,
                    hls_keys: &hls
                }
            })
            .unwrap()
            .passed
        );
    }
}

fn interlaced_hls_command(write_edits: bool) -> std::process::Command {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut command = std::process::Command::new("ffmpeg");
    command
        .args(["-v", "error", "-threads", "1", "-i"])
        .arg(fixture)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-flags",
            "+ilme+ildct",
            "-x264-params",
            "tff=1",
            "-output_ts_offset",
            "2",
            "-hls_segment_type",
            "fmp4",
            "-hls_segment_options",
            if write_edits {
                "video_track_timescale=90000"
            } else {
                "video_track_timescale=90000:use_editlist=0"
            },
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ]);
    command
}

fn interlaced_hls(root: &Path, write_edits: bool) -> std::path::PathBuf {
    let playlist = root.join("media.m3u8");
    // FFmpeg 9.0.2 hlsenc.c resolves the default init file using '/' in the playlist URL.
    // A dedicated child directory and relative outputs keep every resource beside the playlist.
    let result = interlaced_hls_command(write_edits)
        .current_dir(root)
        .arg("segment-%d.m4s")
        .arg("media.m3u8")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "interlaced HLS fixture generation failed"
    );
    assert_interlaced_resources(&playlist);
    assert_interlaced_native_frames(&playlist);
    playlist
}

fn assert_interlaced_native_frames(playlist: &Path) {
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_frames",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,time_base,nb_read_frames:frame=interlaced_frame,top_field_first",
            "-of",
            "json",
        ])
        .arg(playlist)
        .output()
        .unwrap();
    assert!(probe.status.success());
    let value: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(value["streams"][0]["codec_name"], "h264");
    assert_eq!(value["streams"][0]["time_base"], "1/90000");
    assert_eq!(value["streams"][0]["nb_read_frames"], "240");
    let frames = value["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 240);
    assert!(frames.iter().all(|frame| frame["interlaced_frame"] == 1
        && matches!(frame["top_field_first"].as_u64(), Some(0 | 1))));
}

fn assert_interlaced_resources(playlist: &Path) {
    let root = playlist.parent().unwrap();
    let text = std::fs::read_to_string(playlist).unwrap();
    let mut names = vec!["media.m3u8"];
    let mut maps = 0;
    for line in text.lines() {
        if let Some(map) = line.strip_prefix("#EXT-X-MAP:URI=\"") {
            assert_eq!(map, "init.mp4\"");
            maps += 1;
            names.push("init.mp4");
        } else if !line.is_empty() && !line.starts_with('#') {
            assert!(line.starts_with("segment-"));
            assert_eq!(
                Path::new(line).extension(),
                Some(std::ffi::OsStr::new("m4s"))
            );
            names.push(line);
        }
    }
    assert_eq!(maps, 1);
    assert!(names.len() > 2);
    for name in names {
        assert!(!name.contains(['/', '\\', ':']));
        assert_eq!(
            Path::new(name).file_name(),
            Some(Path::new(name).as_os_str())
        );
        assert!(
            std::fs::symlink_metadata(root.join(name))
                .unwrap()
                .is_file()
        );
        let held = std::fs::File::open(root.join(name)).unwrap_or_else(|error| {
            panic!(
                "fixture resource {name}: {:?}/{:?}",
                error.kind(),
                error.raw_os_error()
            )
        });
        assert!(held.metadata().unwrap().is_file());
        assert_ne!(std::fs::read(root.join(name)).unwrap(), [] as [u8; 0]);
    }
}

#[cfg(windows)]
fn observe_legacy_interlaced_init_placement() {
    let control = common::private_tempdir().unwrap();
    let output = control.path().join("playlist");
    drmr_core::native::create_private_dir_all(&output).unwrap();
    let result = interlaced_hls_command(true)
        .current_dir(control.path())
        .arg(output.join("segment-%d.m4s"))
        .arg(output.join("media.m3u8"))
        .output()
        .unwrap();
    assert!(result.status.success());
    let in_playlist = output.join("init.mp4").is_file();
    let in_working_directory = control.path().join("init.mp4").is_file();
    eprintln!(
        "legacy-backslash-init-placement: playlist_parent={in_playlist}; controlled_cwd={in_working_directory}"
    );
    // Either tool behavior is acceptable here; only the fixed fixture must be fully confined.
    assert!(in_playlist || in_working_directory);
}

fn acquisition_report(input: &Path) -> Vec<String> {
    let root = input.parent().unwrap();
    let mut pending = vec![root.to_path_buf()];
    let mut report = vec![format!(
        "input: {:?}",
        std::fs::File::open(input)
            .and_then(|file| file.metadata().map(|metadata| metadata.len()))
            .map_err(|error| (error.kind(), error.raw_os_error()))
    )];
    while let Some(path) = pending.pop() {
        if report.len() >= 128 {
            report.push("diagnostic entry limit reached".into());
            break;
        }
        let relative = path.strip_prefix(root).unwrap().display();
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => match std::fs::read_dir(&path) {
                Ok(entries) => {
                    pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
                }
                Err(error) => report.push(format!(
                    "{relative}: {:?}/{:?}",
                    error.kind(),
                    error.raw_os_error()
                )),
            },
            Ok(_) => report.push(format!(
                "{relative}: {:?}",
                std::fs::File::open(&path)
                    .and_then(|file| file.metadata().map(|metadata| metadata.len()))
                    .map_err(|error| (error.kind(), error.raw_os_error()))
            )),
            Err(error) => report.push(format!(
                "{relative}: {:?}/{:?}",
                error.kind(),
                error.raw_os_error()
            )),
        }
    }
    report.sort();
    report
}

fn proxy_roles(audit: &Path) -> Vec<String> {
    std::fs::read_dir(audit)
        .unwrap()
        .map(|entry| {
            let call = entry.unwrap().path();
            format!(
                "{}: {:?}",
                call.file_name().unwrap().to_string_lossy(),
                std::fs::read_to_string(call.join("role"))
            )
        })
        .collect()
}

#[test]
fn complete_frame_evidence_rejects_an_interlaced_source_or_actual_hls_result() {
    #[cfg(windows)]
    observe_legacy_interlaced_init_placement();
    let source_root = common::private_tempdir().unwrap();
    let input = source_profile(source_root.path(), "video");
    let output = source_root.path().join("result");
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let c = converter();
    assert!(
        c.convert(clear_request(&input, &output, &keys, &hls))
            .unwrap()
            .verification
            .passed
    );
    let result_root = common::private_tempdir().unwrap();
    // This candidate isolates frame evidence from unsupported empty/multiple edits.
    // Disabling fixture edit writing changes its absolute origin; verification never shifts it.
    let interlaced_result = interlaced_hls(result_root.path(), false);
    let complex_root = common::private_tempdir().unwrap();
    let complex_result = interlaced_hls(complex_root.path(), true);
    let interlaced_root = common::private_tempdir().unwrap();
    let interlaced_source = source_profile(interlaced_root.path(), "interlaced-video");
    let originals = [
        source_root.path(),
        result_root.path(),
        interlaced_root.path(),
        complex_root.path(),
    ]
    .map(|path| (path, common::snapshot_tree(path)));
    // Keep the original fixture's complex-edit refusal as its own exact boundary.
    assert_eq!(
        c.verify(VerificationRequest {
            source: VerificationSource {
                input: &input,
                input_keys: &keys,
                hls_keys: &hls
            },
            output: VerificationSource {
                input: &complex_result,
                input_keys: &keys,
                hls_keys: &hls
            },
        })
        .unwrap_err()
        .0,
        "presentation-window-not-supported"
    );
    let clear_result = output.join("master.m3u8");
    let real_probe = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| drmr_test_support::program_path(&directory.join("ffprobe")))
        .find(|path| path.is_file())
        .map(|path| std::path::absolute(path).unwrap())
        .expect("actual FFprobe must be installed");
    for (role, source, result) in [
        ("mp4", input.as_path(), interlaced_result.as_path()),
        (
            "transport",
            interlaced_source.as_path(),
            clear_result.as_path(),
        ),
    ] {
        let proxy_root = common::private_tempdir().unwrap();
        let audit = proxy_root.path().join("audit");
        std::fs::create_dir(&audit).unwrap();
        let proxy = drmr_test_support::install(
            &proxy_root.path().join("probe"),
            &drmr_test_support::ToolSpec {
                mode: drmr_test_support::Mode::FfprobeProxy,
                ..Default::default()
            }
            .with("real_probe", real_probe.to_str().unwrap())
            .with("omit_role", role)
            .with("audit_dir", audit.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(
            converter()
                .with_ffprobe(proxy.to_str().unwrap())
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: source,
                        input_keys: &keys,
                        hls_keys: &hls
                    },
                    output: VerificationSource {
                        input: result,
                        input_keys: &keys,
                        hls_keys: &hls
                    },
                })
                .unwrap_err()
                .0,
            "ts-progressive-frame-evidence-required",
            "selected_role={role}; source={:?}; result={:?}; proxy_roles={:?}",
            acquisition_report(source),
            acquisition_report(result),
            proxy_roles(&audit)
        );
        assert_profile_omission(&audit, role);
    }
    for (path, before) in originals {
        assert_eq!(common::snapshot_tree(path), before);
    }
}

fn remove_json_field(value: &mut serde_json::Value, key: &str) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            usize::from(object.remove(key).is_some())
                + object
                    .values_mut()
                    .map(|child| remove_json_field(child, key))
                    .sum::<usize>()
        }
        serde_json::Value::Array(array) => array
            .iter_mut()
            .map(|child| remove_json_field(child, key))
            .sum(),
        _ => 0,
    }
}

fn assert_profile_omission(audit: &Path, selected_role: &str) {
    let mut selected = 0;
    let mut selected_reference = 0;
    let mut unselected = 0;
    for entry in std::fs::read_dir(audit).unwrap() {
        let call = entry.unwrap().path();
        let role = std::fs::read_to_string(call.join("role")).unwrap();
        let is_selected = std::fs::read_to_string(call.join("selected")).unwrap() == "yes";
        assert_eq!(is_selected, role == selected_role);
        let mut before: serde_json::Value =
            serde_json::from_slice(&std::fs::read(call.join("before.json")).unwrap()).unwrap();
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(call.join("after.json")).unwrap()).unwrap();
        assert_eq!(before["streams"].as_array().unwrap().len(), 1);
        assert_eq!(after["streams"].as_array().unwrap().len(), 1);
        let entries = std::fs::read_to_string(call.join("before.entries")).unwrap();
        let forwarded = std::fs::read_to_string(call.join("after.entries")).unwrap();
        assert!(
            entries
                .split([',', ':', '='])
                .any(|field| field == "field_order")
        );
        assert!(remove_json_field(&mut before, "codec_tag_string") > 0);
        if is_selected {
            selected += 1;
            let expected_extension = if selected_role == "transport" {
                "ts"
            } else {
                "m3u8"
            };
            if std::fs::read_to_string(call.join("input.extension")).unwrap() == expected_extension
            {
                selected_reference += 1;
            }
            assert_eq!(
                forwarded,
                entries
                    .split(',')
                    .filter(|field| *field != "field_order")
                    .collect::<Vec<_>>()
                    .join(",")
            );
            remove_json_field(&mut before, "field_order");
            let mut omitted = after.clone();
            assert_eq!(remove_json_field(&mut omitted, "field_order"), 0);
        } else {
            unselected += 1;
            assert_eq!(entries, forwarded);
        }
        assert_eq!(before, after, "only the selected profile field may change");
    }
    assert!(
        selected > 0,
        "selected role was not probed with omitted metadata"
    );
    assert!(unselected > 0, "independent unselected role was not probed");
    assert!(
        selected_reference > 0,
        "the actual selected original TS or result playlist profile must omit the field"
    );
}
#[test]
fn unsupported_audio_and_interlaced_video_fail_before_publication() {
    for (profile, expected) in [
        ("44100", "ts-aac-sample-rate-not-supported"),
        ("interlaced", "ts-progressive-frame-evidence-required"),
    ] {
        let root = common::private_tempdir().unwrap();
        let input = source_profile(root.path(), profile);
        let output = root.path().join("result");
        let before = common::snapshot_tree(root.path());
        let error = converter()
            .convert(clear_request(
                &input,
                &output,
                &InputKeys::default(),
                &HlsInputKeys::default(),
            ))
            .unwrap_err();
        assert_eq!(error.0, expected, "{profile}");
        assert!(!output.exists());
        assert_eq!(common::snapshot_tree(root.path()), before);
    }
}
#[test]
fn segment_durations_and_output_profile_are_not_silently_normalized() {
    let root = common::private_tempdir().unwrap();
    let input = source(root.path());
    let original = std::fs::read_to_string(&input).unwrap();
    let output = root.path().join("result");
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    std::fs::write(
        &input,
        original.replacen("#EXTINF:2.000000,", "#EXTINF:1.000000,", 1),
    )
    .unwrap();
    assert_eq!(
        converter()
            .convert(clear_request(&input, &output, &keys, &hls))
            .unwrap_err()
            .0,
        "ts-segment-duration-mismatch"
    );
    assert!(!output.exists());
    std::fs::write(&input, original).unwrap();
    let mut request = clear_request(&input, &output, &keys, &hls);
    request.format = OutputFormat::Mp4;
    assert_eq!(
        converter().convert(request).unwrap_err().0,
        "ts-output-profile-not-supported"
    );
    assert!(!output.exists());
    let mut request = clear_request(&input, &output, &keys, &hls);
    request.target = TargetProtection::Cenc;
    assert_eq!(
        converter().convert(request).unwrap_err().0,
        "ts-output-profile-not-supported"
    );
    assert!(!output.exists());
}
fn encrypt_rotating_transport(root: &Path, input: &Path) {
    use drmr_core::conversion::{SegmentIv, encrypt_segment};
    let original = std::fs::read_to_string(input).unwrap();
    let mut text = String::new();
    let mut ordinal = 0_u64;
    let uri_a = "https://operator.invalid/private-ts-a";
    let uri_b = "https://operator.invalid/private-ts-b";
    let other = "202122232425262728292a2b2c2d2e2f";
    for line in original.lines() {
        if !line.starts_with('#') && !line.is_empty() {
            if ordinal == 2 {
                text.push_str("#EXT-X-KEY:METHOD=NONE\n");
            } else {
                let (uri, key) = if ordinal < 2 {
                    (uri_a, KEY)
                } else {
                    (uri_b, other)
                };
                let iv = if ordinal == 4 {
                    SegmentIv::from_hex("303132333435363738393a3b3c3d3e3f").unwrap()
                } else {
                    SegmentIv::for_sequence(7 + ordinal)
                };
                let cipher = root.join(format!("cipher-{ordinal}"));
                encrypt_segment(
                    &root.join(line),
                    &cipher,
                    &Aes128Key::from_hex(key).unwrap(),
                    &iv,
                )
                .unwrap();
                std::fs::rename(cipher, root.join(line)).unwrap();
                {
                    use std::fmt::Write as _;
                    write!(text, "#EXT-X-KEY:METHOD=AES-128,URI=\"{uri}\"").unwrap();
                }
                if ordinal == 4 {
                    text.push_str(",IV=0x303132333435363738393a3b3c3d3e3f");
                }
                text.push('\n');
            }
            ordinal += 1;
        }
        text.push_str(line);
        text.push('\n');
    }
    assert_eq!(ordinal, 5);
    std::fs::write(input, text).unwrap();
}
#[test]
fn aes_transport_rotation_implicit_and_explicit_vectors_and_none_keep_original_media() {
    let root = common::private_tempdir().unwrap();
    let input = source(root.path());
    encrypt_rotating_transport(root.path(), &input);
    let uri_a = "https://operator.invalid/private-ts-a";
    let uri_b = "https://operator.invalid/private-ts-b";
    let other = "202122232425262728292a2b2c2d2e2f";
    let output = root.path().join("result");
    let keys = InputKeys::default();
    let hls = HlsInputKeys::new(vec![
        (uri_a.into(), Aes128Key::from_hex(KEY).unwrap()),
        (uri_b.into(), Aes128Key::from_hex(other).unwrap()),
    ])
    .unwrap();
    let sink = Arc::new(drmr_core::events::Collector::default());
    let c = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), sink.clone())),
        "packager",
        "ffmpeg",
    );
    assert!(
        c.convert(clear_request(&input, &output, &keys, &hls))
            .unwrap()
            .verification
            .passed
    );
    assert!(
        c.verify(VerificationRequest {
            source: VerificationSource {
                input: &input,
                input_keys: &keys,
                hls_keys: &hls
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &keys,
                hls_keys: &HlsInputKeys::default()
            }
        })
        .unwrap()
        .passed
    );
    let records = serde_json::to_string(&sink.records()).unwrap();
    for secret in [KEY, other, uri_a, uri_b, "303132333435363738393a3b3c3d3e3f"] {
        assert!(!records.contains(secret));
    }
    let denied = root.path().join("missing-keys");
    assert_eq!(
        c.convert(clear_request(
            &input,
            &denied,
            &keys,
            &HlsInputKeys::default()
        ))
        .unwrap_err()
        .0,
        "hls-input-key-required"
    );
    assert!(!denied.exists());
}
fn packet_pid(packet: &[u8]) -> u16 {
    (u16::from(packet[1] & 31) << 8) | u16::from(packet[2])
}
fn section_at(packet: &[u8]) -> usize {
    let payload = if packet[3] & 0x20 != 0 {
        5 + usize::from(packet[4])
    } else {
        4
    };
    payload + 1 + usize::from(packet[payload])
}
fn section_crc(data: &[u8]) -> u32 {
    let mut c = u32::MAX;
    for &b in data {
        c ^= u32::from(b) << 24;
        for _ in 0..8 {
            c = (c << 1) ^ if c & 0x8000_0000 != 0 { 0x04c1_1db7 } else { 0 };
        }
    }
    c
}
fn rewrite_section(packet: &mut [u8], mut change: impl FnMut(&mut Vec<u8>)) {
    let at = section_at(packet);
    let n = 3 + ((usize::from(packet[at + 1] & 15) << 8) | usize::from(packet[at + 2]));
    let mut section = packet[at..at + n - 4].to_vec();
    change(&mut section);
    let n = section.len() + 1;
    section[1] = (section[1] & 0xf0) | u8::try_from(n >> 8).unwrap();
    section[2] = u8::try_from(n & 255).unwrap();
    let crc = section_crc(&section);
    section.extend(crc.to_be_bytes());
    packet[at..].fill(255);
    packet[at..at + section.len()].copy_from_slice(&section);
}
fn packet_index(bytes: &[u8], pid: u16) -> usize {
    bytes
        .as_chunks::<188>()
        .0
        .iter()
        .position(|p| packet_pid(p) == pid)
        .unwrap()
}
fn corrupt_transport(root: &std::path::Path, case: &str) {
    let path = root.join("seg-07.ts");
    let mut bytes = std::fs::read(&path).unwrap();
    let pat = packet_index(&bytes, 0);
    let pmt = packet_index(&bytes, 4096);
    let video = packet_index(&bytes, 256);
    match case {
        "sync" => bytes[0] = 0,
        "truncated" => {
            bytes.pop();
        }
        "tei" => bytes[1] |= 0x80,
        "scrambling" => bytes[3] |= 0x80,
        "crc" => {
            let packet = &mut bytes[pat * 188..(pat + 1) * 188];
            let at = section_at(packet);
            packet[at + 8] ^= 1;
        }
        "continuity" => {
            let next = bytes
                .as_chunks::<188>()
                .0
                .iter()
                .enumerate()
                .find(|(i, p)| *i > video && packet_pid(*p) == 256)
                .unwrap()
                .0;
            bytes[next * 188 + 3] = (bytes[next * 188 + 3] & 0xf0) | (bytes[video * 188 + 3] & 15);
        }
        "discontinuity" => {
            assert_ne!(bytes[video * 188 + 3] & 0x20, 0);
            bytes[video * 188 + 5] |= 0x80;
        }
        "unknown-pid" => {
            bytes[video * 188 + 1] = (bytes[video * 188 + 1] & 0xe0) | 1;
            bytes[video * 188 + 2] = 2;
        }
        "unknown-stream" => {
            rewrite_section(&mut bytes[pmt * 188..(pmt + 1) * 188], |s| s[12] = 0x15);
        }
        "multi-program" => rewrite_section(&mut bytes[pat * 188..(pat + 1) * 188], |s| {
            s.extend([0, 2, 0xf0, 1]);
        }),
        "changed-pmt" => {
            let next = root.join("seg-08.ts");
            let mut second = std::fs::read(&next).unwrap();
            let at = second
                .as_chunks::<188>()
                .0
                .iter()
                .position(|p| packet_pid(p) == 4096)
                .unwrap();
            rewrite_section(&mut second[at * 188..(at + 1) * 188], |s| s[5] ^= 2);
            std::fs::write(next, second).unwrap();
        }
        "clock-wrap" => {
            let next = bytes
                .as_chunks::<188>()
                .0
                .iter()
                .enumerate()
                .find(|(i, p)| *i > video && packet_pid(*p) == 256 && p[1] & 0x40 != 0)
                .unwrap()
                .0;
            let packet = &mut bytes[next * 188..(next + 1) * 188];
            let payload = if packet[3] & 0x20 != 0 {
                5 + usize::from(packet[4])
            } else {
                4
            };
            packet[payload + 9..payload + 14].copy_from_slice(&[0x21, 0, 1, 0, 1]);
        }
        "pes-length" => {
            let audio = bytes
                .as_chunks::<188>()
                .0
                .iter()
                .position(|p| packet_pid(p) == 257 && p[1] & 0x40 != 0)
                .unwrap();
            let packet = &mut bytes[audio * 188..(audio + 1) * 188];
            let at = if packet[3] & 0x20 != 0 {
                5 + usize::from(packet[4])
            } else {
                4
            };
            packet[at + 4] = 0;
            packet[at + 5] = 1;
        }
        _ => unreachable!(),
    }
    std::fs::write(path, bytes).unwrap();
}
#[test]
fn malformed_transport_and_program_changes_fail_without_discarding_input_data() {
    let original = common::private_tempdir().unwrap();
    source(original.path());
    let before = common::snapshot_tree(original.path());
    for case in [
        "sync",
        "truncated",
        "tei",
        "scrambling",
        "crc",
        "continuity",
        "discontinuity",
        "unknown-pid",
        "unknown-stream",
        "multi-program",
        "changed-pmt",
        "clock-wrap",
        "pes-length",
    ] {
        let root = common::private_tempdir().unwrap();
        for (name, bytes) in &before {
            std::fs::write(root.path().join(name), bytes).unwrap();
        }
        corrupt_transport(root.path(), case);
        let snapshot = common::snapshot_tree(root.path());
        let output = root.path().join("result");
        let result = converter().convert(clear_request(
            &root.path().join("media.m3u8"),
            &output,
            &InputKeys::default(),
            &HlsInputKeys::default(),
        ));
        assert!(result.is_err(), "accepted {case}");
        assert!(!output.exists(), "published {case}");
        assert_eq!(
            common::snapshot_tree(root.path()),
            snapshot,
            "changed {case}"
        );
    }
}

#[test]
fn impossible_resource_budget_refuses_transport_before_publishing_or_changing_source() {
    let root = common::private_tempdir().unwrap();
    let input = source(root.path());
    let before = common::snapshot_tree(root.path());
    let output = root.path().join("result");
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
        .with_limiter(Arc::new(drmr_core::resources::Limiter::new(1, 1)));
    let c = Converter::new(Arc::new(runner), "packager", "ffmpeg");
    assert_eq!(
        c.convert(clear_request(
            &input,
            &output,
            &InputKeys::default(),
            &HlsInputKeys::default()
        ))
        .unwrap_err()
        .0,
        "resource-budget"
    );
    assert!(!output.exists());
    assert_eq!(common::snapshot_tree(root.path()), before);
}
fn exclusive_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.share_mode(0);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    file.try_lock().map_err(std::io::Error::other)?;
    Ok(file)
}
struct FailureCleanup(Option<Box<dyn FnOnce()>>);
impl Drop for FailureCleanup {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup();
        }
    }
}
struct CancelOnDrop(drmr_core::concurrency::Stop);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.set();
    }
}
#[test]
fn cancellation_after_transport_admission_reclaims_the_actual_locked_media_child() {
    use std::time::{Duration, Instant};
    let root = common::private_tempdir().unwrap();
    let input = source(root.path());
    let original = common::snapshot_tree(root.path());
    let pid = root.path().join("descendant.pid");
    let lock = root.path().join("descendant.lock");
    let output = root.path().join("result");
    let tool = drmr_test_support::install(
        &root.path().join("probe"),
        &drmr_test_support::ToolSpec::default()
            .with("descendant_pid_file", pid.to_str().unwrap())
            .with("descendant_lock_file", lock.to_str().unwrap())
            .with(
                "release_file",
                root.path().join("never-release").to_str().unwrap(),
            ),
    )
    .unwrap();
    let stop = drmr_core::concurrency::Stop::new();
    let c = converter()
        .with_stop(stop.clone())
        .with_timeout(Duration::from_secs(15))
        .with_ffprobe(tool.to_str().unwrap());
    std::thread::scope(|scope| {
        let _cancel = CancelOnDrop(stop.clone());
        let worker = scope.spawn(|| {
            c.convert(clear_request(
                &input,
                &output,
                &InputKeys::default(),
                &HlsInputKeys::default(),
            ))
        });
        let end = Instant::now() + Duration::from_secs(10);
        while !std::fs::read_to_string(&pid)
            .is_ok_and(|s| s.ends_with('\n') && s.trim().parse::<u32>().is_ok())
        {
            assert!(
                !worker.is_finished(),
                "converter exited before fixture readiness"
            );
            assert!(Instant::now() < end, "media child did not become ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            exclusive_lock(&lock).is_err(),
            "fixture must hold an actual exclusive lock"
        );
        let descendant = std::fs::read_to_string(&pid)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        #[cfg(unix)]
        let mut failure_cleanup = {
            let process =
                rustix::process::Pid::from_raw(i32::try_from(descendant).unwrap()).unwrap();
            let group = rustix::process::getpgid(Some(process)).unwrap();
            assert_ne!(group, rustix::process::getpgrp());
            FailureCleanup(Some(Box::new(move || {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            })))
        };
        #[cfg(windows)]
        let mut failure_cleanup = {
            let process = winsafe::HPROCESS::OpenProcess(
                winsafe::co::PROCESS::SYNCHRONIZE | winsafe::co::PROCESS::TERMINATE,
                false,
                descendant,
            )
            .unwrap();
            FailureCleanup(Some(Box::new(move || {
                let _ = process.TerminateProcess(1);
                let _ = process.WaitForSingleObject(Some(5000));
            })))
        };
        #[cfg(windows)]
        let process =
            winsafe::HPROCESS::OpenProcess(winsafe::co::PROCESS::SYNCHRONIZE, false, descendant)
                .unwrap();
        stop.set();
        let result = worker.join().unwrap();
        #[cfg(windows)]
        assert_eq!(
            process.WaitForSingleObject(Some(0)),
            Ok(winsafe::co::WAIT::OBJECT_0),
            "return preceded kernel process completion"
        );
        exclusive_lock(&lock).unwrap_or_else(|error| {
            panic!("return preceded actual descendant lock release: {error:?}; result={result:?}")
        });
        failure_cleanup.0 = None;
        assert_eq!(result.unwrap_err().0, "cancelled");
        assert!(!output.exists());
    });
    for (name, bytes) in original {
        assert_eq!(std::fs::read(root.path().join(name)).unwrap(), bytes);
    }
}
#[test]
fn unsupported_transport_playlist_metadata_and_escaping_uris_fail_closed() {
    let root = common::private_tempdir().unwrap();
    let input = source(root.path());
    let text = std::fs::read_to_string(&input).unwrap();
    let output = root.path().join("result");
    for (prefix, resource) in [
        ("#EXT-X-DISCONTINUITY\n", "seg-07.ts"),
        ("#EXT-X-BYTERANGE:188@0\n", "seg-07.ts"),
        ("#EXT-X-GAP\n", "seg-07.ts"),
        (
            "#EXT-X-PROGRAM-DATE-TIME:2026-10-18T00:00:00Z\n",
            "seg-07.ts",
        ),
        (
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"https://operator.invalid/private\"\n",
            "seg-07.ts",
        ),
        ("", "../escaped.ts"),
        ("", "https://operator.invalid/remote.ts"),
        ("", "seg-07.ts:stream"),
        ("", "%2e%2e/escaped.ts"),
    ] {
        let changed = text
            .replacen("#EXTINF:", &format!("{prefix}#EXTINF:"), 1)
            .replacen("seg-07.ts", resource, 1);
        std::fs::write(&input, changed).unwrap();
        assert!(
            converter()
                .convert(clear_request(
                    &input,
                    &output,
                    &InputKeys::default(),
                    &HlsInputKeys::default()
                ))
                .is_err()
        );
        assert!(!output.exists());
    }
}

#[test]
fn dash_b_frames_preserve_absolute_decode_and_presentation_clocks() {
    let root = common::private_tempdir().unwrap();
    let input = source_profile(root.path(), "bframes");
    let before = common::snapshot_tree(root.path());
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    let output_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
    let c = converter();
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let output = root.path().join(target.as_str());
        let mut request = clear_request(&input, &output, &empty, &hls);
        request.format = OutputFormat::Dash;
        request.target = target;
        request.output_key = (target != TargetProtection::Clear).then_some(&key);
        let result = c
            .convert(request)
            .unwrap_or_else(|e| panic!("{}: {e}", target.as_str()));
        assert!(result.verification.passed);
        let proof = c
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &output.join("manifest.mpd"),
                    input_keys: &output_keys,
                    hls_keys: &hls,
                },
            })
            .unwrap_or_else(|e| panic!("fresh {}: {e}", target.as_str()));
        assert!(proof.passed && proof.media_verified);
        assert_eq!(
            proof.checks.stream_metadata,
            drmr_core::conversion::CheckOutcome::Pass
        );
        assert!(
            proof.checks.packet_payloads_equal
                && proof.checks.packet_timing_equal
                && proof.checks.decoded_content_equal
        );
        assert!(!proof.license_playback_verified);
    }
    for (name, bytes) in before {
        assert_eq!(std::fs::read(root.path().join(name)).unwrap(), bytes);
    }
}
