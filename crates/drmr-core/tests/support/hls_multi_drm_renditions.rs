//! HLS rendition assertions belong to HLS even when decoding uses joined MP4 bytes.
use super::*;

fn transport(root: &Path, rate: Option<u32>) -> PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture("clip.mp4"));
    if let Some(rate) = rate {
        command
            .args([
                "-map", "0:a:0", "-vn", "-c:a", "aac", "-threads", "1", "-ar",
            ])
            .arg(rate.to_string())
            .args(["-ac", "2"]);
    } else {
        command.args([
            "-map",
            "0:v:0",
            "-an",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-preset",
            "fast",
            "-g",
            "48",
            "-keyint_min",
            "48",
            "-sc_threshold",
            "0",
            "-bf",
            "2",
        ]);
    }
    let result = command
        .args([
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(root.join("segment-%03d.ts"))
        .arg(root.join("media.m3u8"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "licensed rendition fixture generation failed"
    );
    root.join("media.m3u8")
}

fn partial_audio_end(playlist: &Path, rate: u32) {
    let original = std::fs::read_to_string(playlist).unwrap();
    let mut lines: Vec<_> = original.lines().map(str::to_owned).collect();
    let at = lines
        .iter()
        .rposition(|line| line.starts_with("#EXTINF:"))
        .unwrap();
    let mut seconds = (240.0 / f64::from(rate)).to_string();
    while seconds.split_once('.').unwrap().1.len() <= 9 {
        seconds.push('0');
    }
    lines[at] = format!("#EXTINF:{seconds},");
    std::fs::write(playlist, format!("{}\n", lines.join("\n"))).unwrap();
}

fn master(root: &Path, rate: u32) -> PathBuf {
    let path = root.join(format!("joint-{rate}.m3u8"));
    std::fs::write(&path, format!("#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"program-audio\",NAME=\"Primary audio\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,URI=\"aac{rate}/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180,AUDIO=\"program-audio\"\nvideo/media.m3u8\n")).unwrap();
    path
}

fn reject_changed_rendition(original: &Path, result: &Path, keys: &key_file::OperatorKeys) {
    let text = std::fs::read_to_string(result).unwrap();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let result_keys = keys.output_verification_keys().unwrap();
    for changed in [
        text.replace("NAME=\"Primary audio\"", "NAME=\"Other audio\""),
        text.replace("LANGUAGE=\"en\"", "LANGUAGE=\"ja\""),
        text.replace("DEFAULT=NO", "DEFAULT=YES"),
        text.replace("AUTOSELECT=YES", "AUTOSELECT=NO"),
        text.replace("AUTOSELECT=YES", "AUTOSELECT=YES,CHANNELS=\"1\""),
    ] {
        assert_ne!(changed, text);
        std::fs::write(result, changed).unwrap();
        let (command, _) = converter();
        let error = command
            .verify(VerificationRequest {
                source: view(original, &empty, &hls),
                output: view(result, &result_keys, &hls),
            })
            .unwrap_err();
        assert_eq!(error.0, "presentation-metadata-mismatch");
    }
    std::fs::write(result, text).unwrap();
}

fn compare_decoded(original: &Path, result: &Path, keys: &key_file::OperatorKeys) {
    let command = Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::new()),
            Arc::new(Collector::default()),
        )),
        "packager",
        "ffmpeg",
    )
    .with_decode(true)
    .with_ffprobe("ffprobe");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let result_keys = keys.output_verification_keys().unwrap();
    let proof = command
        .verify(VerificationRequest {
            source: view(original, &empty, &hls),
            output: view(result, &result_keys, &hls),
        })
        .unwrap();
    assert!(proof.passed && proof.checks.is_pass(), "{proof:?}");
    assert_eq!(
        proof.decode_evidence,
        DecodeEvidence::ContentCompared { equal: true }
    );
}

