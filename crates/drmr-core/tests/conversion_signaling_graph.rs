//! Signaling checks follow resource contents rather than filename extensions.
use drmr_core::{
    concurrency::Stop,
    conversion::{Converter, FrozenSource, key_file::OperatorKeys},
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{path::Path, sync::Arc};

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
    for name in ["movie.mp4", "movie.bin", "movie"] {
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
    let master = owner.path().join("master.txt");
    let child = owner.path().join("child");
    let init_path = owner.path().join("init.bin");
    std::fs::write(&master, "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nchild\n").unwrap();
    std::fs::write(&init_path, init(false)).unwrap();
    std::fs::write(owner.path().join("media.bin"), b"\x00\x00\x00\x08mdat").unwrap();
    let clear = "#EXTM3U\n#EXT-X-MAP:URI=\"init.bin\"\n#EXTINF:2,\nmedia.bin\n#EXT-X-ENDLIST\n";
    std::fs::write(&child, clear).unwrap();
    check(&master, OutputFormat::Hls).unwrap();
    std::fs::write(
        &child,
        clear.replace("#EXTINF:", "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://synthetic\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:"),
    )
    .unwrap();
    assert!(check(&master, OutputFormat::Hls).is_err(), "aliased child");
    std::fs::write(&child, clear).unwrap();
    std::fs::write(&init_path, init(true)).unwrap();
    assert!(
        check(&master, OutputFormat::Hls).is_err(),
        "aliased initialization"
    );
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
    std::fs::write(&segment, [0x9a; 32]).unwrap();
    let encrypted = clear.replace(
        "#EXTINF:",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"synthetic-key\"\n#EXTINF:",
    );
    std::fs::write(&playlist, encrypted).unwrap();
    check(&playlist, OutputFormat::HlsTs).unwrap();
    std::fs::write(&segment, init(true)).unwrap();
    assert!(check(&playlist, OutputFormat::HlsTs).is_err());
}
