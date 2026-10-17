//! Generated DASH and HLS manifests for synthetic packaged tracks.

#[path = "common/mod.rs"]
mod private_fixture;

#[path = "support/paths.rs"]
mod paths;
use paths::portable_path;

#[path = "manifest_fixture/common.rs"]
#[allow(
    dead_code,
    reason = "shared by the manifest test crates; each uses a subset"
)]
mod common;

use common::{
    FAIRPLAY_URI, IV_HEX, KID_HEX, KID_UUID, files_under, fixture, playready, read, text,
};
use drmr_core::manifest_gen::{
    OutputSegment, OutputTrack, PackagedPresentation, PrivateFairPlaySignaling, write_dash,
    write_hls,
};
use drmr_core::manifest_support::fraction::Fraction;
use drmr_core::manifest_support::py::PyErr;
use drmr_core::manifest_support::xml::{self, Element};
use serde_json::{Value, json};
use std::path::Path;

const MPD_NS: &str = "urn:mpeg:dash:schema:mpd:2011";
const MSPR_NS: &str = "urn:microsoft:playready";

/// `presentation(tmp_path)` of the test: one video, two audio languages, a clear `WebVTT`
/// track; two segments each (2 s + 1 s at 90 kHz) starting at 2 s, the common origin 2 s.
fn presentation(tmp: &Path) -> PackagedPresentation {
    let track = |id: &str, kind: &str, codec: &str, group: &str, language: &str, clear: bool| {
        let folder = tmp.join("cbcs").join(id);
        std::fs::create_dir_all(&folder).unwrap();
        let init = folder.join("init.mp4");
        std::fs::write(&init, "init").unwrap();
        let segments = [180_000, 90_000]
            .iter()
            .enumerate()
            .map(|(i, duration)| {
                let path = folder.join(format!("seg_{i}.m4s"));
                std::fs::write(&path, "media").unwrap();
                let index = i64::try_from(i).unwrap();
                OutputSegment {
                    source_index: index,
                    path,
                    timescale: 90000,
                    decode_start: 180_000 + index * 180_000,
                    duration: *duration,
                    presentation_start: 180_000 + index * 180_000,
                    sample_count: 48,
                }
            })
            .collect();
        let video = kind == "video";
        OutputTrack {
            language: language.to_string(),
            role: "main".to_string(),
            name: id.to_string(),
            group_id: group.to_string(),
            width: if video { 128 } else { 0 },
            height: if video { 72 } else { 0 },
            frame_rate: if video {
                "24/1".to_string()
            } else {
                String::new()
            },
            encrypted: !clear,
            ..OutputTrack::new(id, kind, codec, segments, (!clear).then_some(init), 1000)
        }
    };
    PackagedPresentation {
        tracks: vec![
            track("v000", "video", "avc1.42c00c", "", "und", false),
            track("a000", "audio", "mp4a.40.2", "ja", "ja", false),
            track("a001", "audio", "mp4a.40.2", "en", "en", false),
            track("t000", "text", "wvtt", "", "und", true),
        ],
        key_id_hex: KID_HEX.to_string(),
        widevine_pssh: hex::decode(text(&fixture("manifest_gen.json")["widevine_pssh"])).unwrap(),
        playready: playready(),
        origin: Fraction::from_int(2),
    }
}

fn fairplay() -> PrivateFairPlaySignaling {
    PrivateFairPlaySignaling {
        uri: FAIRPLAY_URI.to_string(),
        iv_hex: IV_HEX.to_string(),
    }
}

fn map_tracks(p: &mut PackagedPresentation, change: impl Fn(&mut OutputTrack)) {
    p.tracks.iter_mut().for_each(change);
}

fn descendants<'a>(element: &'a Element, local: &str) -> Vec<&'a Element> {
    element
        .iter_tree()
        .skip(1)
        .filter(|e| e.local_name() == local)
        .collect()
}

const DESCRIPTIONS: &str = "public.accessibility.describes-video";
const CAPTIONS: &str =
    "public.accessibility.transcribes-spoken-dialog,public.accessibility.describes-music-and-sound";

// ------------------------------------------------------------------------------------------
// the fixture
// ------------------------------------------------------------------------------------------

