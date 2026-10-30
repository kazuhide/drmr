//! Session keys describe the same provider values regardless of attribute order.
use super::*;
use std::fmt::Write as _;

fn reversed(value: &str) -> String {
    let attrs = drmr_core::m3u8::parse_attributes(value).unwrap();
    let mut pairs: Vec<_> = attrs.iter().collect();
    pairs.reverse();
    pairs
        .into_iter()
        .map(|(key, value)| {
            if matches!(key, "METHOD" | "IV") {
                format!("{key}={value}")
            } else {
                format!("{key}=\"{value}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn reordered_session_attributes_match_media_keys_but_changed_values_and_duplicates_do_not() {
    let root = common::private_tempdir().unwrap();
    let original = source(&root.path().join("original"));
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
        let label = format!("session-{target:?}");
        let keys = operator_keys(root.path(), &label, target, OutputFormat::Hls);
        let output = root.path().join(&label);
        let (command, _) = converter();
        convert(
            &command.with_output_signaling(keys.signaling.clone()),
            &original,
            &empty,
            &output,
            OutputFormat::Hls,
            target,
            &keys,
        );
        let entrypoint = output.join("master.m3u8");
        let master = std::fs::read_to_string(&entrypoint).unwrap();
        let media = std::fs::read_to_string(playlists(&output).remove(0)).unwrap();
        let declarations: Vec<_> = media
            .lines()
            .filter_map(|line| line.strip_prefix("#EXT-X-KEY:"))
            .collect();
        let mut sessions = String::new();
        for value in &declarations {
            writeln!(sessions, "#EXT-X-SESSION-KEY:{}", reversed(value)).unwrap();
        }
        let session_master = master
            .lines()
            .filter(|line| !line.starts_with("#EXT-X-VERSION:"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let valid = session_master.replacen(
            "#EXTM3U",
            &format!("#EXTM3U\n#EXT-X-VERSION:6\n{sessions}"),
            1,
        );
        std::fs::write(&entrypoint, &valid).unwrap();
        let result_keys = keys.output_verification_keys().unwrap();
        for mode in [ProcessingMode::Stitch, ProcessingMode::Segments] {
            let (fresh, _) = converter();
            let admission = fresh
                .with_processing(mode)
                .validate_source(
                    view(&entrypoint, &result_keys, &hls),
                    SourceValidationPurpose::Conversion,
                )
                .unwrap_or_else(|error| panic!("{target:?}/{mode:?}: {error}"));
            assert!(admission.verification.passed);
        }
        let (checker, _) = converter();
        checker
            .verify_output_signaling(&entrypoint, OutputFormat::Hls, target, &keys)
            .unwrap();
        fresh(
            &checker,
            &original,
            &entrypoint,
            &keys,
            target,
            OutputFormat::Hls,
        );
        if target == TargetProtection::Cbcs {
            equivalent_iv_spelling(&entrypoint, &keys, &valid);
        }
        reject_session_changes(&entrypoint, &valid, declarations[0], &result_keys, target);
        std::fs::write(&entrypoint, master).unwrap();
    }
}

fn reject_session_changes(
    input: &Path,
    valid: &str,
    first: &str,
    result_keys: &InputKeys,
    target: TargetProtection,
) {
    let entrypoint = input;
    let hls = HlsInputKeys::default();
    let attrs = drmr_core::m3u8::parse_attributes(first).unwrap();
    let method = attrs.get("METHOD").unwrap();
    let format = attrs.get("KEYFORMAT").unwrap();
    let other_method = if method == "SAMPLE-AES" {
        "SAMPLE-AES-CTR"
    } else {
        "SAMPLE-AES"
    };
    let mismatches = [
        (
            "uri",
            first.replace(attrs.get("URI").unwrap(), "data:text/plain;base64,AA=="),
            "hls-drm-signaling-invalid",
        ),
        (
            "method",
            first.replace(method, other_method),
            "hls-drm-signaling-invalid",
        ),
        (
            "format",
            first.replace(format, "com.example.unsupported"),
            "hls-drm-signaling-invalid",
        ),
        (
            "version",
            first.replace("KEYFORMATVERSIONS=\"1\"", "KEYFORMATVERSIONS=\"2\""),
            "hls-drm-signaling-invalid",
        ),
        (
            "iv",
            format!("{first},IV=0x00"),
            "hls-drm-signaling-invalid",
        ),
        // Duplicate attributes are malformed in the earlier shared manifest parser.
        (
            "duplicate-attribute",
            format!("{first},METHOD={method}"),
            "input-inspection-failed",
        ),
    ];
    for (name, changed, code) in mismatches {
        let tampered = valid.replacen(
            &format!("#EXT-X-SESSION-KEY:{}", reversed(first)),
            &format!("#EXT-X-SESSION-KEY:{changed}"),
            1,
        );
        assert_ne!(tampered, valid, "{name}");
        std::fs::write(entrypoint, tampered).unwrap();
        let (fresh, _) = converter();
        assert_eq!(
            fresh
                .validate_source(
                    view(entrypoint, result_keys, &hls),
                    SourceValidationPurpose::Conversion
                )
                .unwrap_err()
                .0,
            code,
            "{target:?}/{name}"
        );
    }
    let duplicate = format!("{valid}#EXT-X-SESSION-KEY:{}\n", reversed(first));
    std::fs::write(entrypoint, duplicate).unwrap();
    let (fresh, _) = converter();
    assert_eq!(
        fresh
            .validate_source(
                view(entrypoint, result_keys, &hls),
                SourceValidationPurpose::Conversion
            )
            .unwrap_err()
            .0,
        "hls-drm-signaling-invalid"
    );
}

fn equivalent_iv_spelling(input: &Path, keys: &key_file::OperatorKeys, valid: &str) {
    let root = input.parent().unwrap();
    let playlist = playlists(root).remove(0);
    let media = std::fs::read_to_string(&playlist).unwrap();
    let map = media
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-MAP:"))
        .unwrap();
    let attrs = drmr_core::m3u8::parse_attributes(map).unwrap();
    let initialization = playlist.parent().unwrap().join(attrs.get("URI").unwrap());
    let bytes = std::fs::read(initialization).unwrap();
    let iv = drmr_core::mp4::tenc_constant_iv_hex(&bytes)
        .unwrap()
        .unwrap();
    let canonical = format!("0x{}", iv.expose());
    let equivalent = format!("0X{}", iv.expose().to_ascii_uppercase());
    assert_ne!(canonical, equivalent);
    let mut changed = String::new();
    let mut sessions = String::new();
    for line in media.lines() {
        if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            writeln!(changed, "{line},IV={canonical}").unwrap();
            writeln!(
                sessions,
                "#EXT-X-SESSION-KEY:{},IV={equivalent}",
                reversed(value)
            )
            .unwrap();
        } else {
            writeln!(changed, "{line}").unwrap();
        }
    }
    let relative = playlist
        .strip_prefix(root)
        .unwrap()
        .to_str()
        .unwrap()
        .replace('\\', "/");
    let master = format!(
        "#EXTM3U\n#EXT-X-VERSION:6\n{sessions}#EXT-X-STREAM-INF:BANDWIDTH=1000000\n{relative}\n"
    );
    std::fs::write(&playlist, changed).unwrap();
    std::fs::write(input, &master).unwrap();
    let (checker, _) = converter();
    let input_keys = keys.output_verification_keys().unwrap();
    let hls = HlsInputKeys::default();
    assert!(
        checker
            .validate_source(
                view(input, &input_keys, &hls),
                SourceValidationPurpose::Conversion
            )
            .unwrap()
            .verification
            .passed
    );
    checker
        .verify_output_signaling(input, OutputFormat::Hls, TargetProtection::Cbcs, keys)
        .unwrap();
    std::fs::write(input, master.replace(&equivalent, "0x00")).unwrap();
    assert_eq!(
        checker
            .validate_source(
                view(input, &input_keys, &hls),
                SourceValidationPurpose::Conversion
            )
            .unwrap_err()
            .0,
        "hls-drm-signaling-invalid"
    );
    std::fs::write(&playlist, media).unwrap();
    std::fs::write(input, valid).unwrap();
}
