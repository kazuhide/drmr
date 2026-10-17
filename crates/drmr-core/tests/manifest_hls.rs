//! HLS playlist mapping, structural rewrites, and media verification gates.

#[path = "support/paths.rs"]
mod paths;
use paths::portable_path;

#[path = "manifest_fixture/common.rs"]
#[allow(
    dead_code,
    reason = "shared by the manifest test crates; each uses a subset"
)]
mod common;
#[path = "manifest_fixture/hls.rs"]
#[allow(
    dead_code,
    reason = "shared by the manifest test crates; each uses a subset"
)]
mod hls_fixture;

use common::{IV_HEX, KID_HEX, error_of, fixture, read, text};
use drmr_core::checks::CheckId;
use drmr_core::config::{KeyMaterial, Rendition};
use drmr_core::manifest_hls::{
    EXTINF_TOLERANCE_S, GateMedia, HlsError, HlsManifestSummary, HlsRewriteResult,
    HlsVariantMapping, analyze_hls_manifest, audio_pcm_consolidation_gate, check_extinf_gate,
    content_identity_gate, find_audio_consolidation_pairs, map_hls_variants_to_dash, rewrite_hls,
    rewrite_hls_master, rewrite_hls_media_playlist,
};
use drmr_core::manifest_support::path;
use drmr_core::manifest_support::py::PyErr;
use hls_fixture::{
    DashRenditionSpec, HlsOptions, SyntheticHlsAsset, audio, build_bbb_like_asset,
    build_synthetic_hls_asset, make_dash_rendition, variant,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ------------------------------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------------------------------

fn uuid_of(playlist: &Path) -> String {
    playlist
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

fn mapping_by_uuid(mappings: &[HlsVariantMapping]) -> BTreeMap<String, String> {
    mappings
        .iter()
        .map(|m| (uuid_of(&m.hls_playlist), m.dash_rendition.rep_id.clone()))
        .collect()
}

fn mapped(asset: &SyntheticHlsAsset, uuid: &str) -> HlsVariantMapping {
    map_hls_variants_to_dash(&asset.master_path, &asset.renditions)
        .unwrap()
        .into_iter()
        .find(|m| uuid_of(&m.hls_playlist) == uuid)
        .unwrap()
}

fn uri_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

fn check(result: &HlsRewriteResult, id: CheckId) -> &str {
    result
        .checks
        .iter()
        .find(|(check, _)| *check == id)
        .map(|(_, status)| status.as_str())
        .unwrap()
}

fn class_and_message(err: &HlsError) -> (String, String) {
    let class = match err {
        HlsError::Rewrite(_) => "HlsRewriteError",
        HlsError::Py(inner) => inner.class_name(),
    };
    (class.to_string(), err.to_string())
}

fn resolved_root(asset: &SyntheticHlsAsset) -> PathBuf {
    path::resolve(&asset.download_root).unwrap()
}

fn mapping_json(mapping: &HlsVariantMapping, root: &Path) -> Value {
    json!({
        "hls_playlist": portable_path(mapping.hls_playlist.strip_prefix(root).unwrap()),
        "dash_rep_id": mapping.dash_rendition.rep_id,
        "signals": mapping.signals.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
        "content_gate": mapping.content_gate,
    })
}

fn summary_json(summary: &HlsManifestSummary) -> Value {
    let num = |value: &Option<num_bigint::BigInt>| value.as_ref().map(ToString::to_string);
    json!({
        "is_master": summary.is_master,
        "version": num(&summary.version),
        "variants": summary.variants.iter().map(|v| json!({
            "kind": v.kind, "uri": v.uri, "codecs": v.codecs, "resolution": v.resolution,
            "bandwidth": num(&v.bandwidth), "audio_group": v.audio_group, "group_id": v.group_id,
            "container": v.container, "has_ext_x_map": v.has_ext_x_map,
            "key_method": v.key_method, "key_format": v.key_format, "key_iv": v.key_iv,
            "version": num(&v.version),
        })).collect::<Vec<_>>(),
    })
}

fn result_json(result: &HlsRewriteResult, out_root: &Path) -> Value {
    let relative = |p: &Path| portable_path(p.strip_prefix(out_root).unwrap());
    json!({
        "output_master_path": relative(&result.output_master_path),
        "output_media_playlist_paths": result.output_media_playlist_paths.iter().map(|p| relative(p)).collect::<Vec<_>>(),
        "audio_groups_consolidated": result.audio_groups_consolidated,
        "ts_audio_groups_removed": result.ts_audio_groups_removed,
        "ext_x_map_added": result.ext_x_map_added,
        "segment_refs_repointed": result.segment_refs_repointed,
        "iv_verified": result.iv_verified,
        "checks": result.checks.iter().map(|(id, status)| json!([id.as_str(), status])).collect::<Vec<_>>(),
    })
}

/// Every `*.m3u8` under `root`, as Python's `tree_texts` lists them.
fn m3u8_texts(root: &Path) -> Value {
    let mut out = serde_json::Map::new();
    for relative in common::files_under(root) {
        if Path::new(&relative)
            .extension()
            .is_some_and(|ext| ext == "m3u8")
        {
            out.insert(relative.clone(), Value::String(read(&root.join(&relative))));
        }
    }
    Value::Object(out)
}

fn sha_tree(root: &Path) -> BTreeMap<String, String> {
    common::files_under(root)
        .into_iter()
        .map(|relative| {
            let digest = hex::encode(Sha256::digest(std::fs::read(root.join(&relative)).unwrap()));
            (relative, digest)
        })
        .collect()
}

fn h264<'a>(rep_id: &'a str, uuid: &'a str, seg_size: usize) -> DashRenditionSpec<'a> {
    DashRenditionSpec::new(rep_id, "avc1.42C00C", "video", uuid, seg_size)
}

fn one_audio_group() -> Vec<hls_fixture::AudioSpec> {
    vec![audio("fmp4_AUDIO-0", "a0", 100, "m4s")]
}

// ------------------------------------------------------------------------------------------
// -- multi-signal-agreement mapping
// ------------------------------------------------------------------------------------------

#[test]
fn test_mapping_pairs_by_codec_family_and_size_rank() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let by_uuid =
        mapping_by_uuid(&map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap());
    assert_eq!(by_uuid["hv-h264-c"], "d-h264-lo");
    assert_eq!(by_uuid["hv-h264-a"], "d-h264-mid");
    assert_eq!(by_uuid["hv-h264-b"], "d-h264-hi");
    assert_eq!(by_uuid["hv-h265-c"], "d-h265-lo");
    assert_eq!(by_uuid["hv-h265-a"], "d-h265-mid");
    assert_eq!(by_uuid["hv-h265-b"], "d-h265-hi");
}

#[test]
fn test_mapping_audio_uses_size_rank_not_group_index() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let by_uuid =
        mapping_by_uuid(&map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap());
    assert_eq!(by_uuid["ha-fmp4-0"], "d-aud-256k");
    assert_eq!(by_uuid["ha-fmp4-1"], "d-aud-64k");
    assert!(!by_uuid.contains_key("ha-ts-0") && !by_uuid.contains_key("ha-ts-1"));
}

#[test]
fn test_mapping_yields_one_entry_per_surviving_playlist() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    assert_eq!(
        map_hls_variants_to_dash(&asset.master_path, &asset.renditions)
            .unwrap()
            .len(),
        8
    );
}

#[test]
fn test_mapping_content_gate_labels() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let mappings = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap();
    let gate = |uuid: &str| {
        mappings
            .iter()
            .find(|m| uuid_of(&m.hls_playlist) == uuid)
            .unwrap()
            .content_gate
            .clone()
    };
    assert_eq!(gate("hv-h264-a"), "waived:TS-H264");
    assert_eq!(gate("hv-h265-a"), "framemd5-eq");
    assert_eq!(gate("ha-fmp4-0"), "pcm-eq");
}

