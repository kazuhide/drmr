//! Synchronous progress callbacks cannot invalidate the final publication proof.
mod common;
use drmr_core::{
    concurrency::Stop,
    conversion::{
        Converter,
        conformance::ManifestValidationTools,
        key_file::BundleOperatorKeys,
        plan::bundle::{BundleOutputIntent, ManifestValidation},
    },
    events::{NoSink, Record, Sink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Copy)]
enum Mutation {
    None,
    Cancel,
    Source,
    Output,
}

struct Observer {
    output: PathBuf,
    root: PathBuf,
    source: PathBuf,
    mode: Mutation,
    stop: Stop,
    fired: AtomicBool,
    changed: AtomicBool,
    final_seen: AtomicBool,
    final_after_commit: AtomicBool,
}

impl Sink for Observer {
    fn emit(&self, record: &Record) {
        let text = |key| record.get(key).and_then(serde_json::Value::as_str);
        if text("stage") != Some("pipeline") {
            return;
        }
        if text("scope") == Some("final-check") && text("state") == Some("finished") {
            self.final_seen.store(true, Ordering::SeqCst);
            self.final_after_commit
                .store(self.output.is_dir(), Ordering::SeqCst);
        }
        if text("scope") != Some("publication")
            || text("state") != Some("started")
            || self.fired.swap(true, Ordering::SeqCst)
        {
            return;
        }
        assert!(
            !self.output.exists(),
            "publication starts before the atomic commit"
        );
        let path = match self.mode {
            Mutation::None => return,
            Mutation::Cancel => {
                self.stop.set();
                return;
            }
            Mutation::Source => self.source.clone(),
            Mutation::Output => self.root.join(
                common::snapshot_tree(&self.root)
                    .into_iter()
                    .find(|(name, _)| {
                        Path::new(name)
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("m4s"))
                    })
                    .expect("verified staged segment")
                    .0,
            ),
        };
        let mut bytes = std::fs::read(&path).unwrap();
        let length = bytes.len();
        let last = length - 1;
        let previous = bytes[last];
        bytes[last] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let changed = std::fs::read(&path).unwrap();
        assert_eq!(changed.len(), length);
        self.changed
            .store(changed[last] != previous, Ordering::SeqCst);
    }
}

fn source(root: &Path) -> (PathBuf, PathBuf) {
    std::fs::create_dir(root).unwrap();
    let input = root.join("media.m3u8");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(clip)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(root.join("segment-%d.ts"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    (input, root.join("segment-0.ts"))
}

fn run(mode: Mutation) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (input, segment) = source(&root.path().join("source"));
    let output = root.path().join("result");
    let observed = Arc::new(Observer {
        output: output.clone(),
        root: root.path().into(),
        source: segment,
        mode,
        stop: Stop::new(),
        fired: AtomicBool::new(false),
        changed: AtomicBool::new(false),
        final_seen: AtomicBool::new(false),
        final_after_commit: AtomicBool::new(false),
    });
    let command = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "must-not-start-decoder",
    )
    .with_stop(observed.stop.clone())
    .with_activity_sink(observed.clone());
    let outputs = [("dash", OutputFormat::Dash), ("hls", OutputFormat::Hls)]
        .into_iter()
        .map(|(id, format)| BundleOutputIntent {
            id: id.into(),
            format,
            target: TargetProtection::Clear,
            drms: vec![],
            conformance: ManifestValidation::None,
            processing: None,
            dash_addressing: None,
        })
        .collect::<Vec<_>>();
    let result = command.convert_bundle(
        &input,
        &outputs,
        &output,
        &BundleOperatorKeys::default(),
        &ManifestValidationTools::default(),
    );
    assert!(observed.fired.load(Ordering::SeqCst));
    match mode {
        Mutation::None => {
            let report = result.unwrap();
            assert!(
                report
                    .outputs
                    .iter()
                    .all(|output| output.conversion.verification.passed)
            );
            assert!(observed.final_seen.load(Ordering::SeqCst));
            assert!(
                observed.final_after_commit.load(Ordering::SeqCst),
                "final-check completion must follow the actual atomic commit"
            );
            assert!(output.is_dir());
        }
        Mutation::Cancel | Mutation::Source | Mutation::Output => {
            if !matches!(mode, Mutation::Cancel) {
                assert!(observed.changed.load(Ordering::SeqCst));
            }
            assert!(
                result.is_err(),
                "a precommit observer must not publish cancelled or changed media"
            );
            let error = result.unwrap_err();
            assert_eq!(
                error.0,
                match mode {
                    Mutation::Cancel => "cancelled",
                    Mutation::Source => "execution-plan-source-changed",
                    Mutation::Output => "verification-output-changed",
                    Mutation::None => unreachable!(),
                }
            );
            assert!(!output.exists());
            assert!(!observed.final_seen.load(Ordering::SeqCst));
        }
    }
    assert!(input.exists());
    assert_eq!(
        common::names_with_prefix(root.path(), ".drmr-convert-"),
        Vec::<String>::new()
    );
}

#[test]
fn publication_started_cancellation_cannot_commit() {
    run(Mutation::Cancel);
}
#[test]
fn publication_started_original_mutation_cannot_commit() {
    run(Mutation::Source);
}
#[test]
fn publication_started_verified_output_mutation_cannot_commit() {
    run(Mutation::Output);
}
#[test]
fn final_check_completion_observes_an_already_committed_destination() {
    run(Mutation::None);
}