#[test]
fn test_three_drm_hls_and_common_urls_with_clear_text() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let p = presentation(tmp.path());
    let paths = write_hls(&p, &tmp.path().join("manifests/master.m3u8"), &fairplay()).unwrap();
    let master = read(&paths[0]);
    assert!(master.contains("AUDIO=\"ja\"") && master.contains("AUDIO=\"en\""));
    assert!(master.contains("SUBTITLES=\"subtitles\""));
    let media = read(&paths[1]);
    assert_eq!(media.matches("#EXT-X-KEY:").count(), 3);
    for system in [
        "com.apple.streamingkeydelivery",
        "com.microsoft.playready",
        "urn:uuid:edef8ba9",
    ] {
        assert!(media.contains(system));
    }
    assert!(media.contains(&format!("IV=0x{IV_HEX}")));
    assert!(media.contains("#EXT-X-MEDIA-SEQUENCE:0"));
    assert!(media.contains("#EXT-X-TARGETDURATION:2"));
    assert!(!read(paths.last().unwrap()).contains("#EXT-X-KEY"));
    for path in &paths[1..] {
        for line in read(path).lines() {
            if !line.is_empty() && !line.starts_with('#') {
                assert!(path.parent().unwrap().join(line).is_file(), "{line}");
            }
        }
    }
}

#[test]
fn test_dash_static_timing_drm_and_same_media_urls() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let p = presentation(tmp.path());
    let path = write_dash(&p, &tmp.path().join("manifests/manifest.mpd")).unwrap();
    let root = xml::fromstring(&read(&path)).unwrap();
    assert_eq!(root.tag, format!("{{{MPD_NS}}}MPD"));
    assert_eq!(root.get("type"), Some("static"));
    assert_eq!(
        root.get("profiles"),
        Some("urn:mpeg:dash:profile:full:2011")
    );
    assert_eq!(root.get("mediaPresentationDuration"), Some("PT3S"));
    assert_eq!(descendants(&root, "Representation").len(), p.tracks.len());
    let pros = descendants(&root, "pro");
    assert_eq!(pros.len(), 3);
    assert!(
        pros.iter()
            .all(|pro| pro.tag == format!("{{{MSPR_NS}}}pro"))
    );
    for list in descendants(&root, "SegmentList") {
        assert_eq!(list.get("startNumber"), Some("0"));
        assert_eq!(list.get("presentationTimeOffset"), Some("180000"));
    }
    for media in descendants(&root, "SegmentURL") {
        assert!(
            path.parent()
                .unwrap()
                .join(media.get("media").unwrap())
                .is_file()
        );
    }
    assert!(
        root.iter_tree()
            .all(|e| e.get("segmentAlignment").is_none())
    );
    for adaptation in descendants(&root, "AdaptationSet") {
        let names: Vec<&str> = adaptation
            .children
            .iter()
            .map(Element::local_name)
            .collect();
        let role = names.iter().position(|n| *n == "Role").unwrap();
        if let Some(last_cp) = names.iter().rposition(|n| *n == "ContentProtection") {
            assert!(last_cp < role);
        }
        if let Some(accessibility) = names.iter().position(|n| *n == "Accessibility") {
            assert!(accessibility < role);
        }
    }
}

fn with_roles(p: &mut PackagedPresentation) {
    map_tracks(p, |t| {
        if t.id == "a000" {
            t.role = DESCRIPTIONS.to_string();
        } else if t.id == "t000" {
            t.role = CAPTIONS.to_string();
        }
    });
}

#[test]
fn test_accessibility_utis_preserved_in_hls_and_mapped_in_dash() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let mut p = presentation(tmp.path());
    with_roles(&mut p);
    let paths = write_hls(&p, &tmp.path().join("manifests/master.m3u8"), &fairplay()).unwrap();
    let master = read(&paths[0]);
    assert!(master.contains(&format!("CHARACTERISTICS=\"{DESCRIPTIONS}\"")));
    assert!(master.contains(&format!("CHARACTERISTICS=\"{CAPTIONS}\"")));
    let dash = read(&write_dash(&p, &tmp.path().join("manifests/manifest.mpd")).unwrap());
    let root = xml::fromstring(&dash).unwrap();
    let mut values: Vec<&str> = descendants(&root, "Accessibility")
        .iter()
        .map(|a| a.get("value").unwrap())
        .collect();
    values.sort_unstable();
    values.dedup();
    assert_eq!(values, vec!["caption", "description"]);
    assert!(
        descendants(&root, "Role")
            .iter()
            .all(|r| !r.get("value").unwrap().starts_with("public."))
    );
}

#[test]
fn test_unknown_role_fails_instead_of_losing_semantics() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let mut p = presentation(tmp.path());
    map_tracks(&mut p, |t| {
        if t.id == "a000" {
            t.role = "private.unspecified".to_string();
        }
    });
    let err = write_dash(&p, &tmp.path().join("manifest.mpd")).unwrap_err();
    assert_eq!(err, PyErr::Value("unsupported-rendition-role".to_string()));
}

fn with_vtt_offset(p: &mut PackagedPresentation) {
    map_tracks(p, |t| {
        if t.content_type == "text" {
            t.dash_presentation_time_offset = Some(Fraction::from_int(0));
        }
    });
}

