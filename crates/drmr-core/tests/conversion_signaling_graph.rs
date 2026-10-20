//! Signaling checks follow resource contents rather than filename extensions.
use drmr_core::{
    concurrency::Stop,
    conversion::{
        Aes128Key, ConversionRequest, Converter, FrozenSource, HlsInputKeys, HlsOutputKey,
        InputKeys, key_file::OperatorKeys,
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn check(path: &Path, format: OutputFormat) -> Result<(), drmr_core::conversion::ConversionError> {
    FrozenSource::capture(path, &Stop::new()).unwrap();
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "must-not-run-packager",
        "must-not-run-ffmpeg",
    )
    .verify_output_signaling(
        path,
        format,
        TargetProtection::Cbcs,
        &OperatorKeys::default(),
    )
}

fn init(provider: bool) -> Vec<u8> {
    let mut bytes = b"\x00\x00\x00\x10ftypisom\x00\x00\x00\x00".to_vec();
    if provider {
        bytes.extend(
            drmr_core::playready::build_pssh_box(&drmr_core::playready::WIDEVINE_SYSTEM_ID, &[])
                .unwrap(),
        );
    }
    bytes
}

#[test]
fn empty_selection_checks_aliased_standalone_mp4_contents() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    for name in ["movie.mp4", "movie.bin", "movie", "movie.MP4", "movie.M4S"] {
        let path = owner.path().join(name);
        std::fs::write(&path, init(false)).unwrap();
        check(&path, OutputFormat::Mp4).unwrap();
        std::fs::write(&path, init(true)).unwrap();
        assert!(check(&path, OutputFormat::Mp4).is_err(), "{name}");
    }
}

#[test]
fn empty_selection_checks_aliased_hls_children_and_initializations() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    for (master_name, child_name, init_name) in [
        ("master.txt", "child", "init.bin"),
        ("MASTER.M3U8", "CHILD.M3U8", "INIT.MP4"),
        ("master", "child.txt", "initialization"),
    ] {
        let master = owner.path().join(master_name);
        let child = owner.path().join(child_name);
        let init_path = owner.path().join(init_name);
        std::fs::write(
            &master,
            format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\n{child_name}\n"),
        )
        .unwrap();
        std::fs::write(&init_path, init(false)).unwrap();
        std::fs::write(owner.path().join("media.bin"), b"\x00\x00\x00\x08mdat").unwrap();
        let clear = format!(
            "#EXTM3U\n#EXT-X-MAP:URI=\"{init_name}\"\n#EXTINF:2,\nmedia.bin\n#EXT-X-ENDLIST\n"
        );
        std::fs::write(&child, &clear).unwrap();
        check(&master, OutputFormat::Hls).unwrap();
        std::fs::write(
            &child,
            clear.replace("#EXTINF:", "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://synthetic\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:"),
        ).unwrap();
        assert!(
            check(&master, OutputFormat::Hls).is_err(),
            "aliased child: {child_name}"
        );
        std::fs::write(&child, &clear).unwrap();
        std::fs::write(&init_path, init(true)).unwrap();
        assert!(
            check(&master, OutputFormat::Hls).is_err(),
            "aliased initialization: {init_name}"
        );
    }
}

#[test]
fn empty_selection_checks_aliased_dash_manifest_records() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let manifest = owner.path().join("manifest.data");
    std::fs::write(owner.path().join("init.bin"), init(false)).unwrap();
    std::fs::write(owner.path().join("segment-1.bin"), b"\x00\x00\x00\x08mdat").unwrap();
    let clear = "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"><Period><AdaptationSet><SegmentTemplate initialization=\"init.bin\" media=\"segment-$Number$.bin\" timescale=\"1\"><SegmentTimeline><S d=\"2\"/></SegmentTimeline></SegmentTemplate><Representation id=\"v\"/></AdaptationSet></Period></MPD>";
    std::fs::write(&manifest, clear).unwrap();
    check(&manifest, OutputFormat::Dash).unwrap();
    std::fs::write(
        &manifest,
        clear.replace("<Representation", "<ContentProtection schemeIdUri=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\"/><Representation"),
    )
    .unwrap();
    assert!(check(&manifest, OutputFormat::Dash).is_err());
}

