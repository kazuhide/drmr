//! Catalog repackaging, ordered checks, failure handling, and report serialization.

#[path = "support/paths.rs"]
mod paths;
use paths::portable_path;

mod pipeline_fakes;

use drmr_core::canonical::Dumps;
use drmr_core::checks::CheckId;
use drmr_core::config::{Asset, KeyMaterial, Rendition};
use drmr_core::events::{Collector, Record, Sink};
use drmr_core::manifest_dash::DashRewriteResult;
use drmr_core::manifest_hls::{HlsRewriteResult, HlsVariantMapping};
use drmr_core::media::FragmentBoundary;
use drmr_core::pipeline::{
    self, AssetOptions, AssetResult, FailFastGate, ManifestOptions, ManifestResult,
    RenditionOptions, RenditionResult, StepResult, assert_disjoint_output_targets,
    check_boundary_agreement, confine, needs_per_segment, repackage_asset, repackage_asset_with,
    repackage_rendition, resolve_max_workers, rewrite_asset_manifests, worst_status,
};
use drmr_core::pipeline_deps::PyError;
use pipeline_fakes::*;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::Duration;

const RECORD_KEYS: [&str; 7] = [
    "ts",
    "stage",
    "level",
    "message",
    "rendition_id",
    "check_id",
    "duration_s",
];
const STAGES: [&str; 6] = ["merge", "decrypt", "encrypt", "manifest", "report", "raw"];

fn no_round_trip() -> RenditionOptions<'static> {
    RenditionOptions {
        verify_round_trip: false,
        ..RenditionOptions::default()
    }
}

/// `one_rendition`: one H.264 rendition with three segments (the stub's default count).
fn one_rendition(root: &Path) -> Rendition {
    build_synthetic_dash_asset(root, &[h264_rep("v0").segments(3)], "title-abc123").renditions[0]
        .clone()
}

fn check(checks: &[(CheckId, String)], id: CheckId) -> &str {
    checks.iter().find(|(key, _)| *key == id).map_or_else(
        || panic!("{id} missing from {checks:?}"),
        |(_, status)| status.as_str(),
    )
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn report_json(output_dir: &Path) -> Value {
    serde_json::from_slice(&fs::read(output_dir.join("reports/phase1-summary.json")).unwrap())
        .unwrap()
}

fn line(record: &Record) -> String {
    Dumps::DEFAULT.dumps(&Value::Object(record.clone()))
}

// --- repackage_rendition: control flow -------------------------------------------------------

#[test]
fn repackage_rendition_produces_expected_output_layout() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();
    let output_dir = dir.path().join("output");

    let result = repackage_rendition(
        &rendition,
        &key(),
        &output_dir,
        &no_round_trip(),
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(result.rendition_id, rendition.rep_id);
    assert_eq!(
        result.clear_path,
        output_dir.join("clear/video/v0/clear.mp4")
    );
    assert!(result.clear_path.is_file());
    assert_eq!(
        result.cbcs_init_path,
        output_dir.join("cbcs/video/v0/init.mp4")
    );
    assert!(result.cbcs_init_path.is_file());
}

#[test]
fn repackage_rendition_renumbers_to_source_origin() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &no_round_trip(),
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(check(&result.checks, CheckId::NumberOrigin), "PASS");
    assert_eq!(result.output_segment_count, result.source_segment_count);
    assert_eq!(
        names(&result.cbcs_segment_paths),
        ["seg_0.m4s", "seg_1.m4s", "seg_2.m4s"]
    );
    assert_eq!(
        result.number_origin_map[0],
        (1, 0),
        "shaka's seg_1 -> source seg_0"
    );
    // Sorted by the numeric index, not by name.
    assert_eq!(
        result
            .cbcs_segment_paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        ["seg_0.m4s", "seg_1.m4s", "seg_2.m4s"]
    );
}

#[test]
fn repackage_rendition_forwards_segment_duration() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &no_round_trip(),
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(
        result.requested_segment_duration_s.to_bits(),
        6.0f64.to_bits()
    );
    assert_eq!(check(&result.checks, CheckId::SegmentDuration), "PASS");
    let durations: Vec<f64> = fakes
        .packager
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            PackagerCall::Encrypt {
                segment_duration_seconds,
                ..
            } => Some(segment_duration_seconds),
            _ => None,
        })
        .collect();
    assert_eq!(
        durations,
        [6.0],
        "the source-derived 6 s reaches the packager"
    );
}

#[test]
fn repackage_rendition_pssh_agreement_is_skipped_in_phase_1() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &no_round_trip(),
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(
        check(&result.checks, CheckId::PsshAgreement),
        "SKIP: verified during manifest validation"
    );
}

#[test]
fn repackage_rendition_no_key_or_iv_in_any_check_value() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &RenditionOptions::default(),
        &fakes.deps(),
    )
    .unwrap();

    for (_, status) in &result.checks {
        assert!(!status.to_lowercase().contains(KEY));
        assert!(!status.to_lowercase().contains(IV));
    }
}

#[test]
fn repackage_rendition_checks_keep_python_insertion_order_and_statuses() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &RenditionOptions::default(),
        &fakes.deps(),
    )
    .unwrap();

    // What Python writes against the stub's empty media (its mp4dump finds neither a tenc box
    // nor a fragment).
    let expected = [
        (CheckId::SegmentDuration, "PASS"),
        (CheckId::NumberOrigin, "PASS"),
        (
            CheckId::IvAgreement,
            "FAIL: no constant IV found in output init segment",
        ),
        (
            CheckId::BoundaryAgreement,
            "FAIL: segment 0 has no fragment (moof/tfdt/trun) metadata",
        ),
        (
            CheckId::PsshAgreement,
            "SKIP: verified during manifest validation",
        ),
        (
            CheckId::Framemd5,
            "SKIP: gated on 6-1 duration/number-origin/boundary-agreement PASS",
        ),
    ];
    let actual: Vec<(CheckId, &str)> = result
        .checks
        .iter()
        .map(|(id, status)| (*id, status.as_str()))
        .collect();
    assert_eq!(actual, expected);
}

// --- on_log ----------------------------------------------------------------------------------

#[test]
fn repackage_rendition_on_log_none_is_a_silent_default() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &no_round_trip(),
        &fakes.deps(),
    );

    assert!(result.is_ok());
}

