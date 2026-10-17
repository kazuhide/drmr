//! DASH protection rewrites and byte preservation using synthetic manifests.

#[path = "support/paths.rs"]
mod paths;
use paths::portable_path;

#[path = "manifest_fixture/common.rs"]
#[allow(
    dead_code,
    reason = "shared by the manifest test crates; each uses a subset"
)]
mod common;
#[path = "manifest_fixture/dash.rs"]
mod dash_fixture;

use common::{KID_UUID, error_of, fixture, playready, read, text};
use dash_fixture::{
    DashOptions, RepSpec, audio_rep, build_synthetic_dash_asset, fake_blobs, h264_rep, h265_rep,
    vp9_rep,
};
use drmr_core::checks::CheckId;
use drmr_core::manifest_dash::{
    DashError, DashManifestSummary, DashRepresentationSummary, DashRewriteResult,
    analyze_dash_manifest, localize_dash_manifest_http, parse_source_playready,
    rewrite_dash_manifest,
};
use drmr_core::manifest_support::xml::{self, Element};
use drmr_core::playready::PlayReadyObjects;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const MPD_NS: &str = "urn:mpeg:dash:schema:mpd:2011";
const CENC_NS: &str = "urn:mpeg:cenc:2013";
const MSPR_NS: &str = "urn:microsoft:playready";
const MP4PROT: &str = "urn:mpeg:dash:mp4protection:2011";
const PLAYREADY: &str = "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95";
const WIDEVINE: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";

// ------------------------------------------------------------------------------------------
// helpers: a whole-document, namespaced view of the WRITTEN output (independent of the
// module's own per-block extractors)
// ------------------------------------------------------------------------------------------

fn root(text: &str) -> Element {
    xml::fromstring(text).unwrap()
}

fn root_of(path: &Path) -> Element {
    root(&read(path))
}

fn children<'a>(element: &'a Element, ns: &str, local: &str) -> Vec<&'a Element> {
    let tag = format!("{{{ns}}}{local}");
    element
        .children
        .iter()
        .filter(|child| child.tag == tag)
        .collect()
}

fn descendants<'a>(element: &'a Element, ns: &str, local: &str) -> Vec<&'a Element> {
    let tag = format!("{{{ns}}}{local}");
    element
        .iter_tree()
        .skip(1)
        .filter(|child| child.tag == tag)
        .collect()
}

fn adaptation_sets(root: &Element) -> Vec<&Element> {
    descendants(root, MPD_NS, "AdaptationSet")
}

fn cp<'a>(adaptation_set: &'a Element, scheme: &str) -> Option<&'a Element> {
    children(adaptation_set, MPD_NS, "ContentProtection")
        .into_iter()
        .find(|cp| cp.get("schemeIdUri").unwrap_or("").to_lowercase() == scheme)
}

fn scheme_value(adaptation_set: &Element) -> Option<&str> {
    cp(adaptation_set, MP4PROT)?.get("value")
}

fn default_kid(adaptation_set: &Element) -> Option<&str> {
    cp(adaptation_set, MP4PROT)?.get(&format!("{{{CENC_NS}}}default_KID"))
}

fn child_text<'a>(element: &'a Element, ns: &str, local: &str) -> Option<&'a str> {
    children(element, ns, local).first()?.text.as_deref()
}

fn playready_pssh_pro(adaptation_set: &Element) -> (Option<&str>, Option<&str>) {
    match cp(adaptation_set, PLAYREADY) {
        None => (None, None),
        Some(cp) => (
            child_text(cp, CENC_NS, "pssh"),
            child_text(cp, MSPR_NS, "pro"),
        ),
    }
}

fn widevine_pssh(adaptation_set: &Element) -> Option<&str> {
    child_text(cp(adaptation_set, WIDEVINE)?, CENC_NS, "pssh")
}

fn is_vp9(adaptation_set: &Element) -> bool {
    adaptation_set.get("mimeType") == Some("video/webm")
}

fn segtpl_tuples(adaptation_set: &Element) -> Vec<[Option<String>; 5]> {
    descendants(adaptation_set, MPD_NS, "SegmentTemplate")
        .into_iter()
        .map(|st| {
            [
                "timescale",
                "duration",
                "media",
                "initialization",
                "startNumber",
            ]
            .map(|name| st.get(name).map(str::to_string))
        })
        .collect()
}

/// The raw `<AdaptationSet …>…</AdaptationSet>` of the first set with this mimeType.
fn adaptation_set_block<'a>(mpd_text: &'a str, mime_type: &str) -> &'a str {
    let pattern = regex::Regex::new(r"(?s)<AdaptationSet\b.*?</AdaptationSet>").unwrap();
    pattern
        .find_iter(mpd_text)
        .map(|found| found.as_str())
        .find(|block| block.contains(&format!("mimeType=\"{mime_type}\"")))
        .unwrap()
}

fn check(result: &DashRewriteResult, id: CheckId) -> &str {
    result
        .checks
        .iter()
        .find(|(check, _)| *check == id)
        .map(|(_, status)| status.as_str())
        .unwrap()
}

fn mixed() -> Vec<RepSpec> {
    vec![
        h264_rep("h264-lo"),
        h264_rep("h264-hi"),
        h265_rep("h265-lo"),
        h265_rep("h265-hi"),
        vp9_rep("vp9-lo"),
        vp9_rep("vp9-hi"),
        audio_rep("audio-lo"),
        audio_rep("audio-hi"),
    ]
}

struct Rewritten {
    source: PathBuf,
    out: PathBuf,
    result: DashRewriteResult,
    pr: PlayReadyObjects,
}

fn rewrite(tmp: &Path, reps: &[RepSpec], options: &DashOptions) -> Rewritten {
    let source = build_synthetic_dash_asset(tmp, reps, options);
    let out = tmp.join("output/manifests/manifest.mpd");
    let pr = playready();
    let result = rewrite_dash_manifest(&source, &out, &pr, None).unwrap();
    Rewritten {
        source,
        out,
        result,
        pr,
    }
}

fn class_and_message(err: &DashError) -> (String, String) {
    let class = match err {
        DashError::Rewrite(_) => "DashRewriteError",
        DashError::PlayReady(_) => "PlayReadyError",
        DashError::Py(inner) => inner.class_name(),
    };
    (class.to_string(), err.to_string())
}

