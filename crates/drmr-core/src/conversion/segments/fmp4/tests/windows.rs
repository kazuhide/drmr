use super::*;

fn finite_initialization(bytes: &[u8], duration: u32) -> Vec<u8> {
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        [
            u32::try_from(payload.len() + 8)
                .unwrap()
                .to_be_bytes()
                .as_slice(),
            &kind,
            payload,
        ]
        .concat()
    }
    fn rebuild(node: &crate::mp4::Mp4Box, bytes: &[u8], edits: &[u8]) -> Vec<u8> {
        if !matches!(&node.kind(), b"moov" | b"trak") {
            return node.header.bytes(bytes).to_vec();
        }
        let mut payload = node
            .children
            .iter()
            .filter(|node| node.kind() != *b"edts")
            .flat_map(|node| rebuild(node, bytes, edits))
            .collect::<Vec<_>>();
        if node.kind() == *b"trak" {
            payload.extend(edits);
        }
        boxed(node.kind(), &payload)
    }
    let mut bytes = bytes.to_vec();
    let nodes = crate::mp4::parse_boxes(&bytes).unwrap();
    let mvhd = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"mvhd")
        .unwrap();
    let at = mvhd.header.offset + mvhd.header.header_size + 12;
    bytes[at..at + 4].copy_from_slice(&48_000_u32.to_be_bytes());
    let mut payload = vec![0; 4];
    payload.extend(1_u32.to_be_bytes());
    payload.extend(duration.to_be_bytes());
    payload.extend(0_i32.to_be_bytes());
    payload.extend(1_u16.to_be_bytes());
    payload.extend(0_u16.to_be_bytes());
    let edits = boxed(*b"edts", &boxed(*b"elst", &payload));
    crate::mp4::parse_boxes(&bytes)
        .unwrap()
        .iter()
        .flat_map(|node| rebuild(node, &bytes, &edits))
        .collect()
}

#[test]
fn finite_audio_windows_keep_exact_one_and_1023_sample_selections() {
    let mut fixture = fixture(48_000);
    fixture.parts.remove(0);
    let initial = std::fs::read(&fixture.parts[0].0).unwrap();
    let coded = fixture.directory.path().join("all-coded-audio.m4s");
    std::fs::write(
        &coded,
        fixture.parts[0]
            .1
            .iter()
            .flat_map(|path| std::fs::read(path).unwrap())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    fixture.parts[0].1 = vec![coded];
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    for duration in [1, 1023] {
        std::fs::write(
            &fixture.parts[0].0,
            finite_initialization(&initial, duration),
        )
        .unwrap();
        let input = manifest_fixture(
            &fixture,
            TargetProtection::Clear,
            OutputFormat::Dash,
            &format!("finite-{duration}"),
        );
        for format in [OutputFormat::Dash, OutputFormat::Hls] {
            let output = fixture
                .directory
                .path()
                .join(format!("finite-{duration}-{}", format.as_str()));
            let (converter, events) = converter();
            let qualified = qualify_dash(
                &converter,
                VerificationSource {
                    input: &input,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                SourceValidationPurpose::Conversion,
            )
            .unwrap();
            assert!(qualified.verification.passed);
            let request = ConversionRequest {
                input: &input,
                output: &output,
                format,
                target: TargetProtection::Clear,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            };
            let report = convert_dash(&converter, request).unwrap();
            assert!(report.verification.passed);
            let result = super::super::super::source::entrypoint(&output, format);
            assert!(
                verify(
                    &converter,
                    VerificationRequest {
                        source: VerificationSource {
                            input: &input,
                            input_keys: &empty,
                            hls_keys: &hls
                        },
                        output: VerificationSource {
                            input: &result,
                            input_keys: &empty,
                            hls_keys: &hls
                        }
                    }
                )
                .unwrap()
                .passed
            );
            assert_no_tools(&events);
        }
    }
}