#[test]
fn test_dash_vtt_keeps_raw_cue_clock_independent_of_transport_origin() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let mut p = presentation(tmp.path());
    with_vtt_offset(&mut p);
    let path = write_dash(&p, &tmp.path().join("manifest.mpd")).unwrap();
    let root = xml::fromstring(&read(&path)).unwrap();
    let list_of = |content_type: &str| {
        descendants(&root, "AdaptationSet")
            .into_iter()
            .find(|a| a.get("contentType") == Some(content_type))
            .map(|a| descendants(a, "SegmentList")[0].clone())
            .unwrap()
    };
    let (text_list, video_list) = (list_of("text"), list_of("video"));
    assert_eq!(text_list.get("presentationTimeOffset"), Some("0"));
    assert_eq!(descendants(&text_list, "S")[0].get("t"), Some("0"));
    assert_eq!(video_list.get("presentationTimeOffset"), Some("180000"));
    let pto: i64 = text_list
        .get("presentationTimeOffset")
        .unwrap()
        .parse()
        .unwrap();
    let scale: i64 = text_list.get("timescale").unwrap().parse().unwrap();
    let rendered = &Fraction::from_int(0) - &Fraction::new(pto, scale).unwrap();
    assert_eq!(rendered, Fraction::from_int(0));
}

fn with_languages(p: &mut PackagedPresentation) {
    map_tracks(p, |t| {
        if t.id == "a000" {
            t.group_id = "languages".to_string();
            t.language = "en".to_string();
            t.is_default = false;
            t.autoselect = false;
        } else if t.id == "a001" {
            t.group_id = "languages".to_string();
            t.language = "ja".to_string();
            t.is_default = true;
            t.autoselect = true;
        } else if t.content_type == "text" {
            t.is_default = false;
            t.autoselect = true;
        }
    });
}

#[test]
fn test_hls_preserves_second_language_default_and_autoselect_flags() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let mut p = presentation(tmp.path());
    with_languages(&mut p);
    let master = read(&write_hls(&p, &tmp.path().join("master.m3u8"), &fairplay()).unwrap()[0]);
    let audio: Vec<&str> = master
        .lines()
        .filter(|l| l.starts_with("#EXT-X-MEDIA:TYPE=AUDIO"))
        .collect();
    assert!(audio[0].contains("LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=NO"));
    assert!(audio[1].contains("LANGUAGE=\"ja\",DEFAULT=YES,AUTOSELECT=YES"));
    assert!(master.contains("TYPE=SUBTITLES") && master.contains("DEFAULT=NO,AUTOSELECT=YES"));
}

#[test]
fn test_hls_rejects_conflicting_default_autoselect_flags() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let mut p = presentation(tmp.path());
    map_tracks(&mut p, |t| {
        if t.content_type == "audio" {
            t.is_default = true;
            t.autoselect = false;
        }
    });
    let err = write_hls(&p, &tmp.path().join("master.m3u8"), &fairplay()).unwrap_err();
    assert_eq!(
        err,
        PyErr::Value("invalid-rendition-selection-flags".to_string())
    );
}

// ------------------------------------------------------------------------------------------
// Byte-for-byte against the committed reference fixtures: every presentation the generator wrote
// ------------------------------------------------------------------------------------------

fn outcome_of<T: Into<Value>>(result: Result<T, PyErr>) -> Value {
    match result {
        Ok(value) => json!({ "ok": value.into() }),
        Err(err) => json!({ "error": [err.class_name(), err.to_string()] }),
    }
}

/// The generator's `emit`: write the HLS and / or DASH manifests of a variant of the base
/// presentation and compare results and files with Python's.
fn emit(name: &str, change: impl Fn(&mut PackagedPresentation), hls: bool, dash: bool) {
    let expected = &fixture("manifest_gen.json")[name];
    let tmp = private_fixture::private_tempdir().unwrap();
    let root = tmp.path();
    let mut p = presentation(root);
    change(&mut p);
    let relative = |path: &Path| portable_path(path.strip_prefix(root).unwrap());
    if hls {
        let result = write_hls(&p, &root.join("manifests/master.m3u8"), &fairplay())
            .map(|paths| paths.iter().map(|p| relative(p)).collect::<Vec<_>>());
        assert_eq!(outcome_of(result), expected["hls"], "{name}: write_hls");
    }
    if dash {
        let result =
            write_dash(&p, &root.join("manifests/manifest.mpd")).map(|path| relative(&path));
        assert_eq!(outcome_of(result), expected["dash"], "{name}: write_dash");
    }
    let manifests = root.join("manifests");
    let written = if manifests.is_dir() {
        files_under(&manifests)
    } else {
        Vec::new()
    };
    if let Some(files) = expected.get("files") {
        let actual: serde_json::Map<String, Value> = written
            .iter()
            .map(|file| (file.clone(), Value::String(read(&manifests.join(file)))))
            .collect();
        assert_eq!(&Value::Object(actual), files, "{name}: files");
    } else {
        assert_eq!(
            json!(written),
            expected["written"],
            "{name}: files written before the error"
        );
    }
}