fn result_json(result: &DashRewriteResult) -> Value {
    json!({
        "adaptation_sets_patched": result.adaptation_sets_patched,
        "playready_pssh_b64": result.playready_pssh_b64,
        "playready_pro_b64": result.playready_pro_b64,
        "widevine_unchanged": result.widevine_unchanged,
        "checks": result.checks.iter().map(|(id, status)| json!([id.as_str(), status])).collect::<Vec<_>>(),
    })
}

fn summary_json(summary: &DashManifestSummary) -> Value {
    let num = |value: &Option<num_bigint::BigInt>| value.as_ref().map(ToString::to_string);
    Value::Array(
        summary
            .representations
            .iter()
            .map(|r| {
                json!({
                    "rep_id": r.rep_id, "content_type": r.content_type, "codec": r.codec,
                    "mime_type": r.mime_type, "container": r.container, "in_scope": r.in_scope,
                    "protection_scheme": r.protection_scheme, "default_kid": r.default_kid,
                    "drm_systems": r.drm_systems, "playready_version": r.playready_version,
                    "playready_algid": r.playready_algid, "timescale": num(&r.timescale),
                    "segment_duration_ticks": num(&r.segment_duration_ticks),
                    "segment_duration_s": r.segment_duration_s.map(drmr_core::manifest_support::py::float_str),
                    "start_number": num(&r.start_number), "bandwidth": num(&r.bandwidth),
                })
            })
            .collect(),
    )
}

fn sha256(path: &Path) -> String {
    hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
}

// ------------------------------------------------------------------------------------------
//
// ------------------------------------------------------------------------------------------

#[test]
fn test_scheme_flips_cenc_to_cbcs_on_every_in_scope_set() {
    let tmp = tempfile::tempdir().unwrap();
    let run = rewrite(tmp.path(), &mixed(), &DashOptions::default());
    let (source, output) = (root_of(&run.source), root_of(&run.out));
    for (src, out) in adaptation_sets(&source)
        .iter()
        .zip(adaptation_sets(&output))
    {
        if is_vp9(out) {
            assert_eq!(scheme_value(out), Some("cenc"));
        } else {
            assert_eq!(scheme_value(src), Some("cenc"));
            assert_eq!(scheme_value(out), Some("cbcs"));
        }
        assert_eq!(default_kid(out), default_kid(src));
    }
    assert_eq!(run.result.adaptation_sets_patched, 6);
    assert_eq!(check(&run.result, CheckId::DashSchemeCbcs), "PASS");
}

// ------------------------------------------------------------------------------------------
//
// ------------------------------------------------------------------------------------------

#[test]
fn test_playready_pssh_and_pro_replaced_in_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let run = rewrite(tmp.path(), &mixed(), &DashOptions::default());
    let output = root_of(&run.out);
    let mut swapped = 0;
    for out in adaptation_sets(&output) {
        let (pssh, pro) = playready_pssh_pro(out);
        if is_vp9(out) {
            assert!(pssh.is_none() && pro.is_none());
            continue;
        }
        assert_eq!(pssh, Some(run.pr.pssh_b64.as_str()));
        assert_eq!(pro, Some(run.pr.pro_b64.as_str()));
        swapped += 1;
    }
    assert_eq!(swapped, 6);
    assert_eq!(run.result.playready_pssh_b64, run.pr.pssh_b64);
    assert_eq!(run.result.playready_pro_b64, run.pr.pro_b64);
    assert_eq!(check(&run.result, CheckId::DashPlayreadySync), "PASS");
}

#[test]
fn test_playready_pssh_and_pro_differ_from_each_other_and_widevine() {
    let tmp = tempfile::tempdir().unwrap();
    let run = rewrite(tmp.path(), &[h264_rep("v0")], &DashOptions::default());
    let output = root_of(&run.out);
    let out = adaptation_sets(&output)[0];
    let (pssh, pro) = playready_pssh_pro(out);
    assert_ne!(pssh, pro);
    assert_ne!(pssh, widevine_pssh(out));
    assert_ne!(pro, widevine_pssh(out));
}

// ------------------------------------------------------------------------------------------
// /  /
// ------------------------------------------------------------------------------------------

#[test]
fn test_widevine_pssh_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let run = rewrite(tmp.path(), &mixed(), &DashOptions::default());
    let (source, output) = (root_of(&run.source), root_of(&run.out));
    let fake = fake_blobs().widevine;
    for (src, out) in adaptation_sets(&source)
        .iter()
        .zip(adaptation_sets(&output))
    {
        assert_eq!(widevine_pssh(out), widevine_pssh(src));
        assert_eq!(widevine_pssh(out), Some(fake.as_str()));
    }
    assert!(run.result.widevine_unchanged);
    assert_eq!(check(&run.result, CheckId::DashWidevineUnchanged), "PASS");
}

#[test]
fn test_segment_template_attrs_byte_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let run = rewrite(tmp.path(), &mixed(), &DashOptions::default());
    let (source, output) = (root_of(&run.source), root_of(&run.out));
    for (src, out) in adaptation_sets(&source)
        .iter()
        .zip(adaptation_sets(&output))
    {
        assert_eq!(segtpl_tuples(out), segtpl_tuples(src));
    }
    assert_eq!(
        check(&run.result, CheckId::DashSegmenttemplateUnchanged),
        "PASS"
    );
}

#[test]
fn test_vp9_adaptationset_is_string_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let run = rewrite(tmp.path(), &mixed(), &DashOptions::default());
    let (source, output) = (read(&run.source), read(&run.out));
    assert_eq!(
        adaptation_set_block(&output, "video/webm"),
        adaptation_set_block(&source, "video/webm")
    );
}

#[test]
fn test_deploy_form_changes_only_drm_descriptors() {
    let tmp = tempfile::tempdir().unwrap();
    let run = rewrite(tmp.path(), &mixed(), &DashOptions::default());
    let (source, output) = (read(&run.source), read(&run.out));
    let blobs = fake_blobs();
    let restored = output
        .replace("value=\"cbcs\"", "value=\"cenc\"")
        .replace(&run.pr.pssh_b64, &blobs.playready_pssh)
        .replace(&run.pr.pro_b64, &blobs.playready_pro);
    assert_eq!(restored, source);
    assert_ne!(output, source);
}