#[test]
fn test_mapping_records_evidence_signals() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let m = mapped(&asset, "hv-h265-a");
    assert_eq!(m.signal("codec_family"), Some("hvc1"));
    assert_eq!(m.signal("resolution"), Some("320x180"));
    assert_eq!(m.signal("frame_rate"), Some("24.000"));
    assert_eq!(m.signal("hls_bandwidth_rank"), m.signal("hls_size_rank"));
    assert_eq!(m.signal("dash_rep_id"), Some("d-h265-mid"));
    for (_, value) in &m.signals {
        assert!(!value.contains(IV_HEX) && !value.to_lowercase().contains("key"));
    }
}

#[test]
fn test_mapping_disambiguates_with_uniform_resolution_and_framerate() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let mappings = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap();
    let resolutions: std::collections::BTreeSet<&str> = mappings
        .iter()
        .filter(|m| m.signal("content_type") == Some("video"))
        .map(|m| m.signal("resolution").unwrap())
        .collect();
    assert_eq!(resolutions.into_iter().collect::<Vec<_>>(), vec!["320x180"]);
    assert_eq!(mappings.len(), 8);
}

fn two_h264_asset(
    root: &Path,
    dash_sizes: [usize; 2],
    variants: &[hls_fixture::VariantSpec],
) -> SyntheticHlsAsset {
    let renditions = vec![
        make_dash_rendition(root, &h264("d0", "d0", dash_sizes[0])),
        make_dash_rendition(root, &h264("d1", "d1", dash_sizes[1])),
    ];
    build_synthetic_hls_asset(
        root,
        variants,
        &one_audio_group(),
        renditions,
        &HlsOptions::default(),
    )
}

#[test]
fn test_mapping_fails_loud_on_bandwidth_size_rank_disagreement() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = two_h264_asset(
        tmp.path(),
        [1000, 2000],
        &[
            variant("avc1.42C00C", 100_000, "fmp4_AUDIO-0", "v0", 5000, 1, "ts"),
            variant("avc1.42C00C", 900_000, "fmp4_AUDIO-0", "v1", 1000, 2, "ts"),
        ],
    );
    let err = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap_err();
    assert!(err.to_string().contains("rank"));
    assert_eq!(
        class_and_message(&err),
        error_of(&fixture("manifest_hls.json")["errors"]["rank_disagreement"])
    );
}

#[test]
fn test_mapping_fails_loud_on_size_tie() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = two_h264_asset(
        tmp.path(),
        [1000, 2000],
        &[
            variant("avc1.42C00C", 100_000, "fmp4_AUDIO-0", "v0", 2000, 1, "ts"),
            variant("avc1.42C00C", 200_000, "fmp4_AUDIO-0", "v1", 2000, 2, "ts"),
        ],
    );
    let err = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap_err();
    assert!(err.to_string().contains("tie") || err.to_string().contains("disambiguate"));
    assert_eq!(
        class_and_message(&err),
        error_of(&fixture("manifest_hls.json")["errors"]["size_tie"])
    );
}

#[test]
fn test_mapping_fails_loud_on_missing_bandwidth() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let text = read(&asset.master_path);
    std::fs::write(
        &asset.master_path,
        text.replacen("BANDWIDTH=655005,", "", 1),
    )
    .unwrap();
    let err = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap_err();
    assert!(err.to_string().contains("BANDWIDTH"));
    let errors = &fixture("manifest_hls.json")["errors"];
    assert_eq!(
        class_and_message(&err),
        error_of(&errors["missing_bandwidth"])
    );
    assert_eq!(
        summary_json(&analyze_hls_manifest(&asset.master_path).unwrap()),
        errors["missing_bandwidth_analyze"]
    );
    std::fs::write(
        &asset.master_path,
        text.replacen("BANDWIDTH=655005,", "BANDWIDTH=x6,", 1),
    )
    .unwrap();
    let err = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap_err();
    assert_eq!(class_and_message(&err), error_of(&errors["bad_bandwidth"]));
    std::fs::write(
        &asset.master_path,
        text.replacen("BANDWIDTH=655005,", "BANDWIDTH=inf,", 1),
    )
    .unwrap();
    let err = analyze_hls_manifest(&asset.master_path).unwrap_err();
    assert_eq!(
        class_and_message(&err),
        error_of(&errors["inf_bandwidth_analyze"])
    );
}

#[test]
fn test_mapping_fails_loud_on_count_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let renditions = vec![make_dash_rendition(tmp.path(), &h264("d0", "d0", 1000))];
    let variants = vec![
        variant("avc1.42C00C", 100_000, "fmp4_AUDIO-0", "v0", 1000, 1, "ts"),
        variant("avc1.42C00C", 200_000, "fmp4_AUDIO-0", "v1", 2000, 2, "ts"),
    ];
    let asset = build_synthetic_hls_asset(
        tmp.path(),
        &variants,
        &one_audio_group(),
        renditions,
        &HlsOptions::default(),
    );
    let err = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap_err();
    assert!(err.to_string().contains("count mismatch"));
    assert_eq!(
        class_and_message(&err),
        error_of(&fixture("manifest_hls.json")["errors"]["count_mismatch"])
    );
}

#[test]
fn test_mapping_fails_loud_on_segment_count_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let video = DashRenditionSpec {
        seg_count: 5,
        ..h264("d0", "d0", 1000)
    };
    let audio_rendition = DashRenditionSpec {
        seg_count: 5,
        timescale: 48000,
        ..DashRenditionSpec::new("da", "mp4a.40.2", "audio", "da", 50)
    };
    let renditions = vec![
        make_dash_rendition(tmp.path(), &video),
        make_dash_rendition(tmp.path(), &audio_rendition),
    ];
    let mut v0 = variant("avc1.42C00C", 100_000, "fmp4_AUDIO-0", "v0", 1000, 1, "ts");
    v0.seg_count = 3;
    let mut a0 = audio("fmp4_AUDIO-0", "a0", 100, "m4s");
    a0.seg_count = 5;
    let asset =
        build_synthetic_hls_asset(tmp.path(), &[v0], &[a0], renditions, &HlsOptions::default());
    let err = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap_err();
    assert!(err.to_string().contains("segment-count"));
    assert_eq!(
        class_and_message(&err),
        error_of(&fixture("manifest_hls.json")["errors"]["segment_count"])
    );
}

// ------------------------------------------------------------------------------------------
// /  /  -- master
// ------------------------------------------------------------------------------------------

fn rewritten_master(tmp: &Path) -> (SyntheticHlsAsset, String) {
    let asset = build_bbb_like_asset(tmp);
    let out = tmp.join("out/master.m3u8");
    assert_eq!(
        rewrite_hls_master(&asset.master_path, &out, None).unwrap(),
        out
    );
    let text = read(&out);
    (asset, text)
}

#[test]
fn test_master_consolidates_audio_index_wise() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, text) = rewritten_master(tmp.path());
    assert!(!text.contains("GROUP-ID=\"ts_AUDIO-0\"") && !text.contains("GROUP-ID=\"ts_AUDIO-1\""));
    assert!(
        text.contains("GROUP-ID=\"fmp4_AUDIO-0\"") && text.contains("GROUP-ID=\"fmp4_AUDIO-1\"")
    );
    assert!(!text.contains("AUDIO=\"ts_AUDIO-0\"") && !text.contains("AUDIO=\"ts_AUDIO-1\""));
    assert_eq!(text.matches("AUDIO=\"fmp4_AUDIO-0\"").count(), 4);
    assert_eq!(text.matches("AUDIO=\"fmp4_AUDIO-1\"").count(), 2);
}