#[test]
fn repackage_rendition_on_log_emits_merge_decrypt_encrypt_report_stages() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();
    let collector = Collector::default();

    repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &RenditionOptions {
            verify_round_trip: false,
            on_log: Some(&collector),
            ..RenditionOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    let records = collector.records();
    let stages: BTreeSet<&str> = records
        .iter()
        .map(|record| record["stage"].as_str().unwrap())
        .collect();
    for stage in ["merge", "decrypt", "encrypt", "report"] {
        assert!(stages.contains(stage), "{stage} missing: {stages:?}");
    }
    for record in &records {
        assert!(
            record.keys().all(|key| RECORD_KEYS.contains(&key.as_str())),
            "{record:?}"
        );
        assert!(STAGES.contains(&record["stage"].as_str().unwrap()));
        assert_ne!(record["ts"].as_str().unwrap(), "");
    }
    // The exact sequence Python emits for this rendition (FAIL records in sorted id order).
    let summary: Vec<(String, String, String)> = records
        .iter()
        .map(|record| {
            (
                record["stage"].as_str().unwrap().to_string(),
                record["level"].as_str().unwrap().to_string(),
                record["message"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected = [
        ("merge", "info", "merging 3 source segments"),
        ("decrypt", "info", "decrypting source to clear"),
        ("encrypt", "info", "re-encrypting to cbcs"),
        (
            "report",
            "error",
            "FAIL: segment 0 has no fragment (moof/tfdt/trun) metadata",
        ),
        (
            "report",
            "error",
            "FAIL: no constant IV found in output init segment",
        ),
        ("report", "info", "FAIL"),
    ]
    .map(|(stage, level, message)| (stage.to_string(), level.to_string(), message.to_string()));
    assert_eq!(summary, expected);
    assert_eq!(records[3]["check_id"], "6-1.boundary-agreement");
    assert_eq!(records[4]["check_id"], "6-1.iv-agreement");
}

#[test]
fn repackage_rendition_on_log_records_carry_rendition_id() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();
    let collector = Collector::default();

    repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &RenditionOptions {
            verify_round_trip: false,
            on_log: Some(&collector),
            ..RenditionOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    let merges: Vec<Record> = collector
        .records()
        .into_iter()
        .filter(|record| record["stage"] == "merge")
        .collect();
    assert_ne!(merges.len(), 0);
    assert!(merges.iter().all(|record| record["rendition_id"] == "v0"));
}

#[test]
fn repackage_rendition_on_log_never_leaks_key_or_iv() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let mut fakes = Fakes::new();
    let (runner, tools) = collecting_runner();
    fakes.runner = runner;
    let collector = Collector::default();

    repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &RenditionOptions {
            on_log: Some(&collector),
            ..RenditionOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    let records = collector.records();
    assert_ne!(records.len(), 0);
    let tool_records = tools.records();
    assert!(
        tool_records
            .iter()
            .any(|record| record["tool"] == "packager"),
        "the packager ran through the job's tool runner"
    );
    for record in records.iter().chain(&tool_records) {
        let text = line(record).to_lowercase();
        assert!(!text.contains(KEY), "{text}");
        assert!(!text.contains(IV), "{text}");
    }
}

#[test]
fn repackage_asset_on_log_threads_through_every_rendition() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(
        dir.path(),
        &[h264_rep("v0").segments(3), audio_rep("a0").segments(3)],
        "title-abc123",
    );
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();
    let collector = Collector::default();

    repackage_asset(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            on_log: Some(&collector),
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    let records = collector.records();
    let ids: BTreeSet<&str> = records
        .iter()
        .filter_map(|record| record.get("rendition_id").and_then(Value::as_str))
        .collect();
    assert_eq!(ids, BTreeSet::from(["v0", "a0"]));
    let last = records.last().unwrap();
    assert_eq!(last["stage"], "report");
    assert!(!last.contains_key("rendition_id"));
    assert_eq!(last["message"], "asset title-abc123: FAIL");
}

// --- verify_round_trip gating ----------------------------------------------------------------

#[test]
fn repackage_rendition_skips_round_trip_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &no_round_trip(),
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(
        check(&result.checks, CheckId::Framemd5),
        "SKIP: verify_round_trip=False"
    );
}

#[test]
fn repackage_rendition_round_trip_gated_on_boundary_check() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &RenditionOptions::default(),
        &fakes.deps(),
    )
    .unwrap();

    assert!(check(&result.checks, CheckId::BoundaryAgreement).starts_with("FAIL"));
    assert!(check(&result.checks, CheckId::Framemd5).starts_with("SKIP: gated on"));
    assert!(
        fakes.media.calls("compute_framemd5").is_empty(),
        "6-4 never ran against garbage"
    );
}

/// Boundaries that agree position by position (what the per-segment transcryption achieves).
fn matching_boundaries() -> Reader<Option<FragmentBoundary>> {
    Box::new(|_, path| {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let index: u64 = stem.trim_start_matches("seg_").parse().unwrap();
        Ok(Some(FragmentBoundary::new(index * 287_744, 281)))
    })
}

#[test]
fn repackage_rendition_round_trip_runs_once_the_6_1_gate_passes() {
    // The other half of the gate (checklist 6-4 after 6-1): duration, number origin and
    // boundaries PASS, so the round trip runs — re-merge, re-decrypt, framemd5 of both clear
    // files — and the identical (empty) clear files PASS.
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let mut fakes = Fakes::new();
    fakes.media.boundary = matching_boundaries();
    let output_dir = dir.path().join("output");

    let result = repackage_rendition(
        &rendition,
        &key(),
        &output_dir,
        &RenditionOptions::default(),
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(check(&result.checks, CheckId::BoundaryAgreement), "PASS");
    assert_eq!(check(&result.checks, CheckId::Framemd5), "PASS");
    let work = output_dir.join("_work/v0");
    assert_eq!(
        fakes.media.calls("compute_framemd5"),
        [
            output_dir.join("clear/video/v0/clear.mp4"),
            work.join("roundtrip_clear_from_cbcs.mp4")
        ]
    );
    assert!(fakes.packager.calls().contains(&PackagerCall::Merge {
        output: work.join("roundtrip_encrypted_cbcs.mp4")
    }));
}

#[test]
fn repackage_rendition_round_trip_reports_mismatch_and_framemd5_errors() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let output_dir = dir.path().join("output");

    let mut fakes = Fakes::new();
    fakes.media.boundary = matching_boundaries();
    fakes.media.framemd5 = Box::new(|_, path| Ok(path.display().to_string()));
    let result = repackage_rendition(
        &rendition,
        &key(),
        &output_dir,
        &RenditionOptions::default(),
        &fakes.deps(),
    )
    .unwrap();
    assert_eq!(
        check(&result.checks, CheckId::Framemd5),
        "FAIL: framemd5 mismatch"
    );

    fakes.media.framemd5 = Box::new(|_, _| {
        Err(PyError::Runtime(
            "ffmpeg framemd5 failed for x: boom".into(),
        ))
    });
    let result = repackage_rendition(
        &rendition,
        &key(),
        &output_dir,
        &RenditionOptions::default(),
        &fakes.deps(),
    )
    .unwrap();
    assert_eq!(
        check(&result.checks, CheckId::Framemd5),
        "FAIL: framemd5 error: ffmpeg framemd5 failed for x: boom"
    );
}

#[test]
fn repackage_rendition_round_trip_reports_a_failed_re_decrypt() {
    // The packager fails only on the second decrypt (the round trip's): rewrite the stub
    // between the steps via a boundary reader, which runs after the re-encryption.
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let mut fakes = Fakes::new();
    let stub_dir = fakes.bin.path().to_path_buf();
    fakes.media.boundary = Box::new(move |_, path| {
        write_stub_packager(&stub_dir, 7, None, None);
        matching_boundaries()(&pipeline_fakes::quiet_runner(), path)
    });

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &RenditionOptions::default(),
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(
        check(&result.checks, CheckId::Framemd5),
        "FAIL: round-trip re-decrypt exit 7"
    );
}

// --- _check_boundary_agreement ---------------------------------------------------------------

#[test]
fn check_boundary_agreement_passes_when_every_segment_matches() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let mut fakes = Fakes::new();
    let boundaries: HashMap<PathBuf, FragmentBoundary> = rendition
        .segment_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            (
                path.clone(),
                FragmentBoundary::new(index as u64 * 144_000, 144_000),
            )
        })
        .collect();
    fakes.media.boundary = Box::new(move |_, path| Ok(Some(boundaries[path].clone())));

    let status =
        check_boundary_agreement(&rendition, &rendition.segment_paths, &fakes.deps()).unwrap();

    assert_eq!(status, "PASS");
}

#[test]
fn check_boundary_agreement_fails_on_segment_count_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();
    let shorter = &rendition.segment_paths[..rendition.segment_paths.len() - 1];

    let status = check_boundary_agreement(&rendition, shorter, &fakes.deps()).unwrap();

    assert_eq!(status, "FAIL: segment count source=3 output=2");
}

#[test]
fn check_boundary_agreement_fails_on_same_total_different_distribution() {
    // Same total sample count (1125 either way), but segment 0 diverges: an aggregate check
    // cannot see this; the per-segment comparison must.
    let dir = tempfile::tempdir().unwrap();
    let mut rendition = one_rendition(dir.path());
    let source_counts = [281u64, 281, 281, 282];
    let output_counts = [282u64, 281, 281, 281];
    assert_eq!(
        source_counts.iter().sum::<u64>(),
        output_counts.iter().sum::<u64>()
    );
    let mut fakes = Fakes::new();
    fakes.media.boundary = Box::new(move |_, path| {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let index: usize = stem.split('_').nth(1).unwrap().parse().unwrap();
        let counts = if path.to_string_lossy().contains("output") {
            output_counts
        } else {
            source_counts
        };
        Ok(Some(FragmentBoundary::new(
            index as u64 * 287_744,
            u32::try_from(counts[index]).unwrap(),
        )))
    });
    rendition.segment_paths = (0..4)
        .map(|i| PathBuf::from(format!("/fake/source/seg_{i}.m4s")))
        .collect();
    let output: Vec<PathBuf> = (0..4)
        .map(|i| PathBuf::from(format!("/fake/output/seg_{i}.m4s")))
        .collect();

    let status = check_boundary_agreement(&rendition, &output, &fakes.deps()).unwrap();

    assert_eq!(
        status,
        "FAIL: segment 0 boundary diverges (source sample_count=281, output sample_count=282)"
    );
}

#[test]
fn check_boundary_agreement_fails_when_fragment_metadata_missing() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();

    let status =
        check_boundary_agreement(&rendition, &rendition.segment_paths, &fakes.deps()).unwrap();

    assert!(status.contains("no fragment"), "{status}");
}

#[test]
fn check_boundary_agreement_records_a_probe_error_and_propagates_anything_else() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let mut fakes = Fakes::new();
    fakes.media.boundary = Box::new(|_, _| Err(PyError::Runtime("mp4dump: unavailable".into())));
    assert_eq!(
        check_boundary_agreement(&rendition, &rendition.segment_paths, &fakes.deps()).unwrap(),
        "FAIL: probe error at segment 0: mp4dump: unavailable"
    );
    // A JSON error from the probe is a ValueError in Python: never caught by the check.
    fakes.media.boundary = Box::new(|_, _| Err(PyError::Value("Expecting value".into())));
    assert_eq!(
        check_boundary_agreement(&rendition, &rendition.segment_paths, &fakes.deps()),
        Err(PyError::Value("Expecting value".into()))
    );
}

// --- per-segment routing ---------------------------------------------------------------------

#[test]
fn needs_per_segment_routes_only_off_grid_audio() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(
        dir.path(),
        &[
            h264_rep("v0"),
            audio_rep("a0"),
            audio_rep("a_aligned").ticks(512_000),
        ],
        "title-abc123",
    );
    let reps: HashMap<&str, &Rendition> = synthetic
        .renditions
        .iter()
        .map(|rendition| (rendition.rep_id.as_str(), rendition))
        .collect();

    assert!(
        !needs_per_segment(reps["v0"]),
        "GOP-aligned video: whole-file"
    );
    assert!(needs_per_segment(reps["a0"]), "off-grid AAC: per-segment");
    assert_eq!(reps["a_aligned"].segment_duration_ticks % 1024, 0);
    assert!(
        !needs_per_segment(reps["a_aligned"]),
        "grid-aligned audio: whole-file"
    );
}