#[test]
fn test_out_of_scope_untouched_check_passes() {
    let tmp = tempfile::tempdir().unwrap();
    let run = rewrite(tmp.path(), &mixed(), &DashOptions::default());
    assert_eq!(check(&run.result, CheckId::DashOutOfScopeUntouched), "PASS");
}

#[test]
fn test_av1_in_mp4_is_out_of_scope_and_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let av1 = RepSpec::new("av1-0", "av01.0.04M.08", "video", "video/mp4");
    let run = rewrite(
        tmp.path(),
        &[h264_rep("h264-0"), av1],
        &DashOptions::default(),
    );
    let output = root_of(&run.out);
    let by_id = |id: &str| {
        adaptation_sets(&output)
            .into_iter()
            .find(|set| {
                children(set, MPD_NS, "Representation")
                    .iter()
                    .any(|rep| rep.get("id") == Some(id))
            })
            .unwrap()
            .clone()
    };
    assert_eq!(scheme_value(&by_id("h264-0")), Some("cbcs"));
    assert_eq!(scheme_value(&by_id("av1-0")), Some("cenc"));
    assert_eq!(run.result.adaptation_sets_patched, 1);
}

// ------------------------------------------------------------------------------------------
//
// ------------------------------------------------------------------------------------------

#[test]
fn test_dynamic_enumeration_no_hardcoded_counts() {
    let cases: Vec<(Vec<RepSpec>, usize)> = vec![
        (vec![h264_rep("v0")], 1),
        (vec![h264_rep("v0"), audio_rep("a0")], 2),
        (
            (0..5)
                .map(|i| h264_rep(&format!("v{i}")))
                .chain([audio_rep("a0")])
                .collect(),
            6,
        ),
        (vec![vp9_rep("w0"), vp9_rep("w1")], 0),
        (
            vec![
                h264_rep("v0"),
                h265_rep("v1"),
                vp9_rep("w0"),
                vp9_rep("w1"),
                vp9_rep("w2"),
                audio_rep("a0"),
            ],
            3,
        ),
    ];
    let expected = fixture("manifest_dash.json");
    for (reps, in_scope) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let title = format!("dyn-{in_scope}-{}", reps.len());
        let run = rewrite(tmp.path(), &reps, &DashOptions::titled(&title));
        assert_eq!(run.result.adaptation_sets_patched, in_scope);
        for out in adaptation_sets(&root_of(&run.out)) {
            let want = if is_vp9(out) { "cenc" } else { "cbcs" };
            assert_eq!(scheme_value(out), Some(want));
        }
        // The same bytes and result as Python's for this shape.
        let case = &expected["dyn"][&title];
        assert_eq!(read(&run.out), text(&case["deploy"]), "{title}");
        assert_eq!(result_json(&run.result), case["result"], "{title}");
    }
}

// ------------------------------------------------------------------------------------------
//
// ------------------------------------------------------------------------------------------

#[test]
fn test_rerun_on_own_output_is_byte_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(tmp.path(), &mixed(), &DashOptions::titled("idem"));
    let pr = playready();
    let (out1, out2) = (tmp.path().join("out1.mpd"), tmp.path().join("out2.mpd"));
    let r1 = rewrite_dash_manifest(&source, &out1, &pr, None).unwrap();
    let r2 = rewrite_dash_manifest(&out1, &out2, &pr, None).unwrap();
    assert_eq!(std::fs::read(&out2).unwrap(), std::fs::read(&out1).unwrap());
    assert_eq!(r2.adaptation_sets_patched, r1.adaptation_sets_patched);
    assert_eq!(r2.checks, r1.checks);
    assert_eq!(read(&out2).matches("value=\"cbcs\"").count(), 6);
}

#[test]
fn test_rerun_with_local_form_is_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let source =
        build_synthetic_dash_asset(tmp.path(), &mixed(), &DashOptions::titled("idem-local"));
    let pr = playready();
    let (out1, out2) = (tmp.path().join("out1.mpd"), tmp.path().join("out2.mpd"));
    rewrite_dash_manifest(&source, &out1, &pr, Some("../../cbcs/")).unwrap();
    rewrite_dash_manifest(&out1, &out2, &pr, Some("../../cbcs/")).unwrap();
    assert_eq!(std::fs::read(&out2).unwrap(), std::fs::read(&out1).unwrap());
}

// ------------------------------------------------------------------------------------------
// url_base
// ------------------------------------------------------------------------------------------

#[test]
fn test_url_base_none_leaves_baseurl_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(tmp.path(), &[h264_rep("v0")], &DashOptions::default());
    let out = tmp.path().join("deploy.mpd");
    rewrite_dash_manifest(&source, &out, &playready(), None).unwrap();
    let output = root_of(&out);
    let base = children(&output, MPD_NS, "BaseURL");
    assert_eq!(
        base[0].text.as_deref(),
        Some("https://cdn.example.test/title-abc123/")
    );
}

#[test]
fn test_url_base_set_rewrites_baseurl_only() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(tmp.path(), &mixed(), &DashOptions::titled("local"));
    let out = tmp.path().join("local.mpd");
    rewrite_dash_manifest(&source, &out, &playready(), Some("../../cbcs/")).unwrap();
    let (src_root, out_root) = (root_of(&source), root_of(&out));
    assert_eq!(
        children(&out_root, MPD_NS, "BaseURL")[0].text.as_deref(),
        Some("../../cbcs/")
    );
    for (src, out) in adaptation_sets(&src_root)
        .iter()
        .zip(adaptation_sets(&out_root))
    {
        assert_eq!(segtpl_tuples(out), segtpl_tuples(src));
    }
    assert_eq!(
        adaptation_set_block(&read(&out), "video/webm"),
        adaptation_set_block(&read(&source), "video/webm")
    );
}

#[test]
fn test_url_base_injected_when_source_has_no_baseurl() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(
        tmp.path(),
        &[h264_rep("v0")],
        &DashOptions::titled("nobase"),
    );
    let stripped = regex::Regex::new(r"(?s)<BaseURL>.*?</BaseURL>")
        .unwrap()
        .replace_all(&read(&source), "")
        .into_owned();
    let no_base = tmp.path().join("nobase.mpd");
    std::fs::write(&no_base, stripped).unwrap();
    let out = tmp.path().join("injected.mpd");
    rewrite_dash_manifest(&no_base, &out, &playready(), Some("/served/cbcs/")).unwrap();
    let output = root_of(&out);
    assert_eq!(
        children(&output, MPD_NS, "BaseURL")[0].text.as_deref(),
        Some("/served/cbcs/")
    );
}