#[test]
fn test_master_no_dangling_audio_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, text) = rewritten_master(tmp.path());
    let present: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("#EXT-X-MEDIA:") && line.contains("GROUP-ID="))
        .map(|line| {
            line.split_once("GROUP-ID=\"")
                .unwrap()
                .1
                .split('"')
                .next()
                .unwrap()
        })
        .collect();
    for line in text
        .lines()
        .filter(|l| l.starts_with("#EXT-X-STREAM-INF:") && l.contains("AUDIO=\""))
    {
        let group = line
            .split_once("AUDIO=\"")
            .unwrap()
            .1
            .split('"')
            .next()
            .unwrap();
        assert!(present.contains(&group));
    }
}

#[test]
fn test_master_diff_is_exactly_audio_flip_and_ts_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let (asset, dst) = rewritten_master(tmp.path());
    let src = read(&asset.master_path);
    let src_lines: Vec<&str> = src.split('\n').collect();
    let dst_lines: Vec<&str> = dst.split('\n').collect();
    let removed: Vec<&str> = src_lines
        .iter()
        .filter(|l| !dst_lines.contains(l))
        .copied()
        .collect();
    let added: Vec<&str> = dst_lines
        .iter()
        .filter(|l| !src_lines.contains(l))
        .copied()
        .collect();
    assert_eq!(
        removed
            .iter()
            .filter(|l| l.starts_with("#EXT-X-MEDIA:") && l.contains("ts_AUDIO"))
            .count(),
        2
    );
    assert!(removed.iter().all(
        |l| (l.starts_with("#EXT-X-MEDIA:") && l.contains("ts_AUDIO"))
            || l.starts_with("#EXT-X-STREAM-INF:")
    ));
    assert!(added.iter().all(|l| l.starts_with("#EXT-X-STREAM-INF:")));
    for a in added {
        let restored = a
            .replace("AUDIO=\"fmp4_AUDIO-0\"", "AUDIO=\"ts_AUDIO-0\"")
            .replace("AUDIO=\"fmp4_AUDIO-1\"", "AUDIO=\"ts_AUDIO-1\"");
        assert!(removed.contains(&restored.as_str()));
    }
}

#[test]
fn test_master_version_not_bumped() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, text) = rewritten_master(tmp.path());
    assert!(text.contains("#EXT-X-VERSION:4"));
}

#[test]
fn test_master_media_playlist_uris_and_tokens_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let (asset, dst) = rewritten_master(tmp.path());
    let src = read(&asset.master_path);
    assert_eq!(uri_lines(&src), uri_lines(&dst));
    assert!(uri_lines(&dst).iter().all(|l| l.contains("?vt=")));
}

#[test]
fn test_master_trailing_newline_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, text) = rewritten_master(tmp.path());
    assert!(text.ends_with('\n') && !text.ends_with("\n\n"));
}

// ------------------------------------------------------------------------------------------
// /
// ------------------------------------------------------------------------------------------

#[test]
fn test_master_fails_loud_on_unmodeled_construct() {
    let bad_lines = [
        "#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=100000,URI=\"iframe.m3u8\"",
        "#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,KEYFORMAT=\"com.apple.streamingkeydelivery\",URI=\"skd://x\"",
        "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"en\",URI=\"subs.m3u8\"",
        "#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"en\",INSTREAM-ID=\"CC1\"",
    ];
    let errors = &fixture("manifest_hls.json")["errors"];
    for (index, bad_line) in bad_lines.iter().enumerate() {
        let tmp = tempfile::tempdir().unwrap();
        let renditions = vec![make_dash_rendition(tmp.path(), &h264("d0", "d0", 100))];
        let options = HlsOptions {
            extra_master_lines: vec![(*bad_line).to_string()],
            ..HlsOptions::default()
        };
        let asset = build_synthetic_hls_asset(
            tmp.path(),
            &[variant(
                "avc1.42C00C",
                1,
                "fmp4_AUDIO-0",
                "v0",
                100,
                1,
                "ts",
            )],
            &[audio("fmp4_AUDIO-0", "a0", 50, "m4s")],
            renditions,
            &options,
        );
        let err =
            rewrite_hls_master(&asset.master_path, &tmp.path().join("o.m3u8"), None).unwrap_err();
        assert!(err.to_string().contains("unmodeled"));
        assert_eq!(
            class_and_message(&err),
            error_of(&errors[format!("unmodeled_{index}")])
        );
    }
}

#[test]
fn test_master_closed_captions_none_attribute_is_benign() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    assert!(read(&asset.master_path).contains("CLOSED-CAPTIONS=NONE"));
    rewrite_hls_master(&asset.master_path, &tmp.path().join("o.m3u8"), None).unwrap();
}

#[test]
fn test_master_rerun_on_own_output_is_byte_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let (out1, out2) = (tmp.path().join("out1.m3u8"), tmp.path().join("out2.m3u8"));
    rewrite_hls_master(&asset.master_path, &out1, None).unwrap();
    rewrite_hls_master(&out1, &out2, None).unwrap();
    assert_eq!(std::fs::read(out2).unwrap(), std::fs::read(out1).unwrap());
}

// ------------------------------------------------------------------------------------------
// /  /  /  -- media playlists
// ------------------------------------------------------------------------------------------

fn rewrite_media(
    asset: &SyntheticHlsAsset,
    uuid: &str,
    out: &Path,
    iv: &str,
    url_base: Option<&str>,
) -> Result<String, HlsError> {
    let m = mapped(asset, uuid);
    rewrite_hls_media_playlist(&m.hls_playlist, out, &m.dash_rendition, iv, url_base)?;
    Ok(read(out))
}

#[test]
fn test_media_playlist_ts_gains_ext_x_map_and_repoints_segments() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let text = rewrite_media(
        &asset,
        "hv-h264-c",
        &tmp.path().join("out.m3u8"),
        IV_HEX,
        None,
    )
    .unwrap();
    assert_eq!(text.matches("#EXT-X-MAP:").count(), 1);
    assert!(text.contains(
        "#EXT-X-MAP:URI=\"https://vod.test/title-hls/dashsub0000/video/dv-h264-lo/init.mp4\""
    ));
    let segments = uri_lines(&text);
    assert_ne!(segments.len(), 0);
    for (index, line) in segments.iter().enumerate() {
        assert_eq!(
            *line,
            format!("https://vod.test/title-hls/dashsub0000/video/dv-h264-lo/seg_{index}.m4s")
        );
    }
    assert!(!text.contains(".ts"));
}

#[test]
fn test_media_playlist_fmp4_repoints_existing_ext_x_map() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let text = rewrite_media(
        &asset,
        "hv-h265-a",
        &tmp.path().join("out.m3u8"),
        IV_HEX,
        None,
    )
    .unwrap();
    assert_eq!(text.matches("#EXT-X-MAP:").count(), 1);
    assert!(text.contains(
        "#EXT-X-MAP:URI=\"https://vod.test/title-hls/dashsub0000/video/dv-h265-mid/init.mp4\""
    ));
    for (index, line) in uri_lines(&text).iter().enumerate() {
        assert_eq!(
            *line,
            format!("https://vod.test/title-hls/dashsub0000/video/dv-h265-mid/seg_{index}.m4s")
        );
    }
}