#[test]
fn joint_transport_renditions_preserve_selection_and_partial_audio_in_both_hls_schemes() {
    let root = common::private_tempdir().unwrap();
    transport(&root.path().join("video"), None);
    for rate in [44_100, 48_000] {
        let audio = transport(&root.path().join(format!("aac{rate}")), Some(rate));
        partial_audio_end(&audio, rate);
        let original = master(root.path(), rate);
        let before = common::snapshot_tree(root.path());
        for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
            let label = format!("joint-{rate}-{target:?}");
            let keys = operator_keys(root.path(), &label, target, OutputFormat::Hls);
            let output = root.path().join(&label);
            let (command, _) = converter();
            let command = command.with_output_signaling(keys.signaling.clone());
            convert(
                &command,
                &original,
                &InputKeys::default(),
                &output,
                OutputFormat::Hls,
                target,
                &keys,
            );
            let result = output.join("master.m3u8");
            fresh(
                &command,
                &original,
                &result,
                &keys,
                target,
                OutputFormat::Hls,
            );
            let master = std::fs::read_to_string(&result).unwrap();
            for assertion in [
                "NAME=\"Primary audio\"",
                "LANGUAGE=\"en\"",
                "DEFAULT=NO",
                "AUTOSELECT=YES",
                "AUDIO=\"program-audio\"",
            ] {
                assert!(
                    master.contains(assertion),
                    "missing rendition assertion: {assertion}"
                );
            }
            if rate == 44_100 && target == TargetProtection::Cenc {
                reject_changed_rendition(&original, &result, &keys);
                compare_decoded(&original, &result, &keys);
            }
        }
        for (name, bytes) in before {
            assert_eq!(std::fs::read(root.path().join(name)).unwrap(), bytes);
        }
    }
}

#[test]
fn single_media_cenc_hls_uses_hls_semantics_without_a_rendition_declaration() {
    let root = common::private_tempdir().unwrap();
    let original = transport(&root.path().join("original"), Some(44_100));
    let before = common::snapshot_tree(original.parent().unwrap());
    let keys = operator_keys(
        root.path(),
        "single",
        TargetProtection::Cenc,
        OutputFormat::Hls,
    );
    let output = root.path().join("result");
    let (command, _) = converter();
    let command = command.with_output_signaling(keys.signaling.clone());
    convert(
        &command,
        &original,
        &InputKeys::default(),
        &output,
        OutputFormat::Hls,
        TargetProtection::Cenc,
        &keys,
    );
    let result = output.join("master.m3u8");
    assert!(
        !std::fs::read_to_string(&result)
            .unwrap()
            .contains("#EXT-X-MEDIA:")
    );
    fresh(
        &command,
        &original,
        &result,
        &keys,
        TargetProtection::Cenc,
        OutputFormat::Hls,
    );
    let media = playlists(&output).remove(0);
    assert_eq!(
        drmr_core::inspection::inspect(&media).unwrap().format,
        drmr_core::inspection::InputFormat::Hls
    );
    fresh(
        &command,
        &original,
        &media,
        &keys,
        TargetProtection::Cenc,
        OutputFormat::Hls,
    );
    #[cfg(unix)]
    {
        let alias = root.path().join("output-alias");
        std::os::unix::fs::symlink(&output, &alias).unwrap();
        let aliased_media = alias.join(media.strip_prefix(&output).unwrap());
        fresh(
            &command,
            &original,
            &aliased_media,
            &keys,
            TargetProtection::Cenc,
            OutputFormat::Hls,
        );
        let input_keys = keys.output_verification_keys().unwrap();
        let hls = HlsInputKeys::default();
        command
            .validate_drm_source(
                view(&aliased_media, &input_keys, &hls),
                &[
                    drmr_core::conversion::DrmSystem::Widevine,
                    drmr_core::conversion::DrmSystem::PlayReady,
                ],
                OutputFormat::Hls,
                TargetProtection::Cenc,
            )
            .unwrap();
    }
    assert_eq!(common::snapshot_tree(original.parent().unwrap()), before);
}