// ------------------------------------------------------------------------------------------
// localize_dash_manifest_http
// ------------------------------------------------------------------------------------------

#[test]
fn test_localize_dash_http_rewrites_baseurl_only() {
    let tmp = tempfile::tempdir().unwrap();
    let source =
        build_synthetic_dash_asset(tmp.path(), &mixed(), &DashOptions::titled("localize-http"));
    let out_text = localize_dash_manifest_http(&read(&source), |old| {
        format!("/media/{}", old.split_once("://").unwrap().1)
    })
    .unwrap();
    let output = root(&out_text);
    assert_eq!(
        children(&output, MPD_NS, "BaseURL")[0].text.as_deref(),
        Some("/media/cdn.example.test/localize-http/")
    );
}

#[test]
fn test_localize_dash_http_passes_current_baseurl_to_resolver() {
    let tmp = tempfile::tempdir().unwrap();
    let options = DashOptions {
        cdn_host: "cdn.example.test".to_string(),
        ..DashOptions::titled("t1")
    };
    let source = build_synthetic_dash_asset(tmp.path(), &[h264_rep("v0")], &options);
    let mut seen = Vec::new();
    localize_dash_manifest_http(&read(&source), |old| {
        seen.push(old.to_string());
        "/media/local-root/".to_string()
    })
    .unwrap();
    assert_eq!(seen, vec!["https://cdn.example.test/t1/".to_string()]);
}

#[test]
fn test_localize_dash_http_never_touches_drm_descriptors() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(
        tmp.path(),
        &mixed(),
        &DashOptions::titled("localize-http-2"),
    );
    let src_text = read(&source);
    let out_text = localize_dash_manifest_http(&src_text, |_| "/media/x/".to_string()).unwrap();
    let (src_root, out_root) = (root(&src_text), root(&out_text));
    for (src, out) in adaptation_sets(&src_root)
        .iter()
        .zip(adaptation_sets(&out_root))
    {
        let src_cp = descendants(src, MPD_NS, "ContentProtection")
            .into_iter()
            .find(|cp| cp.get("schemeIdUri") == Some(MP4PROT))
            .unwrap();
        let out_cp = descendants(out, MPD_NS, "ContentProtection")
            .into_iter()
            .find(|cp| cp.get("schemeIdUri") == Some(MP4PROT))
            .unwrap();
        assert_eq!(out_cp.get("value"), src_cp.get("value"));
    }
}

#[test]
fn test_localize_dash_http_writes_nothing_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(
        tmp.path(),
        &[h264_rep("v0")],
        &DashOptions::titled("readonly-check"),
    );
    let before = std::fs::read(&source).unwrap();
    localize_dash_manifest_http(&read(&source), |_| "/media/x/".to_string()).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
}

#[test]
fn test_localize_dash_http_raises_on_missing_baseurl() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(
        tmp.path(),
        &[h264_rep("v0")],
        &DashOptions::titled("nobase2"),
    );
    let stripped = regex::Regex::new(r"(?s)<BaseURL>.*?</BaseURL>")
        .unwrap()
        .replace_all(&read(&source), "")
        .into_owned();
    let err = localize_dash_manifest_http(&stripped, |_| "/media/x/".to_string()).unwrap_err();
    assert!(matches!(err, DashError::Rewrite(_)));
}

#[test]
fn test_localize_dash_http_rejects_doctype() {
    let malicious = "<?xml version=\"1.0\"?><!DOCTYPE MPD [<!ENTITY x \"y\">]><MPD><BaseURL>https://x/</BaseURL></MPD>";
    let err = localize_dash_manifest_http(malicious, |_| "/media/x/".to_string()).unwrap_err();
    assert!(matches!(err, DashError::Rewrite(_)));
}

// ------------------------------------------------------------------------------------------
// parse_source_playready
// ------------------------------------------------------------------------------------------

#[test]
fn test_parse_source_playready_reads_mspr_pro() {
    let tmp = tempfile::tempdir().unwrap();
    let pr = playready();
    let options = DashOptions {
        playready_pssh_b64: Some(pr.pssh_b64.clone()),
        playready_pro_b64: Some(pr.pro_b64.clone()),
        ..DashOptions::default()
    };
    let source =
        build_synthetic_dash_asset(tmp.path(), &[h264_rep("v0"), audio_rep("a0")], &options);
    let parsed = parse_source_playready(&source).unwrap();
    assert_eq!(parsed.version, "4.3.0.0");
    assert_eq!(
        parsed.la_url.as_deref(),
        Some("https://license.example.invalid/rightsmanager.asmx")
    );
    assert_eq!(parsed.algid.as_deref(), Some("AESCBC"));
    // Everything else Python recovered from the same PRO.
    let expected = &fixture("manifest_dash.json")["real_pr"];
    assert_eq!(read(&source), text(&expected["source"]));
    let want = &expected["parsed"];
    assert_eq!(
        json!(parsed.kid_values_b64),
        want["kid_values_b64"],
        "KID VALUEs"
    );
    assert_eq!(
        json!(parsed.has_custom_attributes),
        want["has_custom_attributes"]
    );
    assert_eq!(parsed.wrmheader_xml, text(&want["wrmheader_xml"]));
}

#[test]
fn test_parse_source_playready_raises_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let source =
        build_synthetic_dash_asset(tmp.path(), &[vp9_rep("w0")], &DashOptions::titled("no-pr"));
    let err = parse_source_playready(&source).unwrap_err();
    assert!(matches!(err, DashError::Rewrite(_)));
}

