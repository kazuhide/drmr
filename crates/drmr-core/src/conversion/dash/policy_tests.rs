use super::*;
use crate::{concurrency::Stop, conversion::dash_addressing::DashAddressing};
use std::process::Command;

fn package(root: &Path, protection: &str, roles: &[&str]) {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    package_source(root, &clip, protection, roles, "2");
}

fn package_source(root: &Path, source: &Path, protection: &str, roles: &[&str], seconds: &str) {
    let mut command = Command::new("packager");
    for role in roles {
        let directory = root.join(role);
        crate::native::create_media_dir_all(&directory).unwrap();
        command.arg(format!(
            "in={},stream={role},init_segment={}/init.mp4,segment_template={}/seg_$Number$.m4s",
            source.display(),
            directory.display(),
            directory.display()
        ));
    }
    command.args([
        "--segment_duration",
        seconds,
        "--fragment_duration",
        seconds,
        "--generate_static_live_mpd",
        "--mp4_reset_initial_composition_offset_to_zero=false",
    ]);
    if protection != "clear" {
        command.args([
            "--enable_raw_key_encryption",
            "--clear_lead",
            "0",
            "--protection_scheme",
            protection,
            "--keys",
            "label=:key_id=000102030405060708090a0b0c0d0e0f:key=101112131415161718191a1b1c1d1e1f",
        ]);
    }
    let result = command
        .arg("--mpd_output")
        .arg(root.join("manifest.mpd"))
        .output()
        .unwrap();
    assert!(result.status.success(), "fixture packaging failed");
    crate::native::seal_media_tree(root).unwrap();
}

#[test]
fn nominal_audio_without_a_planned_duration_uses_the_admitted_six_second_cadence() {
    for rate in [44_100_u32, 48_000] {
        let owner = super::super::files::Stage::new(None).unwrap();
        let coded = super::super::files::Stage::new(None).unwrap();
        let source = coded.path().join("audio.mp4");
        let clip =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
        let result = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-stream_loop", "2", "-i"])
            .arg(clip)
            .args([
                "-map", "0:a:0", "-vn", "-t", "20", "-c:a", "aac", "-threads", "1", "-ar",
            ])
            .arg(rate.to_string())
            .arg(&source)
            .output()
            .unwrap();
        assert!(result.status.success(), "fixture encoding failed");
        package_source(owner.path(), &source, "clear", &["audio"], "6");
        let manifest = owner.path().join("manifest.mpd");
        let before_stage = super::super::files::Stage::new(None).unwrap();
        let before = snapshot_segments(&manifest, before_stage.path(), &Stop::new()).unwrap();
        assert!(before.representations[0].resources.len() >= 4);
        rewrite_output(
            owner.path(),
            DashAddressing::TemplateDuration,
            None,
            &Stop::new(),
        )
        .unwrap();
        verify_output_addressing(owner.path(), DashAddressing::TemplateDuration, &Stop::new())
            .unwrap();
        let text = fs::read_to_string(&manifest).unwrap();
        let document = Document::parse(&text).unwrap();
        let rep = document
            .descendants()
            .find(|node| node.tag_name().name() == "Representation")
            .unwrap();
        let resolved = addressing::resolve(rep, MAX_SEGMENTS).unwrap();
        let duration = resolved.nominal.unwrap().duration;
        // AAC boundaries may differ by one frame. They cannot turn a six-second source into two seconds.
        assert!(
            u128::from(duration.abs_diff(resolved.timescale * 6)) * u128::from(rate)
                <= 1024 * u128::from(resolved.timescale)
        );
        let after_stage = super::super::files::Stage::new(None).unwrap();
        let after = snapshot_segments(&manifest, after_stage.path(), &Stop::new()).unwrap();
        assert_eq!(bytes(&before), bytes(&after));
        for (before, after) in before.representations[0]
            .resources
            .iter()
            .zip(&after.representations[0].resources)
        {
            assert_eq!(before.1, after.1);
        }
    }
}

fn bytes(snapshot: &Segmented) -> Vec<Vec<Vec<u8>>> {
    snapshot
        .representations
        .iter()
        .map(|rep| {
            std::iter::once(&rep.initialization)
                .chain(rep.resources.iter().map(|(path, _)| path))
                .map(|path| fs::read(path).unwrap())
                .collect()
        })
        .collect()
}