#[test]
fn repackage_rendition_audio_uses_boundary_preserving_per_segment_path() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic =
        build_synthetic_dash_asset(dir.path(), &[audio_rep("a0").segments(4)], "title-abc123");
    let rendition = &synthetic.renditions[0];
    let mut fakes = Fakes::new();
    fakes.media.boundary = matching_boundaries();
    let output_dir = dir.path().join("output");

    let result = repackage_rendition(
        rendition,
        &key(),
        &output_dir,
        &no_round_trip(),
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(check(&result.checks, CheckId::SegmentDuration), "PASS");
    assert_eq!(check(&result.checks, CheckId::NumberOrigin), "PASS");
    assert_eq!(check(&result.checks, CheckId::BoundaryAgreement), "PASS");
    assert_eq!(result.output_segment_count, 4);
    assert_eq!(result.source_segment_count, 4);
    assert_eq!(
        names(&result.cbcs_segment_paths),
        ["seg_0.m4s", "seg_1.m4s", "seg_2.m4s", "seg_3.m4s"]
    );
    assert!(result.cbcs_init_path.is_file());
    let transcrypts: Vec<PackagerCall> = fakes
        .packager
        .calls()
        .into_iter()
        .filter(|call| matches!(call, PackagerCall::Transcrypt { .. }))
        .collect();
    assert_eq!(
        transcrypts,
        [PackagerCall::Transcrypt {
            template: portable_path(&output_dir.join("cbcs/audio/a0/seg_$Number$.m4s")),
            segments: 4,
            pssh_hex: None,
            work_dir: Some(output_dir.join("_work").join("a0").join("persegment")),
        }]
    );
}

// --- upstream failure handling  ------------------------------------------------------

#[test]
fn repackage_rendition_decrypt_failure_short_circuits_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::with_exit(5);
    let collector = Collector::default();

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &RenditionOptions {
            on_log: Some(&collector),
            ..RenditionOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(
        check(&result.checks, CheckId::SourceDecrypt),
        "FAIL: exit 5"
    );
    assert_eq!(
        check(&result.checks, CheckId::SegmentDuration),
        "SKIP: upstream step failed"
    );
    assert_eq!(
        check(&result.checks, CheckId::Framemd5),
        "SKIP: upstream step failed"
    );
    assert_eq!(result.cbcs_segment_paths, Vec::<PathBuf>::new());
    assert_eq!(result.output_segment_count, 0);
    // `source.decrypt` first, then `setdefault` in field insertion order.
    let order: Vec<&str> = result.checks.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        order,
        [
            "source.decrypt",
            "6-1.segment-duration",
            "6-1.number-origin",
            "6-1.iv-agreement",
            "6-1.boundary-agreement",
            "6-1.pssh-agreement",
            "6-4.framemd5"
        ]
    );
    let messages: Vec<String> = collector
        .records()
        .iter()
        .map(|record| record["message"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        messages,
        [
            "merging 3 source segments",
            "decrypting source to clear",
            "decrypt failed: exit 5"
        ],
        "no report records on the early exit, as in Python"
    );
}

/// A sink that makes the stub packager fail from the re-encryption on (its record is emitted
/// just before the call), printing a diagnostic.
struct FailFromEncrypt<'a> {
    inner: &'a Collector,
    stub_dir: PathBuf,
}

impl Sink for FailFromEncrypt<'_> {
    fn emit(&self, record: &Record) {
        if record["message"] == "re-encrypting to cbcs" {
            write_stub_packager(&self.stub_dir, 3, Some("packager: cannot encrypt"), None);
        }
        self.inner.emit(record);
    }
}

#[test]
fn repackage_rendition_encrypt_failure_is_a_segment_duration_fail_with_its_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();
    let stub_dir = fakes.bin.path().to_path_buf();
    let collector = Collector::default();
    let sink = FailFromEncrypt {
        inner: &collector,
        stub_dir,
    };

    let result = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &RenditionOptions {
            on_log: Some(&sink),
            ..RenditionOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(
        check(&result.checks, CheckId::SegmentDuration),
        "FAIL: exit 3"
    );
    assert_eq!(
        check(&result.checks, CheckId::NumberOrigin),
        "SKIP: upstream step failed"
    );
    let records = collector.records();
    let tail: Vec<(&str, &str)> = records[records.len() - 2..]
        .iter()
        .map(|record| {
            (
                record["stage"].as_str().unwrap(),
                record["message"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        tail,
        [
            ("encrypt", "encrypt failed: exit 3"),
            ("raw", "packager: cannot encrypt\n")
        ]
    );
    assert_eq!(records[records.len() - 2]["level"], "error");
}

// --- : inputs read-only after a full run ---------------------------------------------

#[test]
fn repackage_asset_full_run_leaves_source_tree_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(
        dir.path(),
        &[h264_rep("v0"), h264_rep("v1"), audio_rep("a0")],
        "ro-check",
    );
    let asset = synthetic.asset(&dir.path().join("output"));
    let before = tree_state(&dir.path().join("download"));
    assert!(!before.is_empty());
    let fakes = Fakes::new();

    repackage_asset(&asset, &AssetOptions::default(), &fakes.deps()).unwrap();

    assert_eq!(tree_state(&dir.path().join("download")), before);
}

// --- repackage_asset: the multi-rendition loop  --------------------------------------

#[test]
fn repackage_asset_loops_over_every_rendition_dynamically() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(
        dir.path(),
        &[h264_rep("v0"), h264_rep("v1"), audio_rep("a0")],
        "asset-xyz",
    );
    let asset = synthetic.asset(&dir.path().join("output"));
    assert_eq!(asset.renditions.len(), 3);
    let fakes = Fakes::new();

    let result = repackage_asset(&asset, &AssetOptions::default(), &fakes.deps()).unwrap();

    assert_eq!(result.schema_version, "1");
    assert_eq!(result.asset_id, "asset-xyz");
    assert_eq!(result.renditions.len(), 3);
    let ids: BTreeSet<&str> = result
        .renditions
        .iter()
        .map(|r| r.rendition_id.as_str())
        .collect();
    assert_eq!(ids, BTreeSet::from(["v0", "v1", "a0"]));
    assert!(result.full_enumeration);
}

#[test]
fn repackage_asset_all_pass_false_when_any_rendition_check_fails() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(dir.path(), &[h264_rep("v0")], "asset-fail");
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();

    let result = repackage_asset(&asset, &AssetOptions::default(), &fakes.deps()).unwrap();

    assert!(!result.all_pass);
}

#[test]
fn repackage_asset_writes_json_and_markdown_reports() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(dir.path(), &[h264_rep("v0")], "asset-report");
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();

    repackage_asset(&asset, &AssetOptions::default(), &fakes.deps()).unwrap();

    let reports = dir.path().join("output/asset-report/reports");
    let json_text = fs::read_to_string(reports.join("phase1-summary.json")).unwrap();
    let json: Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(json["asset_id"], "asset-report");
    assert_eq!(json["schema_version"], "1");
    assert_eq!(json["renditions"].as_array().unwrap().len(), 1);
    assert!(!json_text.to_lowercase().contains(KEY));
    assert!(!json_text.to_lowercase().contains(IV));
    let markdown = fs::read_to_string(reports.join("phase1-summary.md")).unwrap();
    assert!(markdown.contains("asset-report"));
    assert!(markdown.contains("v0"));
    assert!(!markdown.to_lowercase().contains(KEY));
    assert!(!markdown.to_lowercase().contains(IV));
}

#[test]
fn repackage_asset_round_trip_verified_true_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(dir.path(), &[h264_rep("v0")], "rt-default");
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();

    let result = repackage_asset(&asset, &AssetOptions::default(), &fakes.deps()).unwrap();

    assert!(result.round_trip_verified);
}

#[test]
fn repackage_asset_round_trip_verified_false_with_no_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(dir.path(), &[h264_rep("v0")], "rt-disabled");
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();

    let result = repackage_asset(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    assert!(!result.round_trip_verified);
    assert_eq!(report_json(&asset.output_dir)["round_trip_verified"], false);
}

// --- idempotent reruns -----------------------------------------------------------------------