#[test]
fn custom_playready_metadata_is_rejected_before_rewriting() {
    use base64::Engine as _;
    use drmr_core::playready::build_pro;

    for metadata in [
        "<CUSTOMATTRIBUTES><VALUE>synthetic-private-value</VALUE></CUSTOMATTRIBUTES>",
        "<DS_ID>synthetic-private-value</DS_ID>",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let pr = playready();
        let header = pr
            .wrmheader_xml
            .replace("</DATA>", &format!("{metadata}</DATA>"));
        let custom_pro =
            base64::engine::general_purpose::STANDARD.encode(build_pro(header.as_str()).unwrap());
        let source = build_synthetic_dash_asset(
            tmp.path(),
            &[h264_rep("v0"), audio_rep("a0")],
            &DashOptions {
                playready_pssh_b64: Some(pr.pssh_b64.clone()),
                playready_pro_b64: Some(custom_pro.clone()),
                ..DashOptions::default()
            },
        );
        // A clean first header must not hide unsupported metadata on a later adaptation.
        std::fs::write(&source, read(&source).replacen(&custom_pro, &pr.pro_b64, 1)).unwrap();
        let output = tmp.path().join("new-output/manifest.mpd");
        assert_eq!(
            parse_source_playready(&source).unwrap_err().to_string(),
            "unsupported PlayReady custom attributes or DS_ID"
        );
        let error = rewrite_dash_manifest(&source, &output, &pr, None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unsupported PlayReady custom attributes or DS_ID"
        );
        assert!(!error.to_string().contains("synthetic-private-value"));
        assert!(!output.parent().unwrap().exists());
    }
}

#[test]
fn mixed_codec_adaptation_sets_are_rejected_before_rewriting_or_qualifying() {
    for codec in ["av01.0.04M.08", "hev1.1.6.L60.90"] {
        let tmp = tempfile::tempdir().unwrap();
        let pr = playready();
        let source =
            build_synthetic_dash_asset(tmp.path(), &[h264_rep("v0")], &DashOptions::default());
        let original = read(&source);
        let mixed = original.replace("</AdaptationSet>", &format!("<Representation id=\"unsupported\" codecs=\"{codec}\"><SegmentTemplate/></Representation></AdaptationSet>"));
        std::fs::write(&source, &mixed).unwrap();
        let output = tmp.path().join("new-output/manifest.mpd");
        let error = rewrite_dash_manifest(&source, &output, &pr, None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "mixed supported and unsupported codecs in one AdaptationSet"
        );
        assert!(!output.parent().unwrap().exists());
        assert!(analyze_dash_manifest(&source).is_err());
        assert_eq!(read(&source), mixed);
    }
}

// ------------------------------------------------------------------------------------------
// security / read-only / hygiene
// ------------------------------------------------------------------------------------------

fn poisoned_doctype(text: &str) -> String {
    text.replace(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE MPD [<!ENTITY lol \"lol\">]>",
    )
}

#[test]
fn test_rewrite_rejects_doctype() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(
        tmp.path(),
        &[h264_rep("v0")],
        &DashOptions::titled("doctype"),
    );
    let bad = tmp.path().join("bad.mpd");
    std::fs::write(&bad, poisoned_doctype(&read(&source))).unwrap();
    let err =
        rewrite_dash_manifest(&bad, &tmp.path().join("o.mpd"), &playready(), None).unwrap_err();
    assert!(matches!(err, DashError::Rewrite(_)) && err.to_string().contains("DOCTYPE"));
}

#[test]
fn test_input_file_is_not_modified() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(tmp.path(), &mixed(), &DashOptions::titled("ro"));
    let before = sha256(&source);
    rewrite_dash_manifest(
        &source,
        &tmp.path().join("o.mpd"),
        &playready(),
        Some("../cbcs/"),
    )
    .unwrap();
    assert_eq!(sha256(&source), before);
}

#[test]
fn test_fails_loud_on_unexpected_mp4protection_value() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(
        tmp.path(),
        &[h264_rep("v0")],
        &DashOptions::titled("bogus-scheme"),
    );
    let bad = tmp.path().join("bad.mpd");
    std::fs::write(
        &bad,
        read(&source).replace("value=\"cenc\"", "value=\"bogus\""),
    )
    .unwrap();
    let err =
        rewrite_dash_manifest(&bad, &tmp.path().join("o.mpd"), &playready(), None).unwrap_err();
    assert!(matches!(err, DashError::Rewrite(_)) && err.to_string().contains("mp4protection"));
}

#[test]
fn test_fails_loud_on_playready_missing_pro() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(
        tmp.path(),
        &[h264_rep("v0")],
        &DashOptions::titled("half-pr"),
    );
    let mangled = regex::Regex::new(r"<mspr:pro[^>]*>.*?</mspr:pro>")
        .unwrap()
        .replace_all(&read(&source), "")
        .into_owned();
    let bad = tmp.path().join("bad.mpd");
    std::fs::write(&bad, mangled).unwrap();
    let err =
        rewrite_dash_manifest(&bad, &tmp.path().join("o.mpd"), &playready(), None).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("mspr:pro") || message.contains("cenc:pssh"));
}

/// The equivalent spelling with the DASH namespace on an explicit `mpd:` prefix.
fn prefix_dash_namespace(mpd_text: &str) -> String {
    let mut text = mpd_text.replace(
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"",
        "<MPD xmlns:mpd=\"urn:mpeg:dash:schema:mpd:2011\"",
    );
    for tag in [
        "Period",
        "AdaptationSet",
        "Representation",
        "SegmentTemplate",
        "BaseURL",
        "ContentProtection",
    ] {
        text = text
            .replace(&format!("<{tag}"), &format!("<mpd:{tag}"))
            .replace(&format!("</{tag}>"), &format!("</mpd:{tag}>"));
    }
    text
}

#[test]
fn test_namespace_prefixed_mpd_fails_loud_not_silent_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(tmp.path(), &mixed(), &DashOptions::titled("prefixed"));
    let prefixed = prefix_dash_namespace(&read(&source));
    let src = tmp.path().join("prefixed.mpd");
    std::fs::write(&src, &prefixed).unwrap();
    assert!(prefixed.contains("<mpd:AdaptationSet"));
    assert!(
        !regex::Regex::new(r"<AdaptationSet\b")
            .unwrap()
            .is_match(&prefixed)
    );
    assert_eq!(adaptation_sets(&root(&prefixed)).len(), 8);
    let out = tmp.path().join("o.mpd");
    let err = rewrite_dash_manifest(&src, &out, &playready(), None).unwrap_err();
    assert!(err.to_string().contains("parsed manifest contains"));
    assert!(!out.exists());
}

#[test]
fn test_no_key_or_iv_in_result_or_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let run = rewrite(tmp.path(), &[h264_rep("v0")], &DashOptions::default());
    // The field set is pinned by this exhaustive pattern: a new field fails to compile here,
    // and none of these names is key- or IV-like.
    let DashRewriteResult {
        output_mpd_path: _,
        adaptation_sets_patched: _,
        playready_pssh_b64: _,
        playready_pro_b64: _,
        widevine_unchanged: _,
        checks: _,
    } = run.result;
    let text = read(&run.out);
    assert!(!text.contains("DRM_KEY") && !text.contains("DRM_IV"));
}