#[test]
fn test_media_playlist_key_line_copied_verbatim_and_iv_verified() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let m = mapped(&asset, "hv-h264-c");
    let key_line = |text: &str| {
        text.lines()
            .find(|l| l.starts_with("#EXT-X-KEY:"))
            .unwrap()
            .to_string()
    };
    let src_key = key_line(&read(&m.hls_playlist));
    let text = rewrite_media(
        &asset,
        "hv-h264-c",
        &tmp.path().join("out.m3u8"),
        IV_HEX,
        None,
    )
    .unwrap();
    assert_eq!(key_line(&text), src_key);
}

#[test]
fn test_media_playlist_iv_accepts_0x_prefixed_expected() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    rewrite_media(
        &asset,
        "hv-h264-c",
        &tmp.path().join("out.m3u8"),
        &format!("0x{IV_HEX}"),
        None,
    )
    .unwrap();
    rewrite_media(
        &asset,
        "hv-h264-c",
        &tmp.path().join("out2.m3u8"),
        &format!("0X{}", IV_HEX.to_uppercase()),
        None,
    )
    .unwrap();
}

#[test]
fn test_media_playlist_iv_mismatch_raises() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let err = rewrite_media(
        &asset,
        "hv-h264-c",
        &tmp.path().join("out.m3u8"),
        "ffffffffffffffffffffffffffffffff",
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("IV"));
    // The message never carries the IV value.
    assert!(
        !err.to_string().contains(IV_HEX) && !err.to_string().to_lowercase().contains("ffffffff")
    );
    assert_eq!(
        class_and_message(&err),
        error_of(&fixture("manifest_hls.json")["errors"]["iv_mismatch"])
    );
}

#[test]
fn test_media_playlist_version_not_bumped() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let text = rewrite_media(
        &asset,
        "hv-h264-c",
        &tmp.path().join("out.m3u8"),
        IV_HEX,
        None,
    )
    .unwrap();
    assert!(text.contains("#EXT-X-VERSION:7"));
}

#[test]
fn test_media_playlist_extinf_and_other_lines_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let m = mapped(&asset, "hv-h264-c");
    let text = rewrite_media(
        &asset,
        "hv-h264-c",
        &tmp.path().join("out.m3u8"),
        IV_HEX,
        None,
    )
    .unwrap();
    let extinf = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| l.starts_with("#EXTINF:"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(extinf(&read(&m.hls_playlist)), extinf(&text));
    for tag in [
        "#EXTM3U",
        "#EXT-X-PLAYLIST-TYPE:VOD",
        "#EXT-X-TARGETDURATION:6",
        "#EXT-X-ENDLIST",
    ] {
        assert!(text.contains(tag));
    }
}

#[test]
fn test_media_playlist_trailing_newline_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let text = rewrite_media(
        &asset,
        "hv-h264-c",
        &tmp.path().join("out.m3u8"),
        IV_HEX,
        None,
    )
    .unwrap();
    assert!(text.ends_with('\n') && !text.ends_with("\n\n"));
}

#[test]
fn test_media_playlist_deploy_form_matches_dash_segment_urls() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let text = rewrite_media(
        &asset,
        "hv-h265-a",
        &tmp.path().join("out.m3u8"),
        IV_HEX,
        None,
    )
    .unwrap();
    assert_eq!(
        uri_lines(&text)[0],
        "https://vod.test/title-hls/dashsub0000/video/dv-h265-mid/seg_0.m4s"
    );
}

#[test]
fn test_media_playlist_local_form_points_at_cbcs_root() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let text = rewrite_media(
        &asset,
        "hv-h265-a",
        &tmp.path().join("out.m3u8"),
        IV_HEX,
        Some("http://localhost:8000/asset/cbcs/"),
    )
    .unwrap();
    assert_eq!(
        uri_lines(&text)[0],
        "http://localhost:8000/asset/cbcs/video/dv-h265-mid/seg_0.m4s"
    );
    assert!(text.contains(
        "#EXT-X-MAP:URI=\"http://localhost:8000/asset/cbcs/video/dv-h265-mid/init.mp4\""
    ));
}

#[test]
fn test_media_playlist_local_form_relative_base() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let text = rewrite_media(
        &asset,
        "hv-h264-c",
        &tmp.path().join("out.m3u8"),
        IV_HEX,
        Some("../../cbcs/"),
    )
    .unwrap();
    assert_eq!(uri_lines(&text)[0], "../../cbcs/video/dv-h264-lo/seg_0.m4s");
}

#[test]
fn test_media_playlist_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let m = mapped(&asset, "hv-h264-c");
    let (out1, out2) = (tmp.path().join("o1.m3u8"), tmp.path().join("o2.m3u8"));
    rewrite_hls_media_playlist(&m.hls_playlist, &out1, &m.dash_rendition, IV_HEX, None).unwrap();
    rewrite_hls_media_playlist(&out1, &out2, &m.dash_rendition, IV_HEX, None).unwrap();
    assert_eq!(std::fs::read(out2).unwrap(), std::fs::read(out1).unwrap());
}

#[test]
fn test_inputs_not_modified() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let before = sha_tree(&asset.download_root);
    let mappings = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap();
    rewrite_hls_master(&asset.master_path, &tmp.path().join("o/master.m3u8"), None).unwrap();
    for m in &mappings {
        let out = tmp
            .path()
            .join(format!("o/{}.m3u8", uuid_of(&m.hls_playlist)));
        rewrite_hls_media_playlist(&m.hls_playlist, &out, &m.dash_rendition, IV_HEX, None).unwrap();
    }
    assert_eq!(sha_tree(&asset.download_root), before);
}

// ------------------------------------------------------------------------------------------
// -- analyze_hls_manifest
// ------------------------------------------------------------------------------------------

#[test]
fn test_analyze_master_returns_video_variant_and_audio_group_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let summary = analyze_hls_manifest(&asset.master_path).unwrap();
    assert!(summary.is_master);
    assert_eq!(summary.version, Some(4.into()));
    let video: Vec<_> = summary
        .variants
        .iter()
        .filter(|v| v.kind == "video-variant")
        .collect();
    let audio_rows: Vec<_> = summary
        .variants
        .iter()
        .filter(|v| v.kind == "audio-group")
        .collect();
    assert_eq!(video.len(), 6);
    assert_eq!(audio_rows.len(), 4);
    let row = video
        .iter()
        .find(|v| v.bandwidth == Some(655_005.into()))
        .unwrap();
    assert_eq!(row.codecs.as_deref(), Some("avc1.42C00C,mp4a.40.2"));
    assert_eq!(row.resolution.as_deref(), Some("320x180"));
    assert_eq!(row.audio_group.as_deref(), Some("ts_AUDIO-0"));
    assert_eq!(row.version, Some(4.into()));
    assert!(row.container.is_none() && !row.has_ext_x_map);
    assert!(row.key_method.is_none() && row.key_iv.is_none());
    assert!(row.group_id.is_none());
    let a_row = audio_rows
        .iter()
        .find(|v| v.group_id.as_deref() == Some("fmp4_AUDIO-0"))
        .unwrap();
    assert!(a_row.uri.as_deref().unwrap().contains("fmp4_audio"));
    assert_eq!(a_row.version, Some(4.into()));
    assert!(a_row.bandwidth.is_none() && a_row.codecs.is_none());
    assert!(a_row.audio_group.is_none());
}

#[test]
fn test_analyze_master_does_not_open_referenced_media_playlists() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    for relative in common::files_under(&asset.download_root) {
        let file = asset.download_root.join(&relative);
        if file != asset.master_path {
            std::fs::remove_file(file).unwrap();
        }
    }
    let summary = analyze_hls_manifest(&asset.master_path).unwrap();
    assert!(summary.is_master);
    assert_eq!(summary.variants.len(), 10);
}