#[test]
fn repackage_rendition_is_idempotent_across_reruns() {
    let dir = tempfile::tempdir().unwrap();
    let rendition = one_rendition(dir.path());
    let fakes = Fakes::new();
    let output_dir = dir.path().join("output");

    let first = repackage_rendition(
        &rendition,
        &key(),
        &output_dir,
        &no_round_trip(),
        &fakes.deps(),
    )
    .unwrap();
    let second = repackage_rendition(
        &rendition,
        &key(),
        &output_dir,
        &no_round_trip(),
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(check(&second.checks, CheckId::NumberOrigin), "PASS");
    assert_eq!(second.output_segment_count, first.output_segment_count);
    assert_eq!(second.output_segment_count, second.source_segment_count);
    assert_eq!(
        names(&second.cbcs_segment_paths),
        names(&first.cbcs_segment_paths)
    );
}

// --- write-site confinement ------------------------------------------------------------------

#[test]
fn repackage_rendition_refuses_to_write_outside_output_dir() {
    let dir = tempfile::tempdir().unwrap();
    let output_dir = dir.path().join("output");
    fs::create_dir(&output_dir).unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();

    let refused = confine(&output_dir.join("../../escaped"), &output_dir);

    assert_eq!(
        refused,
        Err(PyError::Value(format!(
            "refusing to write outside {}: {}",
            portable_path(&root.join("output")),
            portable_path(&root.parent().unwrap().join("escaped"))
        )))
    );
    let safe = output_dir.join("clear/video/v0/clear.mp4");
    assert_eq!(confine(&safe, &output_dir), Ok(safe));
}

#[test]
fn repackage_rendition_refuses_a_rendition_that_escapes_through_its_rel_dir() {
    let dir = tempfile::tempdir().unwrap();
    let mut rendition = one_rendition(dir.path());
    rendition.rel_dir = PathBuf::from("../../elsewhere");
    let fakes = Fakes::new();

    let error = repackage_rendition(
        &rendition,
        &key(),
        &dir.path().join("output"),
        &no_round_trip(),
        &fakes.deps(),
    )
    .unwrap_err();

    assert!(
        error.message().starts_with("refusing to write outside"),
        "{error}"
    );
    assert!(fakes.packager.calls().is_empty(), "nothing ran");
}

// --- _worst_status ---------------------------------------------------------------------------

fn statuses(items: &[&str]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

#[test]
fn worst_status_reports_pass_when_all_pass_or_skip() {
    assert_eq!(
        worst_status(&statuses(&["PASS", "PASS"]), "SKIP: n/a"),
        "PASS"
    );
    assert_eq!(
        worst_status(&statuses(&["PASS", "SKIP: waived"]), "SKIP: n/a"),
        "PASS"
    );
}

#[test]
fn worst_status_reports_fail_with_count_and_first_detail() {
    assert_eq!(
        worst_status(&statuses(&["PASS", "FAIL: x", "FAIL: y"]), "SKIP: n/a"),
        "FAIL: 2/3 gate(s) failed; first: FAIL: x"
    );
}

#[test]
fn worst_status_reports_all_skip_verbatim() {
    assert_eq!(
        worst_status(&statuses(&["SKIP: a"]), "SKIP: n/a"),
        "SKIP: a"
    );
}

#[test]
fn worst_status_none_status_when_empty() {
    assert_eq!(
        worst_status(&[], "SKIP: nothing to gate"),
        "SKIP: nothing to gate"
    );
}

// --- rewrite_asset_manifests -----------------------------------------------------------------

/// A `RenditionResult` for the manifest tests, with its init and clear files written.
fn manifest_rendition_result(
    output_dir: &Path,
    rendition: &Rendition,
    init_bytes: &[u8],
) -> RenditionResult {
    let init_path = output_dir
        .join("cbcs")
        .join(&rendition.rel_dir)
        .join("init.mp4");
    fs::create_dir_all(init_path.parent().unwrap()).unwrap();
    fs::write(&init_path, init_bytes).unwrap();
    let clear_path = output_dir
        .join("clear")
        .join(&rendition.rel_dir)
        .join("clear.mp4");
    fs::create_dir_all(clear_path.parent().unwrap()).unwrap();
    fs::write(&clear_path, b"FAKE-CLEAR-CONTENT").unwrap();
    RenditionResult {
        rendition_id: rendition.rep_id.clone(),
        content_type: rendition.content_type.clone(),
        clear_path,
        cbcs_init_path: init_path,
        cbcs_segment_paths: Vec::new(),
        source_segment_count: 2,
        output_segment_count: 2,
        number_origin_map: Vec::new(),
        requested_segment_duration_s: 6.0,
        checks: vec![(CheckId::IvAgreement, "PASS".into())],
    }
}

fn manifests_for(
    root: &Path,
    title: &str,
    init_bytes: impl Fn(&[u8]) -> Vec<u8>,
    fakes: &Fakes,
    on_log: Option<&dyn Sink>,
) -> (ManifestResult, Asset) {
    let synthetic = build_synthetic_dash_asset(root, &[h264_rep("v0").segments(2)], title);
    let asset = synthetic.asset_as(&root.join("out"), title);
    let objects = playready_objects(KID);
    let result = manifest_rendition_result(
        &asset.output_dir,
        &asset.renditions[0],
        &init_bytes(&objects.pssh_box),
    );
    let master = write_minimal_hls_master(root, title, 2);
    let manifests = rewrite_asset_manifests(
        &asset,
        &objects,
        &ManifestOptions {
            rendition_results: std::slice::from_ref(&result),
            hls_master_path: &master,
            local_url_base: None,
            on_log,
        },
        &fakes.deps(),
    )
    .unwrap();
    (manifests, asset)
}

#[test]
fn rewrite_asset_manifests_pssh_agreement_and_structural_checks() {
    let dir = tempfile::tempdir().unwrap();
    let fakes = Fakes::new();

    let (result, asset) = manifests_for(
        dir.path(),
        "pssh-test",
        |pssh| [b"FAKE-MOOV-HEADER-".as_slice(), pssh, b"-TAIL"].concat(),
        &fakes,
        None,
    );

    assert_eq!(check(&result.checks, CheckId::PsshAgreement), "PASS");
    assert!(check(&result.checks, CheckId::AudioPcmIdentity).starts_with("SKIP"));
    assert!(check(&result.checks, CheckId::HlsContentGate).starts_with("SKIP"));
    assert_eq!(
        check(&result.checks, CheckId::DashConformance),
        "SKIP: DASH-IF validator not yet integrated"
    );
    assert!(result.dash.output_mpd_path.exists());
    assert!(result.hls.output_master_path.exists());
    // : manifests/ mirrors the source host + path structure, never flattened.
    let expected = asset.output_dir.join("manifests").join(
        fs::canonicalize(&asset.mpd_path)
            .unwrap()
            .strip_prefix(&asset.input_base_dir)
            .unwrap(),
    );
    assert_eq!(result.dash.output_mpd_path, expected);
    assert_eq!(
        result
            .dash
            .output_mpd_path
            .parent()
            .unwrap()
            .file_name()
            .unwrap(),
        "dash"
    );
}

#[test]
fn rewrite_asset_manifests_checks_follow_python_dict_order_and_the_6_2_update() {
    // `checks = dict(dash.checks)`, the 6-2 gate's row, `checks.update(hls.checks)` (which
    // overwrites the 6-2 gate's status in place with the module's SKIP row — Python's own
    // behaviour, implemented as is), then the PSSH row, the in-place content/EXTINF rows, 6-6.
    let dir = tempfile::tempdir().unwrap();
    let mut fakes = Fakes::new();
    fakes.manifests.pairs = vec![(dir.path().join("ts.m3u8"), dir.path().join("fmp4.m3u8"))];
    fakes.manifests.pcm_status =
        "FAIL: ts_AUDIO and fmp4_AUDIO decode to different PCM -- consolidation is INVALID".into();

    let (result, _) = manifests_for(dir.path(), "order", <[u8]>::to_vec, &fakes, None);

    let order: Vec<(&str, &str)> = result
        .checks
        .iter()
        .map(|(id, status)| (id.as_str(), status.as_str()))
        .collect();
    assert_eq!(
        order,
        [
            ("6-1.dash-scheme-cbcs", "PASS"),
            ("6-1.dash-playready-sync", "PASS"),
            ("6-1.dash-widevine-unchanged", "PASS"),
            ("6-1.dash-segmenttemplate-unchanged", "PASS"),
            ("6-5.dash-out-of-scope-untouched", "PASS"),
            (
                "6-2.audio-pcm-identity",
                "SKIP: run audio_pcm_consolidation_gate with key material"
            ),
            ("6-1.hls-iv-agreement", "PASS"),
            ("6-1.hls-segment-count", "PASS"),
            ("6-1.hls-version-unchanged", "PASS"),
            ("6-2.audio-consolidation-structure", "PASS"),
            (
                "6-1.hls-content-gate",
                "SKIP: TS H.264 decryption is not supported; segment count, duration, and key signaling checked separately"
            ),
            ("6-1.hls-extinf", "PASS"),
            ("6-5.hls-master-untouched", "PASS"),
            ("6-1.pssh-agreement", "PASS"),
            (
                "6-6.dash-conformance",
                "SKIP: DASH-IF validator not yet integrated"
            ),
        ]
    );
    assert!(pipeline::rendition_all_pass(&result.checks));
}

#[test]
fn audio_pcm_gate_runs_before_hls_repoint_is_emitted() {
    let dir = tempfile::tempdir().unwrap();
    let mut fakes = Fakes::new();
    fakes.manifests.pairs = vec![(
        dir.path().join("ts_audio.m3u8"),
        dir.path().join("fmp4_audio.m3u8"),
    )];

    manifests_for(dir.path(), "order-test", <[u8]>::to_vec, &fakes, None);

    let order: Vec<String> = fakes
        .manifests
        .calls()
        .into_iter()
        .filter_map(|call| call.split(' ').next().map(ToString::to_string))
        .filter(|name| name == "audio_pcm_consolidation_gate" || name == "rewrite_hls")
        .collect();
    assert_eq!(order, ["audio_pcm_consolidation_gate", "rewrite_hls"]);
}

#[test]
fn rewrite_asset_manifests_pssh_agreement_fails_when_box_absent() {
    let dir = tempfile::tempdir().unwrap();
    let fakes = Fakes::new();

    let (result, _) = manifests_for(
        dir.path(),
        "pssh-fail-test",
        |_| b"NO-PSSH-BOX-IN-HERE-AT-ALL".to_vec(),
        &fakes,
        None,
    );

    assert_eq!(
        check(&result.checks, CheckId::PsshAgreement),
        "FAIL: a CBCS init segment does not carry the generated PlayReady pssh box"
    );
}

#[test]
fn rewrite_asset_manifests_records_extinf_probe_errors_and_emits_local_forms() {
    let dir = tempfile::tempdir().unwrap();
    let mut fakes = Fakes::new();
    fakes.manifests.extinf = Box::new(|_| {
        Err(PyError::Runtime(
            "seg_0.m4s has no fragment (moof/tfdt/trun) metadata to measure".into(),
        ))
    });
    let synthetic = build_synthetic_dash_asset(dir.path(), &[h264_rep("v0").segments(2)], "local");
    let asset = synthetic.asset(&dir.path().join("out"));
    let result = manifest_rendition_result(&asset.output_dir, &asset.renditions[0], b"x");
    let master = write_minimal_hls_master(dir.path(), "local", 2);
    let collector = Collector::default();

    let manifests = rewrite_asset_manifests(
        &asset,
        &playready_objects(KID),
        &ManifestOptions {
            rendition_results: std::slice::from_ref(&result),
            hls_master_path: &master,
            local_url_base: Some("http://127.0.0.1:8080/output/local"),
            on_log: Some(&collector),
        },
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(
        check(&manifests.checks, CheckId::HlsExtinf),
        "FAIL: 1/1 gate(s) failed; first: FAIL: EXTINF probe error: seg_0.m4s has no fragment (moof/tfdt/trun) metadata to measure"
    );
    let manifests_root = asset.output_dir.join("manifests");
    let calls = fakes.manifests.calls();
    assert!(calls.contains(&format!(
        "rewrite_dash_manifest {} Some(\"http://127.0.0.1:8080/output/local\")",
        portable_path(&manifests_root.join("local/manifest.example.test/local/dash/manifest.mpd"))
    )));
    assert!(calls.contains(&format!(
        "rewrite_hls {} Some(\"http://127.0.0.1:8080/output/local\")",
        portable_path(&manifests_root.join("local"))
    )));
    // The FAIL rows are logged in sorted id order, then the manifests verdict.
    let records = collector.records();
    let tail: Vec<(&str, &str)> = records[records.len() - 3..]
        .iter()
        .map(|record| {
            (
                record.get("check_id").and_then(Value::as_str).unwrap_or(""),
                record["message"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(tail[0].0, "6-1.hls-extinf");
    assert_eq!(tail[1].0, "6-1.pssh-agreement");
    assert_eq!(tail[2], ("", "manifests: FAIL"));
}

#[test]
fn repackage_asset_manifests_none_when_hls_master_path_omitted() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic =
        build_synthetic_dash_asset(dir.path(), &[h264_rep("v0").segments(3)], "title-abc123");
    let asset = synthetic.asset(&dir.path().join("out"));
    let fakes = Fakes::new();

    let result = repackage_asset(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    assert!(result.manifests.is_none());
    assert_eq!(report_json(&asset.output_dir)["manifests"], Value::Null);
}

#[test]
fn repackage_asset_manifests_populated_when_hls_master_path_given() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic =
        build_synthetic_dash_asset(dir.path(), &[h264_rep("v0").segments(3)], "wiring-test");
    let asset = synthetic.asset(&dir.path().join("out"));
    let master = write_minimal_hls_master(dir.path(), "wiring-test", 3);
    let objects = playready_objects(KID);
    let fakes = Fakes::new();

    let result = repackage_asset(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            hls_master_path: Some(&master),
            playready_objects: Some(&objects),
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    let manifests = result.manifests.as_ref().unwrap();
    assert!(
        manifests
            .checks
            .iter()
            .any(|(id, _)| *id == CheckId::PsshAgreement)
    );
    assert!(
        manifests
            .checks
            .iter()
            .any(|(id, _)| *id == CheckId::DashConformance)
    );
    // The stub never embeds --pssh: an honest FAIL, and all_pass follows the manifests.
    assert!(check(&manifests.checks, CheckId::PsshAgreement).starts_with("FAIL"));
    assert!(!result.all_pass);
    let report = report_json(&asset.output_dir);
    assert!(report["manifests"]["dash"]["output_mpd_path"].is_string());
    assert!(
        report["manifests"]["hls"]["variant_mapping"][0]["dash_rendition"]["init_path"].is_string()
    );
    // The regenerated PlayReady pssh reaches every rendition's --pssh set ( dec 3).
    let pssh: Vec<Option<Vec<String>>> = fakes
        .packager
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            PackagerCall::Encrypt { pssh_hex, .. } => Some(pssh_hex),
            _ => None,
        })
        .collect();
    assert_eq!(pssh, [Some(vec![objects.pssh_hex.clone()])]);
}

#[test]
fn repackage_asset_on_log_per_gate_manifest_records_carry_duration_s() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic =
        build_synthetic_dash_asset(dir.path(), &[h264_rep("v0").segments(3)], "gate-timing");
    let asset = synthetic.asset(&dir.path().join("out"));
    let master = write_minimal_hls_master(dir.path(), "gate-timing", 3);
    let objects = playready_objects(KID);
    let fakes = Fakes::new();
    let collector = Collector::default();

    repackage_asset(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            hls_master_path: Some(&master),
            playready_objects: Some(&objects),
            on_log: Some(&collector),
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    let records = collector.records();
    for record in &records {
        assert!(
            record.keys().all(|key| RECORD_KEYS.contains(&key.as_str())),
            "{record:?}"
        );
        if let Some(duration) = record.get("duration_s") {
            assert!(duration.is_f64() && duration.as_f64().unwrap() >= 0.0);
        }
    }
    let gates: Vec<&Record> = records
        .iter()
        .filter(|record| record["stage"] == "manifest" && record.contains_key("check_id"))
        .collect();
    let mut ids: Vec<&str> = gates
        .iter()
        .map(|record| record["check_id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [
            "6-1.hls-content-gate",
            "6-1.hls-extinf",
            "6-1.pssh-agreement",
            "6-2.audio-pcm-identity"
        ]
    );
    let by_check: HashMap<&str, &Record> = gates
        .iter()
        .map(|record| (record["check_id"].as_str().unwrap(), *record))
        .collect();
    assert_eq!(by_check["6-1.hls-content-gate"]["rendition_id"], "v0");
    assert_eq!(by_check["6-1.hls-extinf"]["rendition_id"], "v0");
    assert!(!by_check["6-2.audio-pcm-identity"].contains_key("rendition_id"));
    assert!(!by_check["6-1.pssh-agreement"].contains_key("rendition_id"));
    let message = by_check["6-2.audio-pcm-identity"]["message"]
        .as_str()
        .unwrap();
    assert!(
        regex::Regex::new(r"^audio-PCM consolidation gate: 0 pair\(s\) checked in \d+\.\d{2}s$")
            .unwrap()
            .is_match(message),
        "{message}"
    );
    let message = by_check["6-1.hls-content-gate"]["message"]
        .as_str()
        .unwrap();
    assert!(
        message.starts_with("content-identity gate: SKIP in "),
        "{message}"
    );
}

// --- rendition-level parallelism  ----------------------------------------------------

/// `_dummy_rendition_result`.
fn dummy_result(rendition: &Rendition, output_dir: &Path) -> RenditionResult {
    RenditionResult {
        rendition_id: rendition.rep_id.clone(),
        content_type: rendition.content_type.clone(),
        clear_path: output_dir
            .join("clear")
            .join(&rendition.rel_dir)
            .join("clear.mp4"),
        cbcs_init_path: output_dir
            .join("cbcs")
            .join(&rendition.rel_dir)
            .join("init.mp4"),
        cbcs_segment_paths: Vec::new(),
        source_segment_count: 0,
        output_segment_count: 0,
        number_origin_map: Vec::new(),
        requested_segment_duration_s: 6.0,
        checks: Vec::new(),
    }
}

#[test]
fn resolve_max_workers_bounds_and_validates() {
    let cpu = pipeline::cpu_count().unwrap_or(4);
    assert_eq!(resolve_max_workers(None, 100), Ok(100.min(cpu)));
    assert_eq!(resolve_max_workers(None, 2), Ok(2.min(cpu)));
    assert!(resolve_max_workers(None, 0).unwrap() >= 1);
    assert_eq!(resolve_max_workers(Some(3), 8), Ok(3));
    assert_eq!(resolve_max_workers(Some(16), 4), Ok(4));
    assert_eq!(resolve_max_workers(Some(1), 8), Ok(1));
    assert_eq!(
        resolve_max_workers(Some(0), 4),
        Err(PyError::Value("max_workers must be >= 1, got 0".into()))
    );
    assert_eq!(
        resolve_max_workers(Some(-2), 4),
        Err(PyError::Value("max_workers must be >= 1, got -2".into()))
    );
}

#[test]
fn repackage_asset_parallel_matches_sequential_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(
        dir.path(),
        &[
            h264_rep("v0"),
            h264_rep("v1"),
            h264_rep("v2"),
            audio_rep("a0"),
        ],
        "determinism",
    );
    let asset = synthetic.asset(&dir.path().join("output"));
    let reports = asset.output_dir.join("reports");
    let fakes = Fakes::new();
    let options = |workers| AssetOptions {
        verify_round_trip: false,
        max_workers: Some(workers),
        ..AssetOptions::default()
    };

    let sequential = repackage_asset(&asset, &options(1), &fakes.deps()).unwrap();
    let sequential_json = fs::read(reports.join("phase1-summary.json")).unwrap();
    let sequential_md = fs::read(reports.join("phase1-summary.md")).unwrap();
    let parallel = repackage_asset(&asset, &options(8), &fakes.deps()).unwrap();

    assert_eq!(parallel, sequential);
    let order: Vec<&str> = parallel
        .renditions
        .iter()
        .map(|r| r.rendition_id.as_str())
        .collect();
    assert_eq!(order, ["v0", "v1", "v2", "a0"]);
    assert_eq!(
        fs::read(reports.join("phase1-summary.json")).unwrap(),
        sequential_json
    );
    assert_eq!(
        fs::read(reports.join("phase1-summary.md")).unwrap(),
        sequential_md
    );
}

#[test]
fn repackage_asset_preserves_order_regardless_of_completion_order() {
    let dir = tempfile::tempdir().unwrap();
    let reps: Vec<RepSpec> = (0..5).map(|i| h264_rep(&format!("v{i}"))).collect();
    let synthetic = build_synthetic_dash_asset(dir.path(), &reps, "order");
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();
    let deps = fakes.deps();
    let count = asset.renditions.len();
    let delayed = |rendition: &Rendition,
                   key: &KeyMaterial,
                   output_dir: &Path,
                   options: &RenditionOptions<'_>|
     -> StepResult {
        let index: usize = rendition.rep_id[1..].parse().unwrap();
        sleep(Duration::from_millis(20 * (count - index) as u64)); // earlier → finishes last
        repackage_rendition(rendition, key, output_dir, options, &deps)
    };

    let result = repackage_asset_with(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            max_workers: Some(5),
            ..AssetOptions::default()
        },
        &deps,
        &delayed,
    )
    .unwrap();

    let order: Vec<&str> = result
        .renditions
        .iter()
        .map(|r| r.rendition_id.as_str())
        .collect();
    assert_eq!(order, ["v0", "v1", "v2", "v3", "v4"]);
}

#[test]
fn repackage_asset_respects_the_concurrency_bound() {
    let dir = tempfile::tempdir().unwrap();
    let reps: Vec<RepSpec> = (0..6).map(|i| h264_rep(&format!("v{i}"))).collect();
    let synthetic = build_synthetic_dash_asset(dir.path(), &reps, "bound");
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();
    let current = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let tracking = |rendition: &Rendition,
                    _: &KeyMaterial,
                    output_dir: &Path,
                    _: &RenditionOptions<'_>|
     -> StepResult {
        let now = current.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        sleep(Duration::from_millis(50));
        current.fetch_sub(1, Ordering::SeqCst);
        Ok(dummy_result(rendition, output_dir))
    };

    repackage_asset_with(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            max_workers: Some(2),
            ..AssetOptions::default()
        },
        &fakes.deps(),
        &tracking,
    )
    .unwrap();

    assert_eq!(
        peak.load(Ordering::SeqCst),
        2,
        "bounded at 2 and genuinely parallel"
    );
}

#[test]
fn repackage_asset_environmental_failure_propagates_and_writes_no_report() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(
        dir.path(),
        &[h264_rep("v0"), h264_rep("v1"), h264_rep("v2")],
        "failfast",
    );
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();
    let maybe_fail = |rendition: &Rendition,
                      _: &KeyMaterial,
                      output_dir: &Path,
                      _: &RenditionOptions<'_>|
     -> StepResult {
        if rendition.rep_id == "v1" {
            return Err(PyError::Packager("simulated spawn failure".into()));
        }
        Ok(dummy_result(rendition, output_dir))
    };

    let error = repackage_asset_with(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            max_workers: Some(3),
            ..AssetOptions::default()
        },
        &fakes.deps(),
        &maybe_fail,
    )
    .unwrap_err();

    assert_eq!(error, PyError::Packager("simulated spawn failure".into()));
    assert!(
        !asset
            .output_dir
            .join("reports/phase1-summary.json")
            .exists()
    );
}