#[test]
fn whole_file_encryption_and_transport_resources_do_not_hide_clear_provider_boxes() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let playlist = owner.path().join("playlist");
    let segment = owner.path().join("segment.data");
    let clear = "#EXTM3U\n#EXTINF:2,\nsegment.data\n#EXT-X-ENDLIST\n";
    std::fs::write(&playlist, clear).unwrap();
    let mut transport = vec![0; 3 * 188];
    for packet in transport.chunks_mut(188) {
        packet[0] = 0x47;
    }
    std::fs::write(&segment, transport).unwrap();
    check(&playlist, OutputFormat::HlsTs).unwrap();
    let encrypted = clear.replace(
        "#EXTINF:",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"synthetic-key\"\n#EXTINF:",
    );
    std::fs::write(&playlist, encrypted).unwrap();
    for leading in [0x9a, b'<', b'#', 0x47] {
        let mut cipher = [0x9a; 32];
        cipher[0] = leading;
        std::fs::write(&segment, cipher).unwrap();
        check(&playlist, OutputFormat::HlsTs).unwrap();
    }
    for kind in [b"skip", b"zzzz"] {
        let mut prefix = 8u32.to_be_bytes().to_vec();
        prefix.extend(kind);
        let mut clear = prefix.clone();
        clear.extend(init(false));
        std::fs::write(&segment, clear).unwrap();
        check(&playlist, OutputFormat::HlsTs).unwrap();
        prefix.extend(init(true));
        std::fs::write(&segment, &prefix).unwrap();
        assert!(check(&playlist, OutputFormat::HlsTs).is_err());
        prefix.extend(b"\x00\x00\x00\x07freeTRAILER!");
        std::fs::write(&segment, prefix).unwrap();
        assert!(check(&playlist, OutputFormat::HlsTs).is_err());
    }
    let mut provider = init(true);
    provider.extend(b"\x00\x00\x00\x07freeTRAILER!");
    assert!(provider.len().is_multiple_of(16));
    std::fs::write(&segment, provider).unwrap();
    assert!(check(&playlist, OutputFormat::HlsTs).is_err());
}

fn video_source(root: &Path) -> PathBuf {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let output = root.join("source.mp4");
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let result = runner
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
                output.to_str().unwrap(),
            ]
            .map(str::to_owned),
            "ffmpeg",
            &RunOptions::default(),
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    output
}

#[test]
fn actual_clear_and_aes_hls_outputs_verify_without_drm_on_native_paths() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let source = video_source(owner.path());
    let original = std::fs::read(&source).unwrap();
    let converter = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    );
    let encryption = HlsOutputKey::new(
        "https://example.invalid/key",
        Aes128Key::from_hex("101112131415161718191a1b1c1d1e1f").unwrap(),
    )
    .unwrap();
    for target in [TargetProtection::Clear, TargetProtection::HlsAes128] {
        let output = owner.path().join(target.as_str());
        let report = converter
            .convert(ConversionRequest {
                input: &source,
                output: &output,
                format: OutputFormat::Hls,
                target,
                input_keys: &InputKeys::default(),
                hls_input_keys: &HlsInputKeys::default(),
                output_key: None,
                hls_key_uri: None,
                hls_output_key: (target == TargetProtection::HlsAes128).then_some(&encryption),
            })
            .unwrap();
        assert!(report.verification.passed && report.verification.media_verified);
        assert!(report.verification.checks.packet_payloads_equal);
        assert!(report.verification.checks.packet_timing_equal);
        assert!(report.verification.checks.decoded_content_equal);
        assert_eq!(report.output.drm_systems, Vec::<String>::new());
        // fspath removes Windows' verbatim prefix, exercising an ordinary drive path in CI.
        let input = PathBuf::from(
            drmr_core::manifest_support::path::fspath(&output.join("master.m3u8")).unwrap(),
        );
        #[cfg(windows)]
        assert!(
            matches!(input.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_)))
        );
        converter
            .verify_output_signaling(&input, OutputFormat::Hls, target, &OperatorKeys::default())
            .unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), original);
    }
}
