use drmr_manifest::{
    CheckStatus, Document, DocumentId, Format, ManifestLink, NotCheckedReason, Options,
    Presentation, Profile, Report, lint_presentation,
};

const MEDIA: &[u8] = b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n";
const IFRAME: &[u8] = b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-I-FRAMES-ONLY\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2,\niframe.ts\n#EXT-X-ENDLIST\n";

fn check(regular_attributes: &str, iframe_attributes: &str) -> Report {
    let master = format!(
        "#EXTM3U\n#EXT-X-VERSION:6\n\
         #EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID=\"v\",NAME=\"Camera\",URI=\"main.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Audio\",URI=\"audio.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"s\",NAME=\"Subtitles\",URI=\"text.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"avc1.42c01e\",VIDEO=\"v\"{regular_attributes}\n\
         main.m3u8\n\
         #EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=100000,URI=\"iframes.m3u8\",VIDEO=\"v\"{iframe_attributes}\n"
    );
    let documents = [
        (DocumentId(0), master.as_bytes()),
        (DocumentId(1), MEDIA),
        (DocumentId(2), MEDIA),
        (DocumentId(3), MEDIA),
        (DocumentId(4), IFRAME),
    ]
    .map(|(id, bytes)| Document {
        id,
        format: Format::Hls,
        bytes,
        base_uri: None,
    });
    let links = [
        ("main.m3u8", DocumentId(1)),
        ("audio.m3u8", DocumentId(2)),
        ("text.m3u8", DocumentId(3)),
        ("iframes.m3u8", DocumentId(4)),
    ]
    .map(|(reference, to)| ManifestLink {
        from: DocumentId(0),
        reference,
        to,
    });
    lint_presentation(
        Presentation {
            entrypoint: DocumentId(0),
            documents: &documents,
            links: &links,
        },
        &Options::new(Profile::HlsBaseManifest),
    )
    .unwrap()
}

#[test]
fn hdcp_levels_are_enumerated_and_known_extensions_are_unresolved() {
    for level in ["", ",HDCP-LEVEL=NONE", ",HDCP-LEVEL=TYPE-0"] {
        assert!(check(level, "").is_clean_for_scope());
        assert!(check("", level).is_clean_for_scope());
    }
    for value in ["INVALID", "TYPE-2", "0"] {
        let result = check(&format!(",HDCP-LEVEL={value}"), "");
        assert!(
            !result.is_clean_for_scope(),
            "invalid protection level accepted: errors={}, incomplete={}",
            result.errors,
            result.incomplete
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.rule_id == "HLS-HDCP-LEVEL")
        );
    }
    assert!(!check(",HDCP-LEVEL=\"NONE\"", "").is_clean_for_scope());
    let extension = check(",HDCP-LEVEL=TYPE-1", "");
    assert_eq!(extension.errors, 0);
    assert!(extension.incomplete && !extension.is_clean_for_scope());
    assert!(extension.coverage.iter().any(|coverage| {
        coverage.rule_id == "HLS-HDCP-LEVEL"
            && coverage.status == CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature)
    }));
}

#[test]
fn iframe_declarations_exclude_regular_variant_attributes() {
    let regular =
        ",HDCP-LEVEL=TYPE-0,FRAME-RATE=24,AUDIO=\"a\",SUBTITLES=\"s\",CLOSED-CAPTIONS=NONE";
    for allowed in [
        "",
        ",HDCP-LEVEL=NONE",
        ",HDCP-LEVEL=TYPE-0,AVERAGE-BANDWIDTH=90000,RESOLUTION=640x360",
    ] {
        assert!(check(regular, allowed).is_clean_for_scope());
    }
    for forbidden in [
        ",FRAME-RATE=24",
        ",AUDIO=\"a\"",
        ",SUBTITLES=\"s\"",
        ",CLOSED-CAPTIONS=NONE",
    ] {
        let result = check(regular, forbidden);
        assert!(
            !result.is_clean_for_scope(),
            "excluded iframe attribute accepted: errors={}, incomplete={}",
            result.errors,
            result.incomplete
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.rule_id == "HLS-I-FRAME-ATTRIBUTES")
        );
    }
}

#[test]
fn closed_caption_services_use_the_enumerated_canonical_identifiers() {
    let captions = |identifier: &str| {
        let master = format!(
            "#EXTM3U\n#EXT-X-VERSION:7\n\
             #EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"Captions\",INSTREAM-ID=\"{identifier}\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"avc1.42c01e\",CLOSED-CAPTIONS=\"cc\"\n\
             main.m3u8\n"
        );
        let documents =
            [(DocumentId(0), master.as_bytes()), (DocumentId(1), MEDIA)].map(|(id, bytes)| {
                Document {
                    id,
                    format: Format::Hls,
                    bytes,
                    base_uri: None,
                }
            });
        let links = [ManifestLink {
            from: DocumentId(0),
            reference: "main.m3u8",
            to: DocumentId(1),
        }];
        lint_presentation(
            Presentation {
                entrypoint: DocumentId(0),
                documents: &documents,
                links: &links,
            },
            &Options::new(Profile::HlsBaseManifest),
        )
        .unwrap()
    };
    for identifier in [
        "CC1",
        "CC2",
        "CC3",
        "CC4",
        "SERVICE1",
        "SERVICE9",
        "SERVICE10",
        "SERVICE63",
    ] {
        let report = captions(identifier);
        assert!(
            report.is_clean_for_scope(),
            "canonical caption identifier rejected: {identifier}"
        );
    }
    for identifier in [
        "SERVICE01",
        "SERVICE001",
        "SERVICE063",
        "SERVICE0",
        "SERVICE64",
        "SERVICE+1",
        "SERVICE1x",
        "service1",
        "CC01",
    ] {
        let report = captions(identifier);
        assert!(
            !report.is_clean_for_scope(),
            "noncanonical caption identifier accepted: {identifier}"
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.rule_id == "HLS-RENDITION")
        );
    }
}