fn all_modes() -> [DashAddressing; 4] {
    [
        DashAddressing::Template,
        DashAddressing::TemplateDuration,
        DashAddressing::List,
        DashAddressing::Base,
    ]
}

#[test]
fn exact_list_and_indexed_forms_reinput_without_changing_media_or_actual_clocks() {
    for protection in ["clear", "cenc", "cbcs"] {
        for mode in [DashAddressing::List, DashAddressing::Base] {
            let owner = super::super::files::Stage::new(None).unwrap();
            package(owner.path(), protection, &["video", "audio"]);
            let manifest = owner.path().join("manifest.mpd");
            let before_stage = super::super::files::Stage::new(None).unwrap();
            let before = snapshot_segments(&manifest, before_stage.path(), &Stop::new()).unwrap();
            verify_output_addressing(owner.path(), DashAddressing::Template, &Stop::new()).unwrap();
            rewrite_output(owner.path(), mode, Some(2000), &Stop::new()).unwrap();
            verify_output_addressing(owner.path(), mode, &Stop::new()).unwrap();
            for wrong in all_modes() {
                if wrong != mode {
                    assert_eq!(
                        verify_output_addressing(owner.path(), wrong, &Stop::new()),
                        Err(error("output-addressing-verification-failed"))
                    );
                }
            }
            let projection = fs::read(&manifest).unwrap();
            check_acquired_limits(owner.path(), &projection, 100, &Stop::new()).unwrap();
            assert!(check_acquired_limits(owner.path(), &projection, 1, &Stop::new()).is_err());
            let after_stage = super::super::files::Stage::new(None).unwrap();
            let after = snapshot_segments(&manifest, after_stage.path(), &Stop::new())
                .unwrap_or_else(|error| panic!("{protection} {mode:?}: {error:?}"));
            assert_eq!(bytes(&before), bytes(&after), "{protection} {mode:?}");
            for (before, after) in before.representations.iter().zip(&after.representations) {
                assert_eq!(
                    before
                        .resources
                        .iter()
                        .map(|(_, clock)| clock)
                        .collect::<Vec<_>>(),
                    after
                        .resources
                        .iter()
                        .map(|(_, clock)| clock)
                        .collect::<Vec<_>>()
                );
                assert_eq!(before.window.first, after.window.first);
                assert_eq!(before.window.end, after.window.end);
            }
        }
    }
}

#[test]
fn indexed_source_layout_keeps_unique_stems_for_representations_in_the_same_directory() {
    use std::collections::BTreeMap;
    let owner = super::super::files::Stage::new(None).unwrap();
    package(owner.path(), "clear", &["video", "audio"]);
    let manifest = owner.path().join("manifest.mpd");
    let before_stage = super::super::files::Stage::new(None).unwrap();
    let before = snapshot_segments(&manifest, before_stage.path(), &Stop::new()).unwrap();
    let original = fs::read_to_string(&manifest).unwrap();
    let document = Document::parse(&original).unwrap();
    let mut files = BTreeMap::new();
    let mut templates = BTreeMap::new();
    super::super::files::create_dir(&owner.path().join("shared")).unwrap();
    for (ordinal, rep) in document
        .descendants()
        .filter(|node| node.tag_name().name() == "Representation")
        .enumerate()
    {
        let resolved = addressing::resolve(rep, MAX_SEGMENTS).unwrap();
        let prefix = format!("shared/track-{ordinal:04}");
        let init = resolved
            .base
            .join(relative(&resolved.initialization.uri).unwrap());
        files.insert(init.clone(), PathBuf::from(format!("{prefix}-init.mp4")));
        let start_number = resolved.segments[0].number;
        for segment in resolved.segments {
            let old = resolved.base.join(relative(&segment.resource.uri).unwrap());
            files.insert(
                old,
                PathBuf::from(format!("{prefix}-{}.m4s", segment.number)),
            );
        }
        templates.insert(
            init,
            crate::conversion::layout::RelocatedTemplate {
                media: Some(format!("{prefix}-$Number$.m4s")),
                start_number: Some(start_number),
            },
        );
    }
    for (old, new) in &files {
        fs::rename(owner.path().join(old), owner.path().join(new)).unwrap();
    }
    relocate_output(owner.path(), &files, &templates, &Stop::new()).unwrap();
    rewrite_output(owner.path(), DashAddressing::Base, Some(2000), &Stop::new()).unwrap();
    verify_output_addressing(owner.path(), DashAddressing::Base, &Stop::new()).unwrap();
    assert_eq!(
        layout_resources(&manifest, &Stop::new()).unwrap(),
        [
            vec![PathBuf::from("shared/track-0000-init-indexed.mp4")],
            vec![PathBuf::from("shared/track-0001-init-indexed.mp4")]
        ]
    );
    let after_stage = super::super::files::Stage::new(None).unwrap();
    let after = snapshot_segments(&manifest, after_stage.path(), &Stop::new()).unwrap();
    assert_eq!(bytes(&before), bytes(&after));
    for (before, after) in before.representations.iter().zip(&after.representations) {
        assert_eq!(before.window.first, after.window.first);
        assert_eq!(before.window.end, after.window.end);
    }
}

