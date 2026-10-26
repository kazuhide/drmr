//! Fractional AAC resources fit the original integer HLS target without edits.
use super::*;

#[test]
fn original_hls_target_survives_segment_conversion_with_fractional_aac_durations() {
    for rate in [44_100_u32, 48_000] {
        let mut fixture = fixture(rate);
        fixture.parts.retain(|(init, _, _)| {
            crate::inspection::inspect(init).unwrap().tracks[0].kind == TrackKind::Audio
        });
        assert_eq!(fixture.parts.len(), 1);
        let (_, resources, complete) = &mut fixture.parts[0];
        let (_, fragments) =
            crate::mp4::split_fragmented(&std::fs::read(complete).unwrap()).unwrap();
        let cut = usize::try_from(6 * rate / 1024 + 1).unwrap();
        assert!(fragments.len() > cut);
        resources.clear();
        for (index, range) in [0..cut, cut..fragments.len()].into_iter().enumerate() {
            let path = fixture
                .directory
                .path()
                .join(format!("fractional-{index}.m4s"));
            std::fs::write(&path, fragments[range].concat()).unwrap();
            resources.push(path);
        }
        let input = manifest_fixture(
            &fixture,
            TargetProtection::Clear,
            OutputFormat::Hls,
            "source",
        );
        let media = input.parent().unwrap().join("track-0000/media.m3u8");
        let original = std::fs::read_to_string(&media).unwrap();
        assert!(original.contains("#EXT-X-TARGETDURATION:7\n"));
        let durations = original
            .lines()
            .filter_map(|line| line.strip_prefix("#EXTINF:"))
            .map(str::to_string)
            .collect::<Vec<_>>();
        let first = durations[0]
            .split(',')
            .next()
            .unwrap()
            .parse::<f64>()
            .unwrap();
        assert!(first > 6.0 && first < 6.5);
        // The generated fixture's precise durations fit six after RFC rounding.
        // Only its header is changed; the native proof below admits real media.
        std::fs::write(
            &media,
            original.replace("#EXT-X-TARGETDURATION:7\n", "#EXT-X-TARGETDURATION:6\n"),
        )
        .unwrap();
        let before = std::fs::read(&media).unwrap();
        let output = fixture.directory.path().join("converted");
        let (command, events) = converter();
        let command = command.with_processing(ProcessingMode::Segments);
        let empty = InputKeys::default();
        let hls = HlsInputKeys::default();
        let report = command
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::Hls,
                target: TargetProtection::Clear,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap();
        assert!(report.verification.passed);
        let result = std::fs::read_to_string(output.join("track-0000/media.m3u8")).unwrap();
        assert!(result.contains("#EXT-X-TARGETDURATION:6\n"));
        assert_eq!(
            result
                .lines()
                .filter_map(|line| line.strip_prefix("#EXTINF:"))
                .collect::<Vec<_>>(),
            durations.iter().map(String::as_str).collect::<Vec<_>>()
        );
        let verified = command
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &output.join("master.m3u8"),
                    input_keys: &empty,
                    hls_keys: &hls,
                },
            })
            .unwrap();
        assert!(verified.passed);
        assert_eq!(std::fs::read(&media).unwrap(), before);
        assert_no_tools(&events);
    }
}
