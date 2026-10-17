//! Read-only inspection and explicit, non-executing conversion plans.

use drmr_core::inspection::{InputFormat, Protection, inspect};

fn atom(kind: &[u8], body: &[u8]) -> Vec<u8> {
    [
        u32::try_from(body.len() + 8)
            .unwrap()
            .to_be_bytes()
            .as_slice(),
        kind,
        body,
    ]
    .concat()
}

fn entry(protection: Option<&[u8; 4]>, malformed: bool) -> Vec<u8> {
    let mut payload = vec![0_u8; 78];
    if let Some(scheme) = protection {
        let schm = atom(b"schm", &[&[0_u8; 4][..], scheme, &[0, 1, 0, 0]].concat());
        let mut tenc = vec![1, 0, 0, 0, 0, 0x19, 1, 0];
        tenc.extend([0x11; 16]);
        tenc.push(16);
        tenc.extend([0x22; 16]);
        if malformed {
            tenc.truncate(5);
        }
        let sinf = atom(
            b"sinf",
            &[
                atom(b"frma", b"avc1"),
                schm,
                atom(b"schi", &atom(b"tenc", &tenc)),
            ]
            .concat(),
        );
        payload.extend(sinf);
    }
    atom(
        if protection.is_some() {
            b"encv"
        } else {
            b"avc1"
        },
        &payload,
    )
}

fn movie(entries: &[Vec<u8>]) -> Vec<u8> {
    let tracks: Vec<_> = entries
        .iter()
        .map(|entry| {
            let stsd = atom(
                b"stsd",
                &[&[0_u8; 4][..], &1_u32.to_be_bytes(), entry].concat(),
            );
            atom(
                b"trak",
                &atom(b"mdia", &atom(b"minf", &atom(b"stbl", &stsd))),
            )
        })
        .collect();
    atom(b"moov", &tracks.concat())
}

#[test]
fn mp4_sample_descriptions_keep_their_own_schemes_keys_and_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data-with-no-extension");
    std::fs::write(
        &path,
        movie(&[
            entry(Some(b"cenc"), false),
            entry(Some(b"cbcs"), false),
            entry(None, false),
        ]),
    )
    .unwrap();
    let report = inspect(&path).unwrap();
    assert_eq!(report.format, InputFormat::Mp4);
    assert_eq!(report.protection, Protection::Mixed);
    assert_eq!(
        report
            .tracks
            .iter()
            .map(|item| item.protection)
            .collect::<Vec<_>>(),
        [Protection::Cenc, Protection::Cbcs, Protection::Clear]
    );
    assert_eq!(report.tracks[1].pattern.unwrap().encrypted_blocks, 1);
    let shown = serde_json::to_string(&report).unwrap();
    assert!(
        !shown.contains(&"22".repeat(16)),
        "IV bytes are never report data"
    );
    assert!(!report.media_verified);
}

#[test]
fn malformed_protection_does_not_turn_into_clear() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.mp4");
    std::fs::write(&path, movie(&[entry(Some(b"cbcs"), true)])).unwrap();
    let report = inspect(&path).unwrap();
    assert_eq!(report.protection, Protection::Unknown);
}

#[test]
fn large_sparse_media_payload_is_skipped_and_metadata_is_found_after_it() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.mp4");
    let mut file = std::fs::File::create(&path).unwrap();
    let size = 512_u32 * 1024 * 1024;
    file.write_all(&[&size.to_be_bytes()[..], b"mdat"].concat())
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(size))).unwrap();
    file.write_all(&movie(&[entry(None, false)])).unwrap();
    drop(file);
    assert_eq!(inspect(&path).unwrap().protection, Protection::Clear);
}

#[test]
fn planning_preserves_key_roles_and_rejects_container_scheme_mismatches() {
    use drmr_core::inspection::{OutputFormat, TargetProtection, plan};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.mp4");
    std::fs::write(&path, movie(&[entry(Some(b"cenc"), false)])).unwrap();
    let result = plan(
        inspect(&path).unwrap(),
        TargetProtection::Cbcs,
        vec![OutputFormat::Dash, OutputFormat::Hls],
    );
    assert!(result.compatible);
    assert!(!result.execution_available);
    assert_eq!(
        result
            .keys
            .iter()
            .map(|key| key.role.as_str())
            .collect::<Vec<_>>(),
        ["input", "output"]
    );
    let result = plan(
        inspect(&path).unwrap(),
        TargetProtection::HlsAes128,
        vec![OutputFormat::Mp4],
    );
    assert!(!result.compatible);
}

#[test]
fn missing_input_is_a_classified_error_without_the_path() {
    let secret = "/missing/credential-sentinel/asset.mp4";
    let error = inspect(std::path::Path::new(secret)).unwrap_err();
    assert_eq!(error.to_string(), "input-unreadable");
    assert!(!format!("{error:?}").contains("credential-sentinel"));
}

#[test]
fn manifest_declared_protection_is_not_media_verification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.mpd");
    std::fs::write(&path, r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"><Period><AdaptationSet contentType="video" codecs="avc1.64001f"><ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/><Representation id="v0"/></AdaptationSet></Period></MPD>"#).unwrap();
    let report = inspect(&path).unwrap();
    assert_eq!(report.format, InputFormat::Dash);
    assert_eq!(report.protection, Protection::Cenc);
    assert!(!report.media_verified);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn all_sixteen_protection_intents_have_explicit_key_roles_and_execution_limits() {
    use drmr_core::inspection::{
        Evidence, Inspection, OutputFormat, TargetProtection, Track, TrackKind, plan,
    };
    for source in [
        Protection::Clear,
        Protection::Cenc,
        Protection::Cbcs,
        Protection::HlsAes128,
    ] {
        for target in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
            TargetProtection::HlsAes128,
        ] {
            let input = Inspection::new(
                InputFormat::Hls,
                Evidence::Manifest,
                vec![Track {
                    id: "track-0".into(),
                    kind: TrackKind::Video,
                    codec: None,
                    protection: source,
                    key_ids: Vec::new(),
                    pattern: None,
                }],
                Vec::new(),
                Vec::new(),
            );
            let output = if target == TargetProtection::Cenc {
                OutputFormat::Dash
            } else {
                OutputFormat::Hls
            };
            let report = plan(input, target, vec![output]);
            assert!(report.compatible, "{source:?} -> {target:?}");
            assert!(!report.execution_available);
            assert_eq!(
                report.keys.iter().any(|key| key.role == "input"),
                source != Protection::Clear
            );
            assert_eq!(
                report.keys.iter().any(|key| key.role == "output"),
                target != TargetProtection::Clear
            );
        }
    }
}

#[test]
fn an_empty_manifest_is_not_a_compatible_conversion_source() {
    use drmr_core::inspection::{TargetProtection, plan};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.mpd");
    std::fs::write(&path, "<MPD><Period/></MPD>").unwrap();
    let report = plan(inspect(&path).unwrap(), TargetProtection::Clear, Vec::new());
    assert!(!report.compatible);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "source-protection-unresolved")
    );
}