#[test]
fn addressing_verification_rejects_a_mixed_representation_contract() {
    let owner = super::super::files::Stage::new(None).unwrap();
    package(owner.path(), "clear", &["video", "audio"]);
    let manifest = owner.path().join("manifest.mpd");
    let original = fs::read_to_string(&manifest).unwrap();
    let template = {
        let document = Document::parse(&original).unwrap();
        let rep = document
            .descendants()
            .filter(|node| node.tag_name().name() == "Representation")
            .nth(1)
            .unwrap();
        original[one(rep, "SegmentTemplate").unwrap().unwrap().range()].to_owned()
    };
    rewrite_output(owner.path(), DashAddressing::List, Some(2000), &Stop::new()).unwrap();
    let mut mixed = fs::read_to_string(&manifest).unwrap();
    let range = {
        let document = Document::parse(&mixed).unwrap();
        document
            .descendants()
            .filter(|node| node.tag_name().name() == "SegmentList")
            .nth(1)
            .unwrap()
            .range()
    };
    mixed.replace_range(range, &template);
    fs::write(&manifest, &mixed).unwrap();
    source_references(mixed.as_bytes()).unwrap();
    for mode in all_modes() {
        assert_eq!(
            verify_output_addressing(owner.path(), mode, &Stop::new()),
            Err(error("output-addressing-verification-failed"))
        );
    }
}

#[test]
fn indexed_addressing_verification_checks_the_actual_index_bytes() {
    use std::io::{Seek as _, SeekFrom, Write as _};
    let owner = super::super::files::Stage::new(None).unwrap();
    package(owner.path(), "clear", &["video"]);
    rewrite_output(owner.path(), DashAddressing::Base, Some(2000), &Stop::new()).unwrap();
    let manifest = owner.path().join("manifest.mpd");
    let original = fs::read_to_string(&manifest).unwrap();
    let document = Document::parse(&original).unwrap();
    let rep = document
        .descendants()
        .find(|node| node.tag_name().name() == "Representation")
        .unwrap();
    let resolved = addressing::resolve(rep, MAX_SEGMENTS).unwrap();
    let resource = resolved.index.unwrap();
    let path = owner.path().join(resolved.base).join(resource.uri);
    verify_output_addressing(owner.path(), DashAddressing::Base, &Stop::new()).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
    // An unchanged MPD cannot hide a corrupt SIDX box type in the output media.
    file.seek(SeekFrom::Start(resource.range.unwrap().start + 4))
        .unwrap();
    file.write_all(b"free").unwrap();
    file.sync_all().unwrap();
    assert_eq!(
        verify_output_addressing(owner.path(), DashAddressing::Base, &Stop::new()),
        Err(error("output-addressing-verification-failed"))
    );
    assert_eq!(fs::read_to_string(&manifest).unwrap(), original);
}

