//! Source admission, provider subsets and optional decode remain independent checks.
use super::*;

fn progressive_keys() -> InputKeys {
    InputKeys::new(vec![
        conversion_key(KID, KEY),
        conversion_key(
            "303132333435363738393a3b3c3d3e3f",
            "202122232425262728292a2b2c2d2e2f",
        ),
    ])
    .unwrap()
}
fn conversion_key(kid: &str, key: &str) -> drmr_core::conversion::ContentKey {
    drmr_core::conversion::ContentKey::from_hex(kid, key).unwrap()
}
fn fragmented_video(dash: &Path, output: &Path) {
    let initialization = common::snapshot_tree(dash)
        .into_iter()
        .find_map(|(name, _)| {
            let path = dash.join(name);
            (path.file_name().is_some_and(|name| name == "init.mp4")
                && drmr_core::inspection::inspect(&path).unwrap().tracks[0].kind
                    == drmr_core::inspection::TrackKind::Video)
                .then_some(path)
        })
        .unwrap();
    let mut resources: Vec<_> = std::fs::read_dir(initialization.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("m4s"))
        })
        .collect();
    resources.sort_by_key(|path| {
        path.file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("seg_")
            .unwrap()
            .parse::<u64>()
            .unwrap()
    });
    let mut file = drmr_core::native::create_private_new(output).unwrap();
    for path in std::iter::once(initialization).chain(resources) {
        file.write_all(&std::fs::read(path).unwrap()).unwrap();
    }
}
fn encrypted_video(
    command: &Converter,
    video: &Path,
    root: &Path,
    protection: TargetProtection,
) -> (PathBuf, InputKeys) {
    let output = root.join(format!("video-{protection:?}.mp4"));
    let mut raw = key_file::OperatorKeys::default();
    raw.output = Some(drmr_core::conversion::OutputKey::from_hex(KID, KEY).unwrap());
    convert(
        command,
        video,
        &InputKeys::default(),
        &output,
        OutputFormat::Mp4,
        protection,
        &raw,
    );
    (output, raw.output_verification_keys().unwrap())
}
#[test]
fn qualified_fragmented_video_and_progressive_finite_audio_mp4_accept_explicit_drm() {
    let root = common::private_tempdir().unwrap();
    let original = source(&root.path().join("original"));
    let (command, _) = converter();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let dash = root.path().join("reference-dash");
    let bare = key_file::OperatorKeys::default();
    convert(
        &command,
        &original,
        &empty,
        &dash,
        OutputFormat::Dash,
        TargetProtection::Clear,
        &bare,
    );
    let video = root.path().join("video.mp4");
    fragmented_video(&dash, &video);
    let mut sources = vec![
        (
            "fragmented-clear",
            video.clone(),
            InputKeys::default(),
            video.clone(),
        ),
        (
            "progressive-clear",
            fixture("clip.mp4"),
            InputKeys::default(),
            fixture("clip.mp4"),
        ),
        (
            "progressive-cenc",
            fixture("cenc-iv8.mp4"),
            progressive_keys(),
            fixture("clip.mp4"),
        ),
        (
            "progressive-cbcs",
            fixture("cbcs-joint.mp4"),
            progressive_keys(),
            fixture("clip.mp4"),
        ),
    ];
    for protection in [TargetProtection::Cenc, TargetProtection::Cbcs] {
        let (output, keys) = encrypted_video(&command, &video, root.path(), protection);
        sources.push((
            if protection == TargetProtection::Cenc {
                "fragmented-cenc"
            } else {
                "fragmented-cbcs"
            },
            output,
            keys,
            video.clone(),
        ));
    }
    for (label, input, input_keys, reference) in sources {
        let qualified = command
            .validate_source(
                view(&input, &input_keys, &hls),
                SourceValidationPurpose::Conversion,
            )
            .unwrap();
        assert!(qualified.verification.passed);
        for (format, target) in [
            (OutputFormat::Dash, TargetProtection::Cenc),
            (OutputFormat::Dash, TargetProtection::Cbcs),
            (OutputFormat::Hls, TargetProtection::Cenc),
            (OutputFormat::Hls, TargetProtection::Cbcs),
        ] {
            let name = format!("{label}-{format:?}-{target:?}");
            let keys = operator_keys(root.path(), &name, target, format);
            let output = root.path().join(&name);
            let (command, _) = converter();
            let command = command.with_output_signaling(keys.signaling.clone());
            convert(
                &command,
                &input,
                &input_keys,
                &output,
                format,
                target,
                &keys,
            );
            let result = output.join(if format == OutputFormat::Dash {
                "manifest.mpd"
            } else {
                "master.m3u8"
            });
            fresh(&command, &reference, &result, &keys, target, format);
            println!(
                "HLS_DRM_SOURCE_CELL {}",
                serde_json::json!({"source":label,"format":format,"target":target,"native":true,"fresh":true,"license_playback_verified":false})
            );
        }
    }
}