/// `_StartRecorder`: records each start, then sleeps and raises or returns per its script.
struct StartRecorder {
    script: HashMap<String, (u64, bool)>,
    started: Mutex<Vec<String>>,
}

impl StartRecorder {
    fn new(script: &[(&str, u64, bool)]) -> StartRecorder {
        StartRecorder {
            script: script
                .iter()
                .map(|(id, millis, raises)| ((*id).to_string(), (*millis, *raises)))
                .collect(),
            started: Mutex::new(Vec::new()),
        }
    }

    fn step(&self, rendition: &Rendition, output_dir: &Path) -> StepResult {
        self.started.lock().unwrap().push(rendition.rep_id.clone());
        let (millis, raises) = self.script[&rendition.rep_id];
        sleep(Duration::from_millis(millis));
        if raises {
            return Err(PyError::Packager(format!(
                "simulated environmental fault in {}",
                rendition.rep_id
            )));
        }
        Ok(dummy_result(rendition, output_dir))
    }

    fn started(&self) -> BTreeSet<String> {
        self.started.lock().unwrap().iter().cloned().collect()
    }

    fn run(&self, asset: &Asset, workers: i64) -> Result<AssetResult, PyError> {
        let fakes = Fakes::new();
        let step = |rendition: &Rendition,
                    _: &KeyMaterial,
                    output_dir: &Path,
                    _: &RenditionOptions<'_>|
         -> StepResult { self.step(rendition, output_dir) };
        repackage_asset_with(
            asset,
            &AssetOptions {
                verify_round_trip: false,
                max_workers: Some(workers),
                ..AssetOptions::default()
            },
            &fakes.deps(),
            &step,
        )
    }
}