fn change_track(
    id: &'static str,
    change: impl Fn(&mut OutputTrack),
) -> impl Fn(&mut PackagedPresentation) {
    move |p| {
        for track in &mut p.tracks {
            if track.id == id {
                change(track);
            }
        }
    }
}

#[test]
fn generated_manifests_match_python_for_every_variant() {
    emit("base", |_| {}, true, true);
    emit("roles", with_roles, true, true);
    emit(
        "short_roles",
        |p| {
            map_tracks(p, |t| match t.id.as_str() {
                "a000" => t.role = "description".to_string(),
                "t000" => t.role = "caption".to_string(),
                "v000" => t.role = String::new(),
                _ => {}
            });
        },
        true,
        true,
    );
    emit("vtt", with_vtt_offset, true, true);
    emit("languages", with_languages, true, true);
    emit(
        "unknown_role",
        change_track("a000", |t| t.role = "private.unspecified".to_string()),
        true,
        true,
    );
    emit(
        "conflicting_flags",
        |p| {
            map_tracks(p, |t| {
                if t.content_type == "audio" {
                    t.is_default = true;
                    t.autoselect = false;
                }
            });
        },
        true,
        true,
    );
    emit(
        "two_defaults",
        |p| {
            map_tracks(p, |t| {
                if t.content_type == "audio" {
                    t.is_default = true;
                    t.autoselect = true;
                }
            });
        },
        true,
        true,
    );
    emit(
        "audio_only",
        |p| p.tracks.retain(|t| t.content_type == "audio"),
        true,
        true,
    );
    emit(
        "audio_group",
        change_track("v000", |t| {
            t.audio_group = "en".to_string();
            t.frame_rate = "30000/1001".to_string();
            t.bandwidth = 0;
        }),
        true,
        true,
    );
    emit(
        "missing_audio_group",
        change_track("v000", |t| t.audio_group = "fr".to_string()),
        true,
        true,
    );
    emit(
        "subtitle_group",
        change_track("v000", |t| t.subtitle_group = "other".to_string()),
        true,
        true,
    );
    emit(
        "two_subtitle_groups",
        |p| {
            let mut extra = p.tracks[3].clone();
            extra.id = "t001".to_string();
            extra.group_id = "extra".to_string();
            p.tracks.push(extra);
        },
        true,
        true,
    );
    emit(
        "bad_quote",
        change_track("a000", |t| t.name = "a\"b".to_string()),
        true,
        true,
    );
    emit(
        "bad_frame_rate",
        change_track("v000", |t| t.frame_rate = "30000 / 1001".to_string()),
        true,
        true,
    );
}

#[test]
fn generated_manifest_errors_and_edge_shapes_match_python() {
    emit(
        "negative_origin",
        |p| {
            map_tracks(p, |t| {
                if t.content_type == "text" {
                    t.dash_presentation_time_offset = Some(Fraction::from_int(-1));
                }
            });
        },
        true,
        true,
    );
    emit(
        "fraction_origin",
        |p| p.origin = Fraction::new(1, 3).unwrap(),
        true,
        true,
    );
    emit(
        "no_segments",
        change_track("t000", |t| t.segments.clear()),
        true,
        true,
    );
    emit(
        "all_empty",
        |p| map_tracks(p, |t| t.segments.clear()),
        true,
        true,
    );
    emit(
        "zero_timescale",
        change_track("a001", |t| {
            t.segments.iter_mut().for_each(|s| s.timescale = 0);
        }),
        true,
        true,
    );
    emit("bad_kid", |p| p.key_id_hex = "zz".repeat(16), true, true);
    emit(
        "braced_kid",
        |p| p.key_id_hex = format!("{{{}}}", KID_UUID.to_uppercase()),
        true,
        true,
    );
    emit(
        "escapes",
        change_track("a001", |t| {
            t.id = "a&<b>\"q".to_string();
            t.language = "ja\tx".to_string();
        }),
        false,
        true,
    );
}

#[test]
fn fairplay_signaling_never_debug_prints_its_secrets() {
    let printed = format!("{:?}", fairplay());
    assert!(!printed.contains(FAIRPLAY_URI) && !printed.contains(IV_HEX));
    let tmp = private_fixture::private_tempdir().unwrap();
    let printed = format!("{:?}", presentation(tmp.path()));
    assert!(!printed.contains(IV_HEX));
}