#[test]
fn nominal_template_reinput_keeps_actual_variable_segment_durations() {
    let owner = super::super::files::Stage::new(None).unwrap();
    package(owner.path(), "clear", &["video"]);
    let manifest = owner.path().join("manifest.mpd");
    let before_stage = super::super::files::Stage::new(None).unwrap();
    let before = snapshot_segments(&manifest, before_stage.path(), &Stop::new()).unwrap();
    rewrite_output(
        owner.path(),
        DashAddressing::TemplateDuration,
        Some(2000),
        &Stop::new(),
    )
    .unwrap();
    verify_output_addressing(owner.path(), DashAddressing::TemplateDuration, &Stop::new()).unwrap();
    assert_eq!(
        verify_output_addressing(owner.path(), DashAddressing::Template, &Stop::new()),
        Err(error("output-addressing-verification-failed"))
    );
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(text.contains(" duration=\""));
    assert!(!text.contains("SegmentTimeline"));
    assert!(!text.contains("eptDelta"));
    let after_stage = super::super::files::Stage::new(None).unwrap();
    let after = snapshot_segments(&manifest, after_stage.path(), &Stop::new()).unwrap();
    assert_eq!(bytes(&before), bytes(&after));
    assert_eq!(
        before.representations[0]
            .resources
            .iter()
            .map(|(_, clock)| clock)
            .collect::<Vec<_>>(),
        after.representations[0]
            .resources
            .iter()
            .map(|(_, clock)| clock)
            .collect::<Vec<_>>()
    );
}

#[test]
fn source_relocation_proves_every_template_expansion_before_rewriting_the_manifest() {
    use std::collections::BTreeMap;
    let owner = super::super::files::Stage::new(None).unwrap();
    package(owner.path(), "clear", &["video", "audio"]);
    let manifest = owner.path().join("manifest.mpd");
    let original = fs::read_to_string(&manifest).unwrap();
    let before_stage = super::super::files::Stage::new(None).unwrap();
    let before = snapshot_segments(&manifest, before_stage.path(), &Stop::new()).unwrap();
    let document = Document::parse(&original).unwrap();
    let mut files = BTreeMap::new();
    let mut templates = BTreeMap::new();
    for (ordinal, rep) in document
        .descendants()
        .filter(|node| node.tag_name().name() == "Representation")
        .enumerate()
    {
        let resolved = addressing::resolve(rep, MAX_SEGMENTS).unwrap();
        let directory = PathBuf::from(if ordinal == 0 {
            "video/1080p"
        } else {
            "audio/en"
        });
        super::super::files::create_dir(&owner.path().join(&directory)).unwrap();
        let init = resolved
            .base
            .join(relative(&resolved.initialization.uri).unwrap());
        files.insert(init.clone(), directory.join("header.mp4"));
        for segment in resolved.segments {
            let old = resolved.base.join(relative(&segment.resource.uri).unwrap());
            files.insert(old, directory.join(format!("part-{}.m4s", segment.number)));
        }
        templates.insert(
            init,
            crate::conversion::layout::RelocatedTemplate {
                media: Some(format!("{}/part-$Number$.m4s", directory.display())),
                start_number: Some(1),
            },
        );
    }
    for (old, new) in &files {
        fs::rename(owner.path().join(old), owner.path().join(new)).unwrap();
    }
    let mut wrong = templates.clone();
    wrong.insert(
        PathBuf::from("video/init.mp4"),
        crate::conversion::layout::RelocatedTemplate {
            media: Some("video/1080p/other-$Number$.m4s".into()),
            start_number: Some(1),
        },
    );
    assert_eq!(
        relocate_output(owner.path(), &files, &wrong, &Stop::new()),
        Err(error("source-layout-template-mismatch"))
    );
    assert_eq!(fs::read_to_string(&manifest).unwrap(), original);
    relocate_output(owner.path(), &files, &templates, &Stop::new()).unwrap();
    let relocated = fs::read_to_string(&manifest).unwrap();
    assert!(relocated.contains("SegmentTemplate"));
    assert!(!relocated.contains("SegmentList"));
    rewrite_output(owner.path(), DashAddressing::Base, Some(2000), &Stop::new()).unwrap();
    let after_stage = super::super::files::Stage::new(None).unwrap();
    let after = snapshot_segments(&manifest, after_stage.path(), &Stop::new()).unwrap();
    assert_eq!(bytes(&before), bytes(&after));
    assert_eq!(
        layout_resources(&manifest, &Stop::new()).unwrap(),
        [
            vec![PathBuf::from("video/1080p/header-indexed.mp4")],
            vec![PathBuf::from("audio/en/header-indexed.mp4")]
        ]
    );
}