fn six_renditions(root: &Path, title: &str) -> Asset {
    let reps: Vec<RepSpec> = (0..6).map(|i| h264_rep(&format!("v{i}"))).collect();
    build_synthetic_dash_asset(root, &reps, title).asset(&root.join("output"))
}

#[test]
fn repackage_asset_fail_fast_cancels_pending_renditions() {
    // 2 workers over 6 renditions; v0 raises at once, the others hold their slot 0.3 s. Once
    // the failure is seen nothing queued starts: at most v0 and the rendition already taken
    // by the other worker.
    let dir = tempfile::tempdir().unwrap();
    let asset = six_renditions(dir.path(), "failfast-cancel");
    let recorder = StartRecorder::new(&[
        ("v0", 0, true),
        ("v1", 300, false),
        ("v2", 300, false),
        ("v3", 300, false),
        ("v4", 300, false),
        ("v5", 300, false),
    ]);

    let error = recorder.run(&asset, 2).unwrap_err();

    assert!(error.message().contains("v0"), "{error}");
    assert!(
        !asset
            .output_dir
            .join("reports/phase1-summary.json")
            .exists()
    );
    let started = recorder.started();
    assert!(started.contains("v0"));
    assert!(
        started.is_subset(&BTreeSet::from(["v0".into(), "v1".into()])),
        "queued renditions started after the failure: {started:?}"
    );
}

#[test]
fn repackage_asset_fail_fast_does_not_wait_for_earlier_renditions_first() {
    // v0 is slow, v1 raises at once, v2..v5 are quick: true fail-fast sees v1's error at once
    // and starts nothing new while v0 drains.
    let dir = tempfile::tempdir().unwrap();
    let asset = six_renditions(dir.path(), "failfast-late");
    let recorder = StartRecorder::new(&[
        ("v0", 600, false),
        ("v1", 0, true),
        ("v2", 100, false),
        ("v3", 100, false),
        ("v4", 100, false),
        ("v5", 100, false),
    ]);

    let error = recorder.run(&asset, 2).unwrap_err();

    assert!(error.message().contains("v1"), "{error}");
    assert!(
        !asset
            .output_dir
            .join("reports/phase1-summary.json")
            .exists()
    );
    assert_eq!(
        recorder.started(),
        BTreeSet::from(["v0".into(), "v1".into()]),
        "both were already running; nothing queued started"
    );
}

#[test]
fn repackage_asset_fail_fast_reraises_the_first_failure_in_rendition_order() {
    // v2 raises first in wall-clock time, v1 later, v0 succeeds slowly: v1's error comes out,
    // exactly what the sequential loop would raise.
    let dir = tempfile::tempdir().unwrap();
    let reps: Vec<RepSpec> = (0..3).map(|i| h264_rep(&format!("v{i}"))).collect();
    let asset = build_synthetic_dash_asset(dir.path(), &reps, "failfast-order")
        .asset(&dir.path().join("output"));
    let recorder = StartRecorder::new(&[("v0", 300, false), ("v1", 150, true), ("v2", 0, true)]);

    let error = recorder.run(&asset, 3).unwrap_err();

    assert_eq!(
        error,
        PyError::Packager("simulated environmental fault in v1".into())
    );
    assert_eq!(
        recorder.started(),
        BTreeSet::from(["v0".into(), "v1".into(), "v2".into()])
    );
    assert!(
        !asset
            .output_dir
            .join("reports/phase1-summary.json")
            .exists()
    );
}

#[test]
fn repackage_asset_sequential_path_stops_at_the_first_failure() {
    let dir = tempfile::tempdir().unwrap();
    let asset = six_renditions(dir.path(), "sequential-stop");
    let recorder = StartRecorder::new(&[
        ("v0", 0, false),
        ("v1", 0, true),
        ("v2", 0, false),
        ("v3", 0, false),
        ("v4", 0, false),
        ("v5", 0, false),
    ]);

    let error = recorder.run(&asset, 1).unwrap_err();

    assert!(error.message().contains("v1"));
    assert_eq!(
        recorder.started(),
        BTreeSet::from(["v0".into(), "v1".into()])
    );
}

#[test]
fn repackage_asset_a_panicking_rendition_is_a_failure_of_its_own_index() {
    // Python's `except BaseException`: a panic closes the gate like an error and is re-raised
    // after the in-flight renditions drained.
    let dir = tempfile::tempdir().unwrap();
    let asset = six_renditions(dir.path(), "panic");
    let fakes = Fakes::new();
    let started = Mutex::new(Vec::new());
    let step = |rendition: &Rendition,
                _: &KeyMaterial,
                output_dir: &Path,
                _: &RenditionOptions<'_>|
     -> StepResult {
        started.lock().unwrap().push(rendition.rep_id.clone());
        assert!(rendition.rep_id != "v0", "rendition v0 panicked");
        sleep(Duration::from_millis(300));
        Ok(dummy_result(rendition, output_dir))
    };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        repackage_asset_with(
            &asset,
            &AssetOptions {
                verify_round_trip: false,
                max_workers: Some(2),
                ..AssetOptions::default()
            },
            &fakes.deps(),
            &step,
        )
    }));

    let payload = outcome.unwrap_err();
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"rendition v0 panicked")
    );
    let started: BTreeSet<String> = started.lock().unwrap().iter().cloned().collect();
    assert!(
        started.is_subset(&BTreeSet::from(["v0".into(), "v1".into()])),
        "{started:?}"
    );
}

