use drmr_manifest::{
    Document, DocumentId, Format, Options, Presentation, Profile, lint_document, lint_presentation,
};
use std::fmt::Write;
use std::num::NonZeroUsize;

fn corpus() -> Vec<Vec<u8>> {
    let mut inputs: Vec<Vec<u8>> = [
        "",
        "\r\n",
        "\0",
        "é\u{301}",
        "#EXTM3U\r\n",
        "#EXTM3U\n#EXT-X-KEY:METHOD=\"",
        "#EXTM3U\n#EXTINF:1e999999,\nx\n",
        "#EXTM3U\n#EXT-X-PROGRAM-DATE-TIME:99999999999999-01-01T00:00:00Z\n",
        "<!DOCTYPE MPD [<!ENTITY a 'x'>]><MPD>&a;</MPD>",
        "<!-- <x a='b'> -->",
        "<![CDATA[<x>]]>",
        "<MPD a='>\"&amp;\"'></MPD>",
        "<MPD a='unterminated",
        "<MPD>\u{1}</MPD>",
    ]
    .into_iter()
    .map(|value| value.as_bytes().to_vec())
    .collect();
    inputs.extend([
        vec![0xff],
        vec![0xc0, 0x80],
        vec![0xed, 0xa0, 0x80],
        vec![0xf4, 0x90, 0x80, 0x80],
    ]);
    let mut state = 0x35d3_b5b1_u64;
    for length in 1..=256 {
        let bytes = (0..length * 3)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                state.to_le_bytes()[4]
            })
            .collect();
        inputs.push(bytes);
    }
    inputs
}

#[test]
fn arbitrary_bytes_and_malformed_utf8_never_panic_or_depend_on_worker_scheduling() {
    let inputs = corpus();
    for (format, profile) in [
        (Format::Hls, Profile::HlsBaseManifest),
        (Format::Hls, Profile::AppleVodManifest),
        (Format::Dash, Profile::DashStaticManifest),
    ] {
        let mut options = Options::new(profile);
        options.limits.max_diagnostics = 2;
        for bytes in &inputs {
            let document = Document {
                id: DocumentId(0),
                format,
                bytes,
                base_uri: Some("https://private.invalid/secret"),
            };
            let report = std::panic::catch_unwind(|| lint_document(document, &options))
                .expect("input must not panic")
                .unwrap();
            assert!(!format!("{report:?}").contains("private.invalid"));
        }
        let documents: Vec<_> = inputs
            .iter()
            .enumerate()
            .map(|(index, bytes)| Document {
                id: DocumentId(u32::try_from(index).unwrap()),
                format,
                bytes,
                base_uri: None,
            })
            .collect();
        let presentation = Presentation {
            entrypoint: DocumentId(0),
            documents: &documents,
            links: &[],
        };
        options.workers = NonZeroUsize::new(1);
        let mut serial = lint_presentation(presentation, &options).unwrap();
        options.workers = NonZeroUsize::new(8);
        let mut parallel = lint_presentation(presentation, &options).unwrap();
        assert!(parallel.incomplete && parallel.truncated);
        assert!(parallel.workers_used > 1);
        serial.workers_used = 0;
        parallel.workers_used = 0;
        assert_eq!(serial, parallel);
    }
}

#[test]
fn large_xml_attribute_and_namespace_attacks_stop_at_lexical_admission() {
    for prefix in ["a", "xmlns:n"] {
        let mut attributes = String::new();
        for index in 0..50_000 {
            write!(attributes, " {prefix}{index}='x' ").unwrap();
        }
        let bytes = format!("<MPD{attributes}/>");
        let report = lint_document(
            Document {
                id: DocumentId(1),
                format: Format::Dash,
                bytes: bytes.as_bytes(),
                base_uri: None,
            },
            &Options::new(Profile::DashStaticManifest),
        )
        .unwrap();
        assert!(report.incomplete && report.errors == 0);
        assert!(
            !report
                .coverage
                .iter()
                .any(|item| item.rule_id == "DASH-XML-SYNTAX")
        );
    }
    let mut siblings = String::from("<MPD>");
    for index in (0..50_000).rev() {
        write!(siblings, "<r xmlns:n{index}='urn:{index}'/>").unwrap();
    }
    siblings.push_str("</MPD>");
    let report = lint_document(
        Document {
            id: DocumentId(1),
            format: Format::Dash,
            bytes: siblings.as_bytes(),
            base_uri: None,
        },
        &Options::new(Profile::DashStaticManifest),
    )
    .unwrap();
    assert!(report.incomplete && report.errors == 0);
    assert!(
        !report
            .coverage
            .iter()
            .any(|item| item.rule_id == "DASH-XML-SYNTAX")
    );
}
