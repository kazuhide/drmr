use drmr_manifest::{Document, DocumentId, Format, Options, Profile, Severity, lint_document};

fn lint(codec: &str, profile: Profile, iframe: bool) -> drmr_manifest::Report {
    let variant = if iframe {
        format!(
            "#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=100000,AVERAGE-BANDWIDTH=90000,CODECS=\"{codec}\",RESOLUTION=320x180,URI=\"iframe.m3u8\"\n"
        )
    } else {
        format!(
            "#EXT-X-STREAM-INF:BANDWIDTH=100000,AVERAGE-BANDWIDTH=90000,CODECS=\"{codec}\",RESOLUTION=320x180,FRAME-RATE=24\nvideo.m3u8\n"
        )
    };
    let bytes = format!("#EXTM3U\n#EXT-X-VERSION:7\n{variant}");
    lint_document(
        Document {
            id: DocumentId(0),
            format: Format::Hls,
            bytes: bytes.as_bytes(),
            base_uri: None,
        },
        &Options::new(profile),
    )
    .unwrap()
}

#[test]
fn generic_hls_acceptance_does_not_claim_apple_vp9_qualification() {
    for iframe in [false, true] {
        let base = lint("vp09.00.11.08", Profile::HlsBaseManifest, iframe);
        assert_eq!(base.errors, 0);
        assert!(
            base.diagnostics
                .iter()
                .all(|d| d.rule_id != "APPLE-HLS-VIDEO-CODEC")
        );
        let apple = lint("vp09.00.11.08", Profile::AppleVodManifest, iframe);
        let diagnostic = apple
            .diagnostics
            .iter()
            .find(|d| d.rule_id == "APPLE-HLS-VIDEO-CODEC")
            .expect("VP9 must fail the Apple video codec requirement even with RESOLUTION");
        assert_eq!(diagnostic.reference.section, "1.1");
        assert_eq!(diagnostic.severity, Severity::Error);
    }
}

#[test]
fn the_vp9_rule_preserves_existing_apple_codec_and_audio_behavior() {
    for codec in [
        "avc1.64001f",
        "hvc1.1.6.L93.B0",
        "hev1.2.4.L120.B0",
        "av01.0.04M.08",
        "mp4a.40.2",
    ] {
        for iframe in [false, true] {
            let report = lint(codec, Profile::AppleVodManifest, iframe);
            assert!(
                report
                    .diagnostics
                    .iter()
                    .all(|d| d.rule_id != "APPLE-HLS-VIDEO-CODEC")
            );
        }
    }
}