#[test]
fn fail_fast_gate_refuses_only_renditions_after_the_first_failure() {
    let gate = FailFastGate::new(5);
    assert!((0..5).all(|i| gate.may_start(i)));

    gate.record_failure(3);
    assert_eq!(
        (0..5).map(|i| gate.may_start(i)).collect::<Vec<_>>(),
        [true, true, true, true, false]
    );

    gate.record_failure(4);
    assert_eq!(
        (0..5).map(|i| gate.may_start(i)).collect::<Vec<_>>(),
        [true, true, true, true, false]
    );

    gate.record_failure(1);
    assert_eq!(
        (0..5).map(|i| gate.may_start(i)).collect::<Vec<_>>(),
        [true, true, false, false, false]
    );
}

#[test]
fn repackage_asset_content_failures_do_not_raise_and_all_renditions_reported() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(
        dir.path(),
        &[h264_rep("v0"), h264_rep("v1"), audio_rep("a0")],
        "content-fail",
    );
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();

    let result = repackage_asset(
        &asset,
        &AssetOptions {
            max_workers: Some(8),
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    assert!(!result.all_pass);
    let order: Vec<&str> = result
        .renditions
        .iter()
        .map(|r| r.rendition_id.as_str())
        .collect();
    assert_eq!(order, ["v0", "v1", "a0"]);
    assert!(
        asset
            .output_dir
            .join("reports/phase1-summary.json")
            .exists()
    );
}

#[test]
fn repackage_asset_parallel_leaves_source_tree_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(
        dir.path(),
        &[
            h264_rep("v0"),
            h264_rep("v1"),
            h264_rep("v2"),
            audio_rep("a0"),
            audio_rep("a1"),
        ],
        "ro-parallel",
    );
    let asset = synthetic.asset(&dir.path().join("output"));
    let before = tree_state(&dir.path().join("download"));
    assert!(!before.is_empty());
    let fakes = Fakes::new();

    repackage_asset(
        &asset,
        &AssetOptions {
            max_workers: Some(8),
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(tree_state(&dir.path().join("download")), before);
}

#[test]
fn repackage_asset_rejects_colliding_output_targets_on_the_parallel_path() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(dir.path(), &[h264_rep("v0")], "collide");
    let mut asset = synthetic.asset(&dir.path().join("output"));
    asset.renditions.push(asset.renditions[0].clone());
    let fakes = Fakes::new();

    let error = repackage_asset(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            max_workers: Some(2),
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap_err();

    assert_eq!(
        error,
        PyError::Value(
            "renditions must write to distinct output targets for safe concurrent processing: \
             duplicate rep_id='v0'"
                .into()
        )
    );
    assert_eq!(fakes.packager.calls(), []);
}

#[test]
fn assert_disjoint_output_targets_rejects_nested_targets_in_either_order() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic =
        build_synthetic_dash_asset(dir.path(), &[h264_rep("v0"), h264_rep("v1")], "nested");
    let (outer, other) = (&synthetic.renditions[0], &synthetic.renditions[1]);
    let prefix = "renditions must write to distinct output targets for safe concurrent processing";
    // rel_dir (a Path in Python, so PosixPath(...) in the message)
    let mut inner = other.clone();
    inner.rel_dir = outer.rel_dir.join("hd");
    let expected = format!(
        "{prefix}: rel_dir=PosixPath('video/v0/hd') is nested inside rel_dir=PosixPath('video/v0')"
    );
    for pair in [[outer.clone(), inner.clone()], [inner, outer.clone()]] {
        assert_eq!(
            assert_disjoint_output_targets(&pair),
            Err(PyError::Value(expected.clone()))
        );
    }
    // rep_id (a str)
    let mut inner = other.clone();
    inner.rep_id = "v0/hd".into();
    let expected = format!("{prefix}: rep_id='v0/hd' is nested inside rep_id='v0'");
    for pair in [[outer.clone(), inner.clone()], [inner, outer.clone()]] {
        assert_eq!(
            assert_disjoint_output_targets(&pair),
            Err(PyError::Value(expected.clone()))
        );
    }
}

#[test]
fn assert_disjoint_output_targets_judges_paths_the_way_pathlib_does() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic =
        build_synthetic_dash_asset(dir.path(), &[h264_rep("v0"), h264_rep("v1")], "pathlib");
    let prefix = "renditions must write to distinct output targets for safe concurrent processing";
    let (mut first, mut second) = (
        synthetic.renditions[0].clone(),
        synthetic.renditions[1].clone(),
    );
    // `./video/v0` and `video/v0/` are the same Path; the message shows the second.
    first.rel_dir = PathBuf::from("./video/v0");
    second.rel_dir = PathBuf::from("video/v0/");
    assert_eq!(
        assert_disjoint_output_targets(&[first.clone(), second.clone()]),
        Err(PyError::Value(format!(
            "{prefix}: duplicate rel_dir=PosixPath('video/v0')"
        )))
    );
    // Every relative path is nested inside `.`.
    first.rel_dir = PathBuf::from(".");
    second.rel_dir = PathBuf::from("video");
    assert_eq!(
        assert_disjoint_output_targets(&[first.clone(), second.clone()]),
        Err(PyError::Value(format!(
            "{prefix}: rel_dir=PosixPath('video') is nested inside rel_dir=PosixPath('.')"
        )))
    );
    // repr quoting.
    first.rep_id = "it's".into();
    second.rep_id = "it's".into();
    assert_eq!(
        assert_disjoint_output_targets(&[first, second]),
        Err(PyError::Value(format!(
            "{prefix}: duplicate rep_id=\"it's\""
        )))
    );
    assert_eq!(
        pipeline::py_repr("a\u{a0}b\u{2028}c\u{7f}\u{1f600}\u{e000}"),
        "'a\\xa0b\\u2028c\\x7f\u{1f600}\\ue000'"
    );
}

#[test]
fn assert_disjoint_output_targets_accepts_siblings_sharing_a_name_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic =
        build_synthetic_dash_asset(dir.path(), &[h264_rep("v1"), h264_rep("v10")], "siblings");
    let rel_dirs: Vec<&Path> = synthetic
        .renditions
        .iter()
        .map(|r| r.rel_dir.as_path())
        .collect();
    assert_eq!(rel_dirs, [Path::new("video/v1"), Path::new("video/v10")]);

    assert_eq!(
        assert_disjoint_output_targets(&synthetic.renditions),
        Ok(())
    );
}

#[test]
fn repackage_asset_rejects_nested_output_targets_before_any_rendition_starts() {
    let dir = tempfile::tempdir().unwrap();
    let synthetic =
        build_synthetic_dash_asset(dir.path(), &[h264_rep("v0"), h264_rep("v1")], "nested-e2e");
    let mut asset = synthetic.asset(&dir.path().join("output"));
    asset.renditions[1].rel_dir = asset.renditions[0].rel_dir.join("hd");
    let recorder = StartRecorder::new(&[("v0", 0, false), ("v1", 0, false)]);

    let error = recorder.run(&asset, 2).unwrap_err();

    assert!(error.message().contains("distinct output targets"));
    assert!(error.message().contains("nested"));
    assert!(recorder.started().is_empty());
}

/// `cli`'s `print(json.dumps(record))`: the payload and the newline as two writes into a
/// buffer, with an unlocked depth probe around them.
struct ProbingPrintSink {
    buffer: Mutex<String>,
    depth: AtomicUsize,
    max_depth: AtomicUsize,
}