// ------------------------------------------------------------------------------------------
// analyze_dash_manifest
// ------------------------------------------------------------------------------------------

#[test]
fn test_analyze_dash_manifest_reports_in_scope_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let pr = playready();
    let options = DashOptions {
        playready_pssh_b64: Some(pr.pssh_b64.clone()),
        playready_pro_b64: Some(pr.pro_b64.clone()),
        ..DashOptions::default()
    };
    let source =
        build_synthetic_dash_asset(tmp.path(), &[h264_rep("v0"), audio_rep("a0")], &options);
    let summary = analyze_dash_manifest(&source).unwrap();
    let by_id = |id: &str| {
        summary
            .representations
            .iter()
            .find(|r| r.rep_id == id)
            .unwrap()
    };
    assert_eq!(summary.representations.len(), 2);
    let video = by_id("v0");
    assert_eq!(video.content_type, "video");
    assert_eq!(video.codec, "avc1.42C00C");
    assert_eq!(video.mime_type, "video/mp4");
    assert_eq!(video.container, "fmp4");
    assert!(video.in_scope);
    assert_eq!(video.protection_scheme.as_deref(), Some("cenc"));
    assert_eq!(video.default_kid.as_deref(), Some(KID_UUID));
    assert_eq!(video.drm_systems, vec!["PlayReady", "Widevine"]);
    assert_eq!(video.playready_version.as_deref(), Some("4.3.0.0"));
    assert_eq!(video.playready_algid.as_deref(), Some("AESCBC"));
    assert_eq!(video.timescale, Some(24000.into()));
    assert_eq!(video.segment_duration_ticks, Some(144_000.into()));
    assert_eq!(video.segment_duration_s, Some(6.0));
    assert_eq!(video.start_number, Some(0.into()));
    let audio = by_id("a0");
    assert_eq!(audio.content_type, "audio");
    assert_eq!(audio.codec, "mp4a.40.2");
    assert_eq!(audio.mime_type, "audio/mp4");
    assert_eq!(audio.container, "fmp4");
    assert!(audio.in_scope);
    assert_eq!(audio.timescale, Some(48000.into()));
    assert_eq!(audio.segment_duration_ticks, Some(288_000.into()));
    assert_eq!(audio.segment_duration_s, Some(6.0));
    assert_eq!(audio.start_number, Some(0.into()));
    assert_eq!(
        summary_json(&summary),
        fixture("manifest_dash.json")["real_pr"]["summary"]
    );
}

#[test]
fn test_analyze_dash_manifest_includes_out_of_scope_vp9() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(
        tmp.path(),
        &[h264_rep("v0"), vp9_rep("w0")],
        &DashOptions::default(),
    );
    let summary = analyze_dash_manifest(&source).unwrap();
    let vp9 = summary
        .representations
        .iter()
        .find(|r| r.rep_id == "w0")
        .unwrap();
    assert!(!vp9.in_scope);
    assert_eq!(vp9.mime_type, "video/webm");
    assert_eq!(vp9.container, "webm");
    assert_eq!(vp9.protection_scheme.as_deref(), Some("cenc"));
    assert_eq!(vp9.drm_systems, vec!["Widevine"]);
    assert_eq!(vp9.playready_version, None);
    assert_eq!(vp9.playready_algid, None);
    assert!(
        summary
            .representations
            .iter()
            .find(|r| r.rep_id == "v0")
            .unwrap()
            .in_scope
    );
}

#[test]
fn test_analyze_dash_manifest_playready_parse_failure_degrades_to_none() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(tmp.path(), &[h264_rep("v0")], &DashOptions::default());
    let summary = analyze_dash_manifest(&source).unwrap();
    assert_eq!(summary.representations.len(), 1);
    let rep = &summary.representations[0];
    assert_eq!(rep.playready_version, None);
    assert_eq!(rep.playready_algid, None);
    assert_eq!(rep.default_kid.as_deref(), Some(KID_UUID));
    assert_eq!(rep.protection_scheme.as_deref(), Some("cenc"));
}

#[test]
fn test_analyze_dash_manifest_does_not_touch_filesystem_beyond_the_manifest() {
    // Invalid rendition enumeration for this input
    // is covered by configuration tests.
    let tmp = tempfile::tempdir().unwrap();
    let built = build_synthetic_dash_asset(
        &tmp.path().join("source"),
        &[h264_rep("v0"), audio_rep("a0")],
        &DashOptions::default(),
    );
    let deploy_dir = tmp.path().join("deploy-form-only/nested");
    std::fs::create_dir_all(&deploy_dir).unwrap();
    let deploy_mpd = deploy_dir.join("manifest.mpd");
    std::fs::write(&deploy_mpd, read(&built)).unwrap();
    let summary = analyze_dash_manifest(&deploy_mpd).unwrap();
    let ids: Vec<&str> = summary
        .representations
        .iter()
        .map(|r| r.rep_id.as_str())
        .collect();
    assert_eq!(ids, vec!["v0", "a0"]);
    assert!(summary.representations.iter().all(|r| r.in_scope));
}

#[test]
fn test_analyze_dash_manifest_rejects_doctype() {
    let tmp = tempfile::tempdir().unwrap();
    let source = build_synthetic_dash_asset(
        tmp.path(),
        &[h264_rep("v0")],
        &DashOptions::titled("analyze-doctype"),
    );
    let bad = tmp.path().join("bad-analyze.mpd");
    std::fs::write(&bad, poisoned_doctype(&read(&source))).unwrap();
    let err = analyze_dash_manifest(&bad).unwrap_err();
    assert!(matches!(err, DashError::Rewrite(_)) && err.to_string().contains("DOCTYPE"));
}

#[test]
fn test_analyze_dash_manifest_result_has_no_key_or_iv_fields() {
    // Exhaustive patterns pin the field sets; none of the names is key- or IV-like.
    let check_row = |row: DashRepresentationSummary| {
        let DashRepresentationSummary {
            rep_id: _,
            content_type: _,
            codec: _,
            mime_type: _,
            container: _,
            in_scope: _,
            protection_scheme: _,
            default_kid: _,
            drm_systems: _,
            playready_version: _,
            playready_algid: _,
            timescale: _,
            segment_duration_ticks: _,
            segment_duration_s: _,
            start_number: _,
            bandwidth: _,
        } = row;
    };
    let DashManifestSummary { representations } = DashManifestSummary {
        representations: Vec::new(),
    };
    representations.into_iter().for_each(check_row);
}