#[test]
fn hls_and_dash_opaque_initialization_metadata_refuses_before_any_tool() {
    let root = common::private_tempdir().unwrap();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let bare = key_file::OperatorKeys::default();
    let input = fixture("clip.mp4");
    let (seed, _) = converter();
    let keys = operator_keys(
        root.path(),
        "opaque",
        TargetProtection::Cbcs,
        OutputFormat::Hls,
    );
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        let source = root.path().join(format!("source-{format:?}"));
        convert(
            &seed,
            &input,
            &empty,
            &source,
            format,
            TargetProtection::Clear,
            &bare,
        );
        let initialization = common::snapshot_tree(&source)
            .into_iter()
            .find_map(|(name, _)| {
                Path::new(&name)
                    .file_name()
                    .is_some_and(|part| part == "init.mp4")
                    .then(|| source.join(name))
            })
            .unwrap();
        let original = std::fs::read(&initialization).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&initialization)
            .unwrap()
            .write_all(b"\0\0\0\x08skip")
            .unwrap();
        let entrypoint = source.join(if format == OutputFormat::Hls {
            "master.m3u8"
        } else {
            "manifest.mpd"
        });
        for mode in [ProcessingMode::Stitch, ProcessingMode::Segments] {
            let (proof, _) = converter();
            // Native media proof alone deliberately does not authorize opaque controls.
            assert!(
                proof
                    .with_processing(mode)
                    .validate_source(
                        view(&entrypoint, &empty, &hls),
                        SourceValidationPurpose::Conversion
                    )
                    .unwrap()
                    .verification
                    .passed
            );
            let events = Arc::new(Collector::default());
            let command = Converter::new(
                Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
                "must-not-start-packager",
                "must-not-start-decoder",
            )
            .with_processing(mode)
            .with_output_signaling(keys.signaling.clone());
            let output = root.path().join(format!("refused-{format:?}-{mode:?}"));
            let error = command
                .convert(ConversionRequest {
                    input: &entrypoint,
                    output: &output,
                    format: OutputFormat::Hls,
                    target: TargetProtection::Cbcs,
                    input_keys: &empty,
                    hls_input_keys: &hls,
                    output_key: keys.output.as_ref(),
                    hls_key_uri: keys.hls_output_uri(),
                    hls_output_key: None,
                })
                .unwrap_err();
            assert_eq!(
                error.0, "output-signaling-profile-not-supported",
                "{format:?}/{mode:?}"
            );
            assert!(!output.exists());
            assert!(
                !events
                    .records()
                    .iter()
                    .any(|record| record.get("event") == Some(&serde_json::json!("command")))
            );
        }
        assert_eq!(
            std::fs::read(&initialization).unwrap(),
            [original, b"\0\0\0\x08skip".to_vec()].concat()
        );
    }
}