impl Sink for ProbingPrintSink {
    fn emit(&self, record: &Record) {
        let now = self.depth.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_depth.fetch_max(now, Ordering::SeqCst);
        sleep(Duration::from_millis(2));
        self.buffer.lock().unwrap().push_str(&line(record));
        self.buffer.lock().unwrap().push('\n');
        self.depth.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn repackage_asset_parallel_on_log_lines_are_never_torn() {
    let dir = tempfile::tempdir().unwrap();
    let reps: Vec<RepSpec> = (0..6).map(|i| h264_rep(&format!("v{i}"))).collect();
    let synthetic = build_synthetic_dash_asset(dir.path(), &reps, "logsafe");
    let asset = synthetic.asset(&dir.path().join("output"));
    let fakes = Fakes::new();
    let sink = ProbingPrintSink {
        buffer: Mutex::new(String::new()),
        depth: AtomicUsize::new(0),
        max_depth: AtomicUsize::new(0),
    };

    repackage_asset(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            on_log: Some(&sink),
            max_workers: Some(8),
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    assert_eq!(
        sink.max_depth.load(Ordering::SeqCst),
        1,
        "on_log was entered concurrently"
    );
    let text = sink.buffer.lock().unwrap().clone();
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    assert!(lines.len() > asset.renditions.len());
    for line in lines {
        serde_json::from_str::<Value>(line).unwrap_or_else(|e| panic!("torn line {line}: {e}"));
    }
}

// --- reports ---------------------------------------------------------------------------------

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn ids(pairs: &[(CheckId, &str)]) -> Vec<(CheckId, String)> {
    pairs
        .iter()
        .map(|(id, status)| (*id, (*status).to_string()))
        .collect()
}

/// The `AssetResult` of the report fixture, field by field.
fn oracle_result() -> AssetResult {
    let out = PathBuf::from("out/report-asset");
    AssetResult {
        schema_version: "1".into(),
        asset_id: "report-asset".into(),
        renditions: oracle_renditions(&out),
        all_pass: false,
        full_enumeration: true,
        round_trip_verified: true,
        manifests: Some(oracle_manifests(&out)),
    }
}

fn oracle_renditions(out: &Path) -> Vec<RenditionResult> {
    let video = RenditionResult {
        rendition_id: "v0".into(),
        content_type: "video".into(),
        clear_path: out.join("clear/video/v0/clear.mp4"),
        cbcs_init_path: out.join("cbcs/video/v0/init.mp4"),
        cbcs_segment_paths: (0..3)
            .map(|i| out.join(format!("cbcs/video/v0/seg_{i}.m4s")))
            .collect(),
        source_segment_count: 3,
        output_segment_count: 3,
        number_origin_map: vec![(1, 0), (2, 1), (3, 2)],
        requested_segment_duration_s: 144_000.0 / 24_000.0,
        checks: ids(&[
            (CheckId::SegmentDuration, "PASS"),
            (CheckId::NumberOrigin, "PASS"),
            (
                CheckId::IvAgreement,
                "FAIL: no constant IV found in output init segment",
            ),
            (
                CheckId::BoundaryAgreement,
                "FAIL: segment 0 boundary diverges (source sample_count=281, output sample_count=282)",
            ),
            (
                CheckId::PsshAgreement,
                "SKIP: verified during manifest validation",
            ),
            (
                CheckId::Framemd5,
                "SKIP: gated on 6-1 duration/number-origin/boundary-agreement PASS",
            ),
        ]),
    };
    let upstream = "SKIP: upstream step failed";
    let audio = RenditionResult {
        rendition_id: "a0 \u{65e5}\u{672c}\u{8a9e}".into(),
        content_type: "audio".into(),
        clear_path: out.join("clear/audio/a0/clear.mp4"),
        cbcs_init_path: out.join("cbcs/audio/a0/init.mp4"),
        cbcs_segment_paths: Vec::new(),
        source_segment_count: 2,
        output_segment_count: 0,
        number_origin_map: Vec::new(),
        requested_segment_duration_s: 256_000.0 / 48_000.0,
        checks: ids(&[
            (CheckId::SourceDecrypt, "FAIL: exit 5"),
            (CheckId::SegmentDuration, upstream),
            (CheckId::NumberOrigin, upstream),
            (CheckId::IvAgreement, upstream),
            (CheckId::BoundaryAgreement, upstream),
            (CheckId::PsshAgreement, upstream),
            (CheckId::Framemd5, upstream),
        ]),
    };
    let huge = RenditionResult {
        rendition_id: "v1".into(),
        content_type: "video".into(),
        clear_path: PathBuf::from("/abs/clear.mp4"),
        cbcs_init_path: PathBuf::from("/abs/init.mp4"),
        cbcs_segment_paths: vec![PathBuf::from("/abs/seg_0.m4s")],
        source_segment_count: 1,
        output_segment_count: 1,
        number_origin_map: vec![(1, 0)],
        requested_segment_duration_s: 1e22 / 1000.0,
        checks: ids(&[
            (CheckId::SegmentDuration, "PASS"),
            (CheckId::Framemd5, "FAIL: \"quoted\" \\ back\tslash"),
        ]),
    };
    vec![video, audio, huge]
}

fn oracle_manifests(out: &Path) -> ManifestResult {
    let dash_rendition = Rendition {
        rep_id: "v0".into(),
        content_type: "video".into(),
        codec: "avc1.42C00C".into(),
        timescale: 24000,
        segment_duration_ticks: 144_000,
        start_number: 0,
        init_path: PathBuf::from("/download/cdn.example.test/t/video/v0/init.mp4"),
        segment_paths: vec![
            PathBuf::from("/download/cdn.example.test/t/video/v0/seg_0.m4s"),
            PathBuf::from("/download/cdn.example.test/t/video/v0/seg_1.m4s"),
        ],
        rel_dir: PathBuf::from("video/v0"),
        widevine_pssh_hex: Some("46414b45".into()),
        width: Some(320),
        height: Some(180),
        frame_rate: Some("24".into()),
        lang: None,
    };
    let mapping = HlsVariantMapping {
        hls_playlist: PathBuf::from("/download/variants.example.test/t/video/hv0/video_100_x.m3u8"),
        dash_rendition,
        signals: [
            ("content_type", "video"),
            ("codec_family", "avc1"),
            ("hls_bandwidth", "100000"),
            ("odd\"key\\", "tab\tline\n\u{e9}"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .to_vec(),
        content_gate: "waived:TS-H264".into(),
    };
    let dash = DashRewriteResult {
        output_mpd_path: out.join("manifests/manifest.example.test/t/dash/manifest.mpd"),
        adaptation_sets_patched: 1,
        playready_pssh_b64: "UFNTSA==".into(),
        playready_pro_b64: "UFJP".into(),
        widevine_unchanged: true,
        checks: dash_checks(),
    };
    let hls = HlsRewriteResult {
        output_master_path: out.join("manifests/manifest.example.test/t/hls/manifest.m3u8"),
        output_media_playlist_paths: vec![
            out.join("manifests/variants.example.test/t/video/hv0/video_100_x.m3u8"),
        ],
        audio_groups_consolidated: 0,
        ts_audio_groups_removed: 0,
        ext_x_map_added: 1,
        segment_refs_repointed: 2,
        iv_verified: true,
        variant_mapping: vec![mapping],
        checks: hls_checks(),
    };
    let mut manifest_checks = dash_checks();
    manifest_checks.push((
        CheckId::AudioPcmIdentity,
        "SKIP: no ts_AUDIO/fmp4_AUDIO pairs to consolidate".into(),
    ));
    for (id, status) in hls_checks() {
        match manifest_checks.iter_mut().find(|(known, _)| *known == id) {
            Some(entry) => entry.1 = status,
            None => manifest_checks.push((id, status)),
        }
    }
    manifest_checks.push((CheckId::PsshAgreement, "PASS".into()));
    for (id, status) in [
        (
            CheckId::HlsContentGate,
            "SKIP: TS H.264 decryption is not supported; segment count, duration, and key signaling checked separately",
        ),
        (CheckId::HlsExtinf, "PASS"),
    ] {
        manifest_checks
            .iter_mut()
            .find(|(known, _)| *known == id)
            .unwrap()
            .1 = status.into();
    }
    manifest_checks.push((
        CheckId::DashConformance,
        "SKIP: DASH-IF validator not yet integrated".into(),
    ));
    ManifestResult {
        dash,
        hls,
        checks: manifest_checks,
    }
}

#[test]
fn report_writers_match_the_python_oracle_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let full = oracle_result();
    let minimal = AssetResult {
        schema_version: "1".into(),
        asset_id: "empty".into(),
        renditions: Vec::new(),
        all_pass: false,
        full_enumeration: false,
        round_trip_verified: false,
        manifests: None,
    };
    for (name, result) in [
        ("pipeline_report", full),
        ("pipeline_report_minimal", minimal),
    ] {
        let reports = dir.path().join(name);
        pipeline::write_report(&result, &reports).unwrap();
        assert_eq!(
            String::from_utf8(fs::read(reports.join("phase1-summary.json")).unwrap()).unwrap(),
            String::from_utf8(fixture(&format!("{name}.expected.json"))).unwrap(),
            "{name}.json"
        );
        assert_eq!(
            String::from_utf8(fs::read(reports.join("phase1-summary.md")).unwrap()).unwrap(),
            String::from_utf8(fixture(&format!("{name}.expected.md"))).unwrap(),
            "{name}.md"
        );
    }
}

#[test]
fn paths_print_the_way_pathlib_prints_them() {
    assert_eq!(pipeline::py_str(Path::new("./out//a/./b/")), "out/a/b");
    assert_eq!(pipeline::py_str(Path::new("")), ".");
    assert_eq!(pipeline::py_str(Path::new("./")), ".");
    assert_eq!(pipeline::py_str(Path::new("/x/../y")), "/x/../y");
    assert_eq!(
        pipeline::py_str(&PathBuf::from("out").join("t1").join("")),
        "out/t1"
    );
}

#[test]
fn a_report_under_a_relative_output_root_prints_normalized_paths() {
    // `--output./out/` in Python is Path("./out/") → "out": every report path is normalized.
    let dir = tempfile::tempdir().unwrap();
    let synthetic = build_synthetic_dash_asset(dir.path(), &[h264_rep("v0")], "norm");
    let asset = synthetic.asset(&dir.path().join("./out/"));
    let fakes = Fakes::new();

    let result = repackage_asset(
        &asset,
        &AssetOptions {
            verify_round_trip: false,
            ..AssetOptions::default()
        },
        &fakes.deps(),
    )
    .unwrap();

    let report = report_json(&dir.path().join("out/norm"));
    let clear = report["renditions"][0]["clear_path"].as_str().unwrap();
    assert_eq!(
        clear,
        portable_path(&dir.path().join("out/norm/clear/video/v0/clear.mp4"))
    );
    assert!(!clear.contains("/./"));
    assert_eq!(
        result.renditions[0].clear_path,
        dir.path().join("out/norm/clear/video/v0/clear.mp4")
    );
}