// ------------------------------------------------------------------------------------------
// Byte-for-byte against the committed reference fixtures
// ------------------------------------------------------------------------------------------

#[test]
fn mixed_asset_bytes_results_and_summary_match_python() {
    let expected = &fixture("manifest_dash.json")["mixed"];
    let tmp = tempfile::tempdir().unwrap();
    let source =
        build_synthetic_dash_asset(&tmp.path().join("mixed"), &mixed(), &DashOptions::default());
    assert_eq!(read(&source), text(&expected["source"]), "the builder");
    let pr = playready();
    let out = tmp.path().join("out");
    let deploy = rewrite_dash_manifest(&source, &out.join("deploy.mpd"), &pr, None).unwrap();
    let local =
        rewrite_dash_manifest(&source, &out.join("local.mpd"), &pr, Some("../../cbcs/")).unwrap();
    let again =
        rewrite_dash_manifest(&out.join("deploy.mpd"), &out.join("again.mpd"), &pr, None).unwrap();
    assert_eq!(read(&out.join("deploy.mpd")), text(&expected["deploy"]));
    assert_eq!(read(&out.join("local.mpd")), text(&expected["local"]));
    assert_eq!(read(&out.join("again.mpd")), text(&expected["rerun"]));
    assert_eq!(result_json(&deploy), expected["result"]);
    assert_eq!(result_json(&local), expected["local_result"]);
    assert_eq!(result_json(&again), expected["rerun_result"]);
    assert_eq!(deploy.output_mpd_path, out.join("deploy.mpd"));
    let localized = localize_dash_manifest_http(&read(&source), |old| {
        format!("/media/{}", old.split_once("://").unwrap().1)
    })
    .unwrap();
    assert_eq!(localized, text(&expected["localized"]));
    assert_eq!(
        summary_json(&analyze_dash_manifest(&source).unwrap()),
        expected["summary"]
    );
}

#[test]
fn injected_and_lenient_cases_match_python() {
    let cases = fixture("manifest_dash.json");
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("nobase.mpd");
    std::fs::write(&source, text(&cases["injected"]["source"])).unwrap();
    let out = tmp.path().join("injected.mpd");
    rewrite_dash_manifest(&source, &out, &playready(), Some("/served/cbcs/&<x>")).unwrap();
    assert_eq!(read(&out), text(&cases["injected"]["output"]));
    let lenient = tmp.path().join("lenient.mpd");
    std::fs::write(&lenient, text(&cases["lenient"]["source"])).unwrap();
    assert_eq!(
        summary_json(&analyze_dash_manifest(&lenient).unwrap()),
        cases["lenient"]["summary"]
    );
}

/// The generator's `single` asset (one H.264 rendition, title `single`) as text.
fn single_text(tmp: &Path) -> String {
    read(&build_synthetic_dash_asset(
        &tmp.join("single"),
        &[h264_rep("v0")],
        &DashOptions::titled("single"),
    ))
}

fn without_base_url(text: &str) -> String {
    regex::Regex::new(r"(?s)<BaseURL>.*?</BaseURL>")
        .unwrap()
        .replace_all(text, "")
        .into_owned()
}

/// The failing inputs of the generator, rebuilt from the `single` asset, with Python's class
/// and `str(exc)` for each.
#[test]
fn rewrite_errors_match_python() {
    let cases = fixture("manifest_dash.json");
    let errors = &cases["errors"];
    let tmp = tempfile::tempdir().unwrap();
    let single_text = single_text(tmp.path());
    let pro_re = regex::Regex::new(r"<mspr:pro[^>]*>.*?</mspr:pro>").unwrap();
    let root_cenc = single_text
        .replace(" xmlns:cenc=\"urn:mpeg:cenc:2013\"", "")
        .replace(
            "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"",
            "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" xmlns:cenc=\"urn:mpeg:cenc:2013\"",
        );
    let mixed_source =
        build_synthetic_dash_asset(&tmp.path().join("mixed"), &mixed(), &DashOptions::default());
    let rewrite_cases: Vec<(&str, String)> = vec![
        ("doctype", poisoned_doctype(&single_text)),
        (
            "bogus_scheme",
            single_text.replace("value=\"cenc\"", "value=\"bogus\""),
        ),
        ("missing_value", single_text.replace(" value=\"cenc\"", "")),
        (
            "missing_pro",
            pro_re.replace_all(&single_text, "").into_owned(),
        ),
        (
            "missing_mp4protection",
            single_text.replace(
                "schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\"",
                "schemeIdUri=\"urn:example:other\"",
            ),
        ),
        (
            "two_cenc_values",
            single_text.replacen(
                "<Representation",
                "<Label value=\"cenc\">x</Label><Representation",
                1,
            ),
        ),
        ("prefixed", prefix_dash_namespace(&read(&mixed_source))),
        ("root_declared_cenc", root_cenc),
        (
            "malformed_block",
            single_text.replacen("</Representation>", "</Representation></Bad>", 1),
        ),
        ("unclosed", single_text.replace("</AdaptationSet>", "")),
    ];
    for (name, body) in rewrite_cases {
        let bad = tmp.path().join(format!("{name}.mpd"));
        std::fs::write(&bad, body).unwrap();
        let out = tmp.path().join(format!("{name}.out.mpd"));
        let err = rewrite_dash_manifest(&bad, &out, &playready(), None).unwrap_err();
        assert_eq!(class_and_message(&err), error_of(&errors[name]), "{name}");
    }
}