#[test]
fn test_analyze_master_bandwidth_degrades_to_none_on_malformed_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let text = read(&asset.master_path);
    std::fs::write(
        &asset.master_path,
        text.replacen("BANDWIDTH=655005,", "", 1),
    )
    .unwrap();
    let summary = analyze_hls_manifest(&asset.master_path).unwrap();
    let bandwidths: Vec<_> = summary
        .variants
        .iter()
        .filter(|v| v.kind == "video-variant")
        .map(|v| v.bandwidth.clone())
        .collect();
    assert_eq!(bandwidths.iter().filter(|b| b.is_none()).count(), 1);
    assert_eq!(bandwidths.iter().filter(|b| b.is_some()).count(), 5);
}

#[test]
fn test_analyze_media_playlist_ts_h264_reports_no_map() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let summary = analyze_hls_manifest(&asset.media_playlist_path("hv-h264-a", "video")).unwrap();
    assert!(!summary.is_master);
    assert_eq!(summary.version, Some(7.into()));
    assert_eq!(summary.variants.len(), 1);
    let row = &summary.variants[0];
    assert_eq!(row.kind, "media-playlist");
    assert_eq!(row.container.as_deref(), Some("ts"));
    assert!(!row.has_ext_x_map);
    assert_eq!(row.key_method.as_deref(), Some("SAMPLE-AES"));
    assert_eq!(
        row.key_format.as_deref(),
        Some("com.apple.streamingkeydelivery")
    );
    assert_eq!(
        row.key_iv.as_deref(),
        Some(format!("0x{}", IV_HEX.to_uppercase()).as_str())
    );
    assert_eq!(row.version, Some(7.into()));
    assert!(row.codecs.is_none() && row.resolution.is_none() && row.bandwidth.is_none());
    assert!(row.audio_group.is_none() && row.group_id.is_none());
}

#[test]
fn test_analyze_media_playlist_fmp4_h265_reports_ext_x_map() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let summary = analyze_hls_manifest(&asset.media_playlist_path("hv-h265-a", "video")).unwrap();
    assert!(!summary.is_master);
    let row = &summary.variants[0];
    assert_eq!(row.container.as_deref(), Some("m4s"));
    assert!(row.has_ext_x_map);
    assert_eq!(row.key_method.as_deref(), Some("SAMPLE-AES"));
    assert_eq!(
        row.key_format.as_deref(),
        Some("com.apple.streamingkeydelivery")
    );
    assert_eq!(
        row.key_iv.as_deref(),
        Some(format!("0x{}", IV_HEX.to_uppercase()).as_str())
    );
    assert_eq!(row.version, Some(7.into()));
}

#[test]
fn test_analyze_audio_media_playlist_is_media_playlist_kind_not_audio_group() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let summary = analyze_hls_manifest(&asset.media_playlist_path("ha-fmp4-0", "audio")).unwrap();
    assert!(!summary.is_master);
    let row = &summary.variants[0];
    assert_eq!(row.kind, "media-playlist");
    assert_eq!(row.container.as_deref(), Some("m4s"));
    assert!(row.has_ext_x_map);
}

#[test]
fn test_analyze_media_playlist_key_format_defaults_to_none_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let playlist = tmp.path().join("media.m3u8");
    let lines = [
        "#EXTM3U",
        "#EXT-X-VERSION:3",
        "#EXT-X-TARGETDURATION:6",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"https://example.test/key\"",
        "#EXTINF:6,",
        "seg_0.ts",
        "#EXT-X-ENDLIST",
        "",
    ];
    std::fs::write(&playlist, lines.join("\n")).unwrap();
    let summary = analyze_hls_manifest(&playlist).unwrap();
    assert!(!summary.is_master);
    let row = &summary.variants[0];
    assert_eq!(row.key_method.as_deref(), Some("AES-128"));
    assert!(row.key_format.is_none() && row.key_iv.is_none());
    assert_eq!(row.container.as_deref(), Some("ts"));
    assert!(!row.has_ext_x_map);
    assert_eq!(row.version, Some(3.into()));
    assert_eq!(
        summary_json(&summary),
        fixture("manifest_hls.json")["analyze_plain"]
    );
}

#[test]
fn test_analyze_hls_manifest_never_leaks_content_key() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    for target in [
        asset.media_playlist_path("hv-h264-a", "video"),
        asset.master_path.clone(),
    ] {
        for row in analyze_hls_manifest(&target).unwrap().variants {
            if let Some(iv) = row.key_iv {
                assert!(!iv.to_lowercase().contains("key"));
            }
        }
    }
}

// ------------------------------------------------------------------------------------------
// rewrite_hls -> HlsRewriteResult
// ------------------------------------------------------------------------------------------

#[test]
fn test_rewrite_hls_result_counts_and_checks() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let result = rewrite_hls(
        &asset.master_path,
        &asset.renditions,
        &tmp.path().join("output/manifests"),
        IV_HEX,
        None,
    )
    .unwrap();
    assert_eq!(result.audio_groups_consolidated, 3);
    assert_eq!(result.ts_audio_groups_removed, 2);
    assert_eq!(result.ext_x_map_added, 3);
    assert_eq!(result.segment_refs_repointed, 24);
    assert!(result.iv_verified);
    assert_eq!(result.variant_mapping.len(), 8);
    for id in [
        CheckId::HlsIvAgreement,
        CheckId::HlsSegmentCount,
        CheckId::HlsVersionUnchanged,
        CheckId::AudioConsolidationStructure,
        CheckId::HlsMasterUntouched,
    ] {
        assert_eq!(check(&result, id), "PASS", "{id}");
    }
    for id in [
        CheckId::AudioPcmIdentity,
        CheckId::HlsContentGate,
        CheckId::HlsExtinf,
    ] {
        assert!(check(&result, id).starts_with("SKIP"), "{id}");
    }
}

#[test]
fn test_rewrite_hls_output_layout_mirrors_source() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let out_root = tmp.path().join("output/manifests");
    let result = rewrite_hls(
        &asset.master_path,
        &asset.renditions,
        &out_root,
        IV_HEX,
        None,
    )
    .unwrap();
    assert!(result.output_master_path.exists());
    let has_part = |p: &Path, part: &str| p.components().any(|c| c.as_os_str() == part);
    assert!(has_part(&result.output_master_path, "manifest.test"));
    assert_eq!(
        result.output_master_path.file_name().unwrap(),
        "manifest.m3u8"
    );
    assert_eq!(result.output_media_playlist_paths.len(), 8);
    for p in &result.output_media_playlist_paths {
        assert!(p.exists());
        assert!(p.starts_with(&out_root));
        assert!(has_part(p, "variants.test"));
    }
}

#[test]
fn test_rewrite_hls_deploy_vs_local_forms_from_one_code_path() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let deploy = rewrite_hls(
        &asset.master_path,
        &asset.renditions,
        &tmp.path().join("deploy"),
        IV_HEX,
        None,
    )
    .unwrap();
    let local = rewrite_hls(
        &asset.master_path,
        &asset.renditions,
        &tmp.path().join("local"),
        IV_HEX,
        Some("http://localhost/cbcs/"),
    )
    .unwrap();
    let first_seg = |paths: &[PathBuf], uuid: &str| -> String {
        let p = paths
            .iter()
            .find(|p| p.components().any(|c| c.as_os_str() == uuid))
            .unwrap();
        uri_lines(&read(p))[0].to_string()
    };
    assert_eq!(
        first_seg(&deploy.output_media_playlist_paths, "hv-h265-a"),
        "https://vod.test/title-hls/dashsub0000/video/dv-h265-mid/seg_0.m4s"
    );
    assert_eq!(
        first_seg(&local.output_media_playlist_paths, "hv-h265-a"),
        "http://localhost/cbcs/video/dv-h265-mid/seg_0.m4s"
    );
    assert_eq!(
        std::fs::read(&deploy.output_master_path).unwrap(),
        std::fs::read(&local.output_master_path).unwrap()
    );
}