#[test]
fn selected_hls_provider_subsets_bind_only_the_required_fairplay_uri() {
    let root = common::private_tempdir().unwrap();
    let input = fixture("clip.mp4");
    let empty = InputKeys::default();
    for (target, widevine, playready, fairplay) in [
        (TargetProtection::Cenc, true, false, false),
        (TargetProtection::Cenc, false, true, false),
        (TargetProtection::Cbcs, true, false, false),
        (TargetProtection::Cbcs, false, true, false),
        (TargetProtection::Cbcs, true, true, false),
        (TargetProtection::Cbcs, true, false, true),
        (TargetProtection::Cbcs, false, true, true),
    ] {
        let label = format!("subset-{target:?}-{widevine}-{playready}-{fairplay}");
        let mut signaling = serde_json::json!({"system":"multi-drm"});
        if widevine {
            signaling["widevine_pssh"] = pssh(target, KID).into();
        }
        if playready {
            signaling["playready_license_url"] = LICENSE.into();
        }
        let mut output_settings = serde_json::json!({"kid":KID,"key":KEY,"signaling":signaling});
        if fairplay {
            output_settings["signaling"]["fairplay"] = true.into();
            output_settings["hls_key_uri"] = URI.into();
        }
        let path = root.path().join(format!("{label}.keys.json"));
        drmr_core::native::create_private_new(&path)
            .unwrap()
            .write_all(
                serde_json::json!({"schema_version":1,"output":output_settings})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        let keys = key_file::read(&path).unwrap();
        assert_eq!(keys.hls_output_uri().is_some(), fairplay);
        if fairplay {
            output_settings
                .as_object_mut()
                .unwrap()
                .remove("hls_key_uri");
            let missing = root.path().join(format!("{label}-missing-uri.keys.json"));
            drmr_core::native::create_private_new(&missing)
                .unwrap()
                .write_all(
                    serde_json::json!({"schema_version":1,"output":output_settings})
                        .to_string()
                        .as_bytes(),
                )
                .unwrap();
            assert_eq!(key_file::read(&missing).unwrap_err().0, "key-file-invalid");
        }
        let (command, _) = converter();
        let command = command.with_output_signaling(keys.signaling.clone());
        let output = root.path().join(&label);
        convert(
            &command,
            &input,
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
            assert_eq!(
                text.contains("urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"),
                widevine
            );
            assert_eq!(text.contains("com.microsoft.playready"), playready);
            assert_eq!(text.contains("com.apple.streamingkeydelivery"), fairplay);
        }
    }
}

#[test]
fn optional_complete_decode_compares_cenc_two_and_cbcs_three_provider_hls() {
    let root = common::private_tempdir().unwrap();
    let input = fixture("clip.mp4");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
        let label = format!("decoded-{target:?}");
        let keys = operator_keys(root.path(), &label, target, OutputFormat::Hls);
        let (command, _) = converter();
        let command = command.with_output_signaling(keys.signaling.clone());
        let output = root.path().join(&label);
        convert(
            &command,
            &input,
            &empty,
            &output,
            OutputFormat::Hls,
            target,
            &keys,
        );
        let decoded = Converter::new(
            Arc::new(ToolRunner::new(
                Arc::new(Secrets::new()),
                Arc::new(Collector::default()),
            )),
            "packager",
            "ffmpeg",
        )
        .with_ffprobe("ffprobe")
        .with_decode(true);
        let encrypted_keys = keys.output_verification_keys().unwrap();
        let compared = decoded
            .verify(VerificationRequest {
                source: view(&input, &empty, &hls),
                output: view(&output.join("master.m3u8"), &encrypted_keys, &hls),
            })
            .unwrap();
        assert!(
            compared.passed && compared.checks.is_pass(),
            "{target:?}: {compared:?}"
        );
        assert_eq!(
            compared.decode_evidence,
            DecodeEvidence::ContentCompared { equal: true }
        );
        assert!(!compared.license_playback_verified);
    }
}