#[test]
fn analyze_and_parse_errors_match_python() {
    let cases = fixture("manifest_dash.json");
    let errors = &cases["errors"];
    let tmp = tempfile::tempdir().unwrap();
    let single_text = single_text(tmp.path());
    let pro_text_re = regex::Regex::new(r"(<mspr:pro[^>]*>).*?(</mspr:pro>)").unwrap();
    let analyze_cases: Vec<(&str, String)> = vec![
        ("doctype_analyze", poisoned_doctype(&single_text)),
        (
            "analyze_bad_timescale",
            single_text.replace("timescale=\"24000\"", "timescale=\"x\""),
        ),
        (
            "analyze_missing_content_type",
            single_text.replace(" contentType=\"video\"", ""),
        ),
        (
            "analyze_superscript_bandwidth",
            single_text.replace("bandwidth=\"1\"", "bandwidth=\"\u{b2}\""),
        ),
    ];
    for (name, body) in analyze_cases {
        let bad = tmp.path().join(format!("{name}.mpd"));
        std::fs::write(&bad, body).unwrap();
        let err = analyze_dash_manifest(&bad).unwrap_err();
        let (class, message) = error_of(&errors[name]);
        let message = message.replace("{path}", &portable_path(&bad));
        assert_eq!(class_and_message(&err), (class, message), "{name}");
    }
    let parse_cases: Vec<(&str, String)> = vec![
        (
            "empty_pro",
            pro_text_re.replace_all(&single_text, "$1 $2").into_owned(),
        ),
        ("bad_pro", single_text.clone()),
    ];
    for (name, body) in parse_cases {
        let bad = tmp.path().join(format!("{name}.mpd"));
        std::fs::write(&bad, body).unwrap();
        let err = parse_source_playready(&bad).unwrap_err();
        assert_eq!(class_and_message(&err), error_of(&errors[name]), "{name}");
    }
    let vp9_only = build_synthetic_dash_asset(
        &tmp.path().join("vp9"),
        &[vp9_rep("w0")],
        &DashOptions::titled("no-pr"),
    );
    let err = parse_source_playready(&vp9_only).unwrap_err();
    assert_eq!(class_and_message(&err), error_of(&errors["no_playready"]));
}

#[test]
fn localize_and_url_base_errors_match_python() {
    let cases = fixture("manifest_dash.json");
    let errors = &cases["errors"];
    let tmp = tempfile::tempdir().unwrap();
    let nobase = without_base_url(&single_text(tmp.path()));
    let err = localize_dash_manifest_http(&nobase, str::to_string).unwrap_err();
    assert_eq!(
        class_and_message(&err),
        error_of(&errors["localize_missing_baseurl"])
    );
    let doctype = "<?xml version=\"1.0\"?><!DOCTYPE MPD [<!ENTITY x \"y\">]><MPD><BaseURL>https://x/</BaseURL></MPD>";
    let err = localize_dash_manifest_http(doctype, str::to_string).unwrap_err();
    assert_eq!(
        class_and_message(&err),
        error_of(&errors["localize_doctype"])
    );
    // `_apply_url_base` without a BaseURL nor an MPD element, through the public path.
    let no_mpd = tmp.path().join("no-mpd.xml");
    std::fs::write(&no_mpd, "<NoMpd/>").unwrap();
    let err = rewrite_dash_manifest(
        &no_mpd,
        &tmp.path().join("x.mpd"),
        &playready(),
        Some("/x/"),
    )
    .unwrap_err();
    assert_eq!(
        class_and_message(&err),
        error_of(&errors["injected_no_mpd"])
    );
}

#[test]
fn a_missing_source_is_an_os_error_and_a_non_utf8_source_a_unicode_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing.mpd");
    let err =
        rewrite_dash_manifest(&missing, &tmp.path().join("o.mpd"), &playready(), None).unwrap_err();
    assert_eq!(class_and_message(&err).0, "OSError");
    #[cfg(unix)]
    assert!(
        err.to_string()
            .starts_with("[Errno 2] No such file or directory: '")
    );
    #[cfg(windows)]
    {
        let native = std::fs::read(&missing).unwrap_err();
        assert_eq!(native.kind(), std::io::ErrorKind::NotFound);
        let native = native.to_string();
        let detail = native.strip_suffix(" (os error 2)").unwrap();
        assert_eq!(
            err.to_string(),
            format!(
                "[Errno 2] {detail}: {}",
                drmr_core::manifest_support::py::repr_str(missing.to_str().unwrap())
            )
        );
    }
    let latin1 = tmp.path().join("latin1.mpd");
    std::fs::write(&latin1, b"<MPD>\xe9</MPD>").unwrap();
    let err = analyze_dash_manifest(&latin1).unwrap_err();
    assert_eq!(
        class_and_message(&err),
        (
            "UnicodeDecodeError".to_string(),
            "'utf-8' codec can't decode byte 0xe9 in position 5: invalid continuation byte"
                .to_string()
        )
    );
    assert!(err.is_value_error());
}

/// Where Python's `re` and the implementation could part (multi-line and CRLF blocks under `DOTALL`,
/// `\b` after `<AdaptationSet` before a combining mark, `<BaseURL\b` against `<BaseURLx>`,
/// white space and CR around a `PlayReady` blob), against what Python did with the same text.
#[test]
fn regex_edges_match_python() {
    let edges = &fixture("manifest_dash.json")["regex_edges"];
    let tmp = tempfile::tempdir().unwrap();
    for name in [
        "pretty_lf",
        "pretty_crlf",
        "spaced_pssh_lf",
        "spaced_pssh_crlf",
        "combining_mark_tag",
        "word_char_tag",
    ] {
        let case = &edges[name];
        let source = tmp.path().join(format!("{name}.mpd"));
        std::fs::write(&source, text(&case["source"])).unwrap();
        let target = tmp.path().join(format!("{name}.out.mpd"));
        let outcome = match rewrite_dash_manifest(&source, &target, &playready(), None) {
            Ok(result) => json!({ "ok": result_json(&result) }),
            Err(err) => {
                let (class, message) = class_and_message(&err);
                json!({ "error": [class, message] })
            }
        };
        assert_eq!(outcome, case["result"], "{name}");
        if let Some(output) = case.get("output") {
            assert_eq!(read(&target), text(output), "{name}");
            let summary = analyze_dash_manifest(&source).unwrap();
            assert_eq!(summary_json(&summary), case["summary"], "{name}");
        } else {
            assert!(!target.exists(), "{name}");
        }
    }
    let variants = &edges["base_url_variants"];
    let mut seen = Vec::new();
    let localized = localize_dash_manifest_http(text(&variants["source"]), |old| {
        seen.push(old.to_string());
        "/m/&<x>".to_string()
    })
    .unwrap();
    assert_eq!(localized, text(&variants["localized"]));
    assert_eq!(json!(seen), variants["seen"]);
}