#[test]
fn test_rewrite_hls_input_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let before = sha_tree(&asset.download_root);
    rewrite_hls(
        &asset.master_path,
        &asset.renditions,
        &tmp.path().join("out"),
        IV_HEX,
        None,
    )
    .unwrap();
    assert_eq!(sha_tree(&asset.download_root), before);
}

// ------------------------------------------------------------------------------------------
// `test_mapping_phase4` (the config half of its first case is configuration's)
// ------------------------------------------------------------------------------------------

fn with_video<F: Fn(&mut Rendition)>(renditions: &[Rendition], change: F) -> Vec<Rendition> {
    renditions
        .iter()
        .cloned()
        .map(|mut r| {
            if r.content_type == "video" {
                change(&mut r);
            }
            r
        })
        .collect()
}

#[test]
fn test_rank_agreement_does_not_hide_resolution_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let renditions = with_video(&asset.renditions, |r| {
        r.width = Some(1920);
        r.height = Some(1080);
    });
    let err = map_hls_variants_to_dash(&asset.master_path, &renditions).unwrap_err();
    assert!(err.to_string().contains("resolution"));
    let phase4 = &fixture("manifest_hls.json")["phase4"];
    assert_eq!(
        class_and_message(&err),
        error_of(&phase4["resolution_mismatch"])
    );
    let matched = with_video(&asset.renditions, |r| {
        r.width = Some(320);
        r.height = Some(180);
    });
    let root = resolved_root(&asset);
    let mappings = map_hls_variants_to_dash(&asset.master_path, &matched).unwrap();
    assert_eq!(
        json!(
            mappings
                .iter()
                .map(|m| mapping_json(m, &root))
                .collect::<Vec<_>>()
        ),
        phase4["resolution_matched"]
    );
}

#[test]
fn test_audio_join_is_language_first_even_when_global_size_ranks_disagree() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    let lines: Vec<String> = read(&asset.master_path)
        .lines()
        .map(|line| {
            if line.starts_with("#EXT-X-MEDIA:") {
                let language = if line.contains("-0\"") {
                    "LANGUAGE=\"ja\""
                } else {
                    "LANGUAGE=\"en\""
                };
                line.replace("LANGUAGE=\"und\"", language)
            } else {
                line.to_string()
            }
        })
        .collect();
    std::fs::write(&asset.master_path, lines.join("\n") + "\n").unwrap();
    let phase4 = &fixture("manifest_hls.json")["phase4"];
    assert_eq!(read(&asset.master_path), text(&phase4["language_master"]));
    let renditions: Vec<Rendition> = asset
        .renditions
        .iter()
        .cloned()
        .map(|mut r| {
            if r.content_type == "audio" {
                r.lang = Some(if r.rep_id == "d-aud-64k" { "ja" } else { "en" }.to_string());
            }
            r
        })
        .collect();
    let mappings = map_hls_variants_to_dash(&asset.master_path, &renditions).unwrap();
    let by_uuid: BTreeMap<String, &HlsVariantMapping> = mappings
        .iter()
        .map(|m| (uuid_of(&m.hls_playlist), m))
        .collect();
    assert_eq!(by_uuid["ha-fmp4-0"].dash_rendition.rep_id, "d-aud-64k");
    assert_eq!(by_uuid["ha-fmp4-0"].signal("language"), Some("ja"));
    assert_eq!(by_uuid["ha-fmp4-1"].dash_rendition.rep_id, "d-aud-256k");
    let root = resolved_root(&asset);
    assert_eq!(
        json!(
            mappings
                .iter()
                .map(|m| mapping_json(m, &root))
                .collect::<Vec<_>>()
        ),
        phase4["language_first"]
    );
    let uneven: Vec<Rendition> = asset
        .renditions
        .iter()
        .cloned()
        .map(|mut r| {
            if r.content_type == "audio" {
                r.lang = Some("ja".to_string());
            }
            r
        })
        .collect();
    let err = map_hls_variants_to_dash(&asset.master_path, &uneven).unwrap_err();
    assert_eq!(
        class_and_message(&err),
        error_of(&phase4["language_counts"])
    );
}

#[test]
fn test_frame_rate_rounding_is_documented_and_disagreements_fail() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    std::fs::write(
        &asset.master_path,
        read(&asset.master_path).replace("FRAME-RATE=24.000", "FRAME-RATE=29.970"),
    )
    .unwrap();
    let rows = with_video(&asset.renditions, |r| {
        r.frame_rate = Some("30000/1001".to_string());
    });
    let mappings = map_hls_variants_to_dash(&asset.master_path, &rows).unwrap();
    assert_eq!(mappings.len(), rows.len());
    let phase4 = &fixture("manifest_hls.json")["phase4"];
    let root = resolved_root(&asset);
    assert_eq!(
        json!(
            mappings
                .iter()
                .map(|m| mapping_json(m, &root))
                .collect::<Vec<_>>()
        ),
        phase4["frame_rate_rounded"]
    );
    let wrong = with_video(&rows, |r| r.frame_rate = Some("24".to_string()));
    let err = map_hls_variants_to_dash(&asset.master_path, &wrong).unwrap_err();
    assert!(err.to_string().contains("frame-rate") || err.to_string().contains("frame rate"));
    assert_eq!(
        class_and_message(&err),
        error_of(&phase4["frame_rate_wrong"])
    );
    let zero = with_video(&rows, |r| r.frame_rate = Some("1/0".to_string()));
    let err = map_hls_variants_to_dash(&asset.master_path, &zero).unwrap_err();
    assert_eq!(
        class_and_message(&err),
        error_of(&phase4["frame_rate_zero_division"])
    );
}

// ------------------------------------------------------------------------------------------
// Byte-for-byte against the committed reference fixtures
// ------------------------------------------------------------------------------------------

#[test]
fn bbb_like_asset_inputs_mappings_and_outputs_match_python() {
    let expected = &fixture("manifest_hls.json")["bbb"];
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(&tmp.path().join("bbb"));
    let root = asset.download_root.clone();
    assert_eq!(
        m3u8_texts(&root),
        expected["inputs"],
        "the builder's playlists"
    );
    let sizes: serde_json::Map<String, Value> = common::files_under(&root)
        .into_iter()
        .map(|relative| {
            let size = std::fs::metadata(root.join(&relative)).unwrap().len();
            (relative, json!(size))
        })
        .collect();
    assert_eq!(
        Value::Object(sizes),
        expected["files"],
        "the builder's files"
    );
    let resolved = resolved_root(&asset);
    let mappings = map_hls_variants_to_dash(&asset.master_path, &asset.renditions).unwrap();
    assert_eq!(
        json!(
            mappings
                .iter()
                .map(|m| mapping_json(m, &resolved))
                .collect::<Vec<_>>()
        ),
        expected["mappings"]
    );
    for (form, url_base) in [("deploy", None), ("local", Some("http://localhost/cbcs/"))] {
        let out_root = tmp.path().join(form);
        let result = rewrite_hls(
            &asset.master_path,
            &asset.renditions,
            &out_root,
            IV_HEX,
            url_base,
        )
        .unwrap();
        assert_eq!(
            result_json(&result, &out_root),
            expected[form]["result"],
            "{form}"
        );
        assert_eq!(m3u8_texts(&out_root), expected[form]["outputs"], "{form}");
    }
    let by_uuid: BTreeMap<String, HlsVariantMapping> = mappings
        .into_iter()
        .map(|m| (uuid_of(&m.hls_playlist), m))
        .collect();
    let cases = [
        ("hv-h264-c", None),
        ("hv-h265-a", None),
        ("hv-h265-a", Some("http://localhost:8000/asset/cbcs/")),
        ("hv-h264-c", Some("../../cbcs/")),
        ("ha-fmp4-0", None),
    ];
    for (index, (uuid, url_base)) in cases.iter().enumerate() {
        let out = tmp.path().join(format!("media/{index}.m3u8"));
        let m = &by_uuid[*uuid];
        rewrite_hls_media_playlist(&m.hls_playlist, &out, &m.dash_rendition, IV_HEX, *url_base)
            .unwrap();
        let key = format!("{uuid}|{}", url_base.unwrap_or("None"));
        assert_eq!(read(&out), text(&expected["media"][&key]), "{key}");
    }
    let (out1, out2) = (
        tmp.path().join("master/out1.m3u8"),
        tmp.path().join("master/out2.m3u8"),
    );
    rewrite_hls_master(&asset.master_path, &out1, None).unwrap();
    rewrite_hls_master(&out1, &out2, None).unwrap();
    assert_eq!(read(&out1), text(&expected["master"]));
    assert_eq!(read(&out2), text(&expected["master_rerun"]));
    let pairs: Vec<Value> = find_audio_consolidation_pairs(&asset.master_path)
        .unwrap()
        .iter()
        .map(|(a, b)| {
            json!([
                portable_path(a.strip_prefix(&resolved).unwrap()),
                portable_path(b.strip_prefix(&resolved).unwrap())
            ])
        })
        .collect();
    assert_eq!(Value::Array(pairs), expected["pairs"]);
    let analyze = &expected["analyze"];
    assert_eq!(
        summary_json(&analyze_hls_manifest(&asset.master_path).unwrap()),
        analyze["master"]
    );
    for (uuid, ct) in [
        ("hv-h264-a", "video"),
        ("hv-h265-a", "video"),
        ("ha-fmp4-0", "audio"),
    ] {
        let summary = analyze_hls_manifest(&asset.media_playlist_path(uuid, ct)).unwrap();
        assert_eq!(summary_json(&summary), analyze[uuid], "{uuid}");
    }
}

#[test]
fn malformed_masters_and_playlists_fail_like_python() {
    let cases = fixture("manifest_hls.json");
    let errors = &cases["errors"];
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(&tmp.path().join("bbb"));
    let text_of_master = read(&asset.master_path);
    let master_cases: Vec<(&str, String)> = vec![
        (
            "unpaired_ts_group",
            text_of_master.replace("GROUP-ID=\"fmp4_AUDIO-1\"", "GROUP-ID=\"fmp4_AUDIO-7\""),
        ),
        (
            "unquoted_audio",
            text_of_master.replacen("AUDIO=\"ts_AUDIO-0\"", "AUDIO=ts_AUDIO-0", 1),
        ),
        ("no_uri_line", {
            let lines: Vec<&str> = text_of_master.split('\n').collect();
            let mut kept: Vec<&str> = lines[..lines.len() - 2].to_vec();
            kept.push(lines[lines.len() - 1]);
            kept.join("\n")
        }),
    ];
    for (name, body) in master_cases {
        std::fs::write(&asset.master_path, body).unwrap();
        let err =
            rewrite_hls_master(&asset.master_path, &tmp.path().join("o.m3u8"), None).unwrap_err();
        assert_eq!(class_and_message(&err), error_of(&errors[name]), "{name}");
    }
    std::fs::write(&asset.master_path, &text_of_master).unwrap();
    let m = mapped(&asset, "hv-h264-c");
    let playlist = read(&m.hls_playlist);
    let iv_attr = format!(",IV=0x{}", IV_HEX.to_uppercase());
    let playlist_cases: Vec<(&str, String)> = vec![
        ("no_iv", playlist.replace(&iv_attr, "")),
        (
            "no_key",
            playlist
                .split('\n')
                .filter(|l| !l.starts_with("#EXT-X-KEY:"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ("no_index", playlist.replace("seg_1.ts", "segment.ts")),
        (
            "map_without_uri",
            playlist.replace("#EXT-X-KEY:", "#EXT-X-MAP:BYTERANGE=\"1@0\"\n#EXT-X-KEY:"),
        ),
    ];
    for (name, body) in playlist_cases {
        let source = tmp.path().join(format!("{name}.m3u8"));
        std::fs::write(&source, body).unwrap();
        let err = rewrite_hls_media_playlist(
            &source,
            &tmp.path().join("o.m3u8"),
            &m.dash_rendition,
            IV_HEX,
            None,
        )
        .unwrap_err();
        assert_eq!(class_and_message(&err), error_of(&errors[name]), "{name}");
    }
    let outside = tmp.path().join("outside-download.m3u8");
    std::fs::write(&outside, &text_of_master).unwrap();
    let err = map_hls_variants_to_dash(&outside, &asset.renditions).unwrap_err();
    let (class, message) = error_of(&errors["no_download_root"]);
    assert_eq!(
        class_and_message(&err),
        (class, message.replace("{path}", &portable_path(&outside)))
    );
}

#[test]
fn odd_masters_analyze_like_python() {
    let cases = fixture("manifest_hls.json");
    let tmp = tempfile::tempdir().unwrap();
    let odd = tmp.path().join("odd-master.m3u8");
    std::fs::write(
        &odd,
        "#EXTM3U\r\n#EXT-X-VERSION: 4 \r\n#EXT-X-STREAM-INF:BANDWIDTH=1e3,CODECS=\"avc1\"\r\nv.m3u8\r\n\
         #EXT-X-STREAM-INF:BANDWIDTH=nan,AUDIO=\"\"\r\nw.m3u8\r\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"\",URI=\"\"\r\n",
    )
    .unwrap();
    assert_eq!(
        summary_json(&analyze_hls_manifest(&odd).unwrap()),
        cases["analyze_odd"]
    );
}

/// `_ATTR_RE` on odd attribute lists, `\d` on Unicode digits in segment names and group
/// indices and `^(.+)-(\d+)$` on group ids, against what Python did with the same text.
#[test]
fn hls_regex_edges_match_python() {
    let edges = &fixture("manifest_hls.json")["regex_edges"];
    let tmp = tempfile::tempdir().unwrap();
    for (index, case) in edges["attrs"].as_array().unwrap().iter().enumerate() {
        let line = text(&case[0]);
        let playlist = tmp.path().join(format!("attrs-{index}.m3u8"));
        std::fs::write(
            &playlist,
            format!("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-KEY:{line}\n#EXTINF:6,\nseg_0.ts\n"),
        )
        .unwrap();
        let summary = analyze_hls_manifest(&playlist).unwrap();
        assert_eq!(summary_json(&summary), case[1], "{line:?}");
    }
    let asset = build_bbb_like_asset(&tmp.path().join("bbb"));
    let m = mapped(&asset, "hv-h264-c");
    let source = tmp.path().join("segments.m3u8");
    std::fs::write(&source, text(&edges["segments"]["source"])).unwrap();
    let target = tmp.path().join("segments.out.m3u8");
    rewrite_hls_media_playlist(&source, &target, &m.dash_rendition, IV_HEX, None).unwrap();
    assert_eq!(read(&target), text(&edges["segments"]["output"]));
    let master = tmp.path().join("groups.m3u8");
    std::fs::write(&master, text(&edges["groups"]["source"])).unwrap();
    let out = tmp.path().join("groups.out.m3u8");
    let name = rewrite_hls_master(&master, &out, None)
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_string());
    assert_eq!(gate_outcome(name), edges["groups"]["result"]);
    assert_eq!(read(&out), text(&edges["groups"]["output"]));
}

// ------------------------------------------------------------------------------------------
// The gates, through a double of the `media_inspect` functions (the generator's doubles)
// ------------------------------------------------------------------------------------------

/// The generator's `media_inspect` doubles: `same` decodes to `<mode>:<EXTINF count>`,
/// `differ` to `<mode>:<first segment>`, `runtime` raises `RuntimeError`.
struct MediaDouble {
    scenario: RefCell<&'static str>,
    durations: RefCell<Vec<f64>>,
}

impl MediaDouble {
    fn new() -> Self {
        MediaDouble {
            scenario: RefCell::new("same"),
            durations: RefCell::new(Vec::new()),
        }
    }
}

impl GateMedia for MediaDouble {
    fn compute_framemd5(&self, _path: &Path) -> Result<String, PyErr> {
        Ok(if *self.scenario.borrow() == "differ" {
            "framemd5:other"
        } else {
            "framemd5:3"
        }
        .to_string())
    }

    fn decode_hls_media_playlist(
        &self,
        localized: &Path,
        output: &Path,
        mode: &str,
    ) -> Result<PathBuf, PyErr> {
        if *self.scenario.borrow() == "runtime" {
            let name = localized.file_name().unwrap().to_str().unwrap();
            return Err(PyErr::Runtime(format!(
                "ffmpeg SAMPLE-AES decode ({mode}) failed for {name}"
            )));
        }
        let text = read(localized);
        let lines: Vec<&str> = text.split('\n').collect();
        let count = lines.iter().filter(|l| l.starts_with("#EXTINF:")).count();
        let first_uri = lines[1..]
            .iter()
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap();
        let payload = if *self.scenario.borrow() == "differ" {
            format!("{mode}:{first_uri}")
        } else {
            format!("{mode}:{count}")
        };
        std::fs::write(output, payload).unwrap();
        Ok(output.to_path_buf())
    }

    fn decode_media_to_pcm(&self, _input: &Path, output: &Path) -> Result<PathBuf, PyErr> {
        std::fs::write(output, "pcm:3").unwrap();
        Ok(output.to_path_buf())
    }

    fn localize_hls_media_playlist(
        &self,
        text: &str,
        key_file_path: &Path,
        resolve_uri: &dyn Fn(&str) -> Result<PathBuf, PyErr>,
    ) -> Result<String, PyErr> {
        let mut lines = Vec::new();
        for line in text.split('\n') {
            if !line.is_empty() && !line.starts_with('#') {
                lines.push(
                    resolve_uri(line)?
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string(),
                );
            } else {
                lines.push(line.to_string());
            }
        }
        let key_name = key_file_path.file_name().unwrap().to_str().unwrap();
        Ok(format!("LOCALIZED {key_name}\n{}", lines.join("\n")))
    }

    fn measure_segment_durations_s(
        &self,
        _init: &Path,
        _segments: &[PathBuf],
        _timescale: i64,
    ) -> Result<Vec<f64>, PyErr> {
        if *self.scenario.borrow() == "runtime" {
            return Err(PyErr::Runtime(
                "seg_2.m4s has no fragment (moof/tfdt/trun) metadata to measure".to_string(),
            ));
        }
        Ok(self.durations.borrow().clone())
    }

    fn write_raw_key_file(&self, key_hex: &str, dest: &Path) -> Result<PathBuf, PyErr> {
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, hex::decode(key_hex).unwrap()).unwrap();
        Ok(dest.to_path_buf())
    }
}

fn gate_outcome<T: serde::Serialize>(result: Result<T, HlsError>) -> Value {
    match result {
        Ok(value) => json!({ "ok": value }),
        Err(err) => {
            let (class, message) = class_and_message(&err);
            json!({ "error": [class, message] })
        }
    }
}

#[test]
fn gates_match_python_through_the_same_doubles() {
    let expected = &fixture("manifest_hls.json")["gates"];
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(&tmp.path().join("asset"));
    let by_uuid: BTreeMap<String, HlsVariantMapping> =
        map_hls_variants_to_dash(&asset.master_path, &asset.renditions)
            .unwrap()
            .into_iter()
            .map(|m| (uuid_of(&m.hls_playlist), m))
            .collect();
    let key = KeyMaterial::new(KID_HEX, "10".repeat(16), IV_HEX).unwrap();
    assert!(
        !format!("{key:?}").contains("1010"),
        "Debug never prints the key"
    );
    let media = MediaDouble::new();
    let pairs = find_audio_consolidation_pairs(&asset.master_path).unwrap();
    for name in ["same", "differ", "runtime"] {
        *media.scenario.borrow_mut() = name;
        let pcm: Result<Vec<String>, HlsError> = pairs
            .iter()
            .enumerate()
            .map(|(i, (a, b))| {
                audio_pcm_consolidation_gate(
                    a,
                    b,
                    &key,
                    &tmp.path().join(format!("pcm-{name}-{i}")),
                    &media,
                )
            })
            .collect();
        assert_eq!(
            gate_outcome(pcm),
            expected[format!("pcm_{name}")],
            "pcm {name}"
        );
        let content: Result<BTreeMap<String, String>, HlsError> =
            ["hv-h264-a", "hv-h265-a", "ha-fmp4-0"]
                .iter()
                .map(|uuid| {
                    let work = tmp.path().join(format!("content-{name}-{uuid}"));
                    content_identity_gate(
                        &by_uuid[*uuid],
                        &tmp.path().join("clear.mp4"),
                        &key,
                        &work,
                        &media,
                    )
                    .map(|status| ((*uuid).to_string(), status))
                })
                .collect();
        assert_eq!(
            gate_outcome(content),
            expected[format!("content_{name}")],
            "content {name}"
        );
    }
    let localized_dir = tmp.path().join("pcm-same-0/ts");
    assert_eq!(
        read(&localized_dir.join("localized.m3u8")),
        text(&expected["localized_ts"])
    );
    assert_eq!(
        hex::encode(std::fs::read(localized_dir.join("content.key")).unwrap()),
        text(&expected["key_file"])
    );
    *media.scenario.borrow_mut() = "same";
    let video = &by_uuid["hv-h265-a"];
    for (name, values) in [
        ("video_pass", vec![6.0, 6.0, 6.0]),
        ("video_drift", vec![6.0, 6.004, 5.9]),
        ("video_count", vec![6.0, 6.0]),
        ("video_nan", vec![f64::NAN, 6.0, 6.0]),
    ] {
        *media.durations.borrow_mut() = values;
        let status = check_extinf_gate(video, EXTINF_TOLERANCE_S, &media).unwrap();
        assert_eq!(status, text(&expected[format!("extinf_{name}")]), "{name}");
    }
    let audio_mapping = &by_uuid["ha-fmp4-0"];
    *media.durations.borrow_mut() = vec![5.994_666_666_666_667, 5.994_666_666_666_667, 6.016];
    assert_eq!(
        check_extinf_gate(audio_mapping, EXTINF_TOLERANCE_S, &media).unwrap(),
        text(&expected["extinf_audio_pass"])
    );
    assert_eq!(
        check_extinf_gate(audio_mapping, 1e-18, &media).unwrap(),
        text(&expected["extinf_audio_tight"])
    );
    *media.scenario.borrow_mut() = "runtime";
    let err = check_extinf_gate(audio_mapping, EXTINF_TOLERANCE_S, &media).unwrap_err();
    assert_eq!(
        class_and_message(&err),
        error_of(&expected["extinf_runtime"])
    );
}
