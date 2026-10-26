//! The final bundle must contain the exact media checked after each private publication.
use drmr_core::{
    conversion::{
        Converter, HlsInputKeys, InputKeys, VerificationSource,
        conformance::ManifestValidationTools,
        key_file::BundleOperatorKeys,
        plan::bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
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
#[cfg(unix)]
use {
    drmr_core::concurrency::Stop,
    std::{collections::BTreeMap, sync::Mutex},
};

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
    .with_decode(true)
}

fn source(path: &Path) {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(clip)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-an",
            "-map_metadata",
            "-1",
            "-bitexact",
            "-movflags",
            "delay_moov+default_base_moof+frag_keyframe",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[derive(Clone, Copy)]
enum MutationPoint {
    AfterFirstChild,
    BeforeBundlePublication,
}

struct CorruptOutput {
    root: PathBuf,
    point: MutationPoint,
    fired: AtomicBool,
}

fn media_file(root: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = path.symlink_metadata().unwrap();
        assert!(!metadata.is_symlink());
        if metadata.is_dir() {
            if let Some(path) = media_file(&path) {
                return Some(path);
            }
        } else if path.extension().is_some_and(|extension| extension == "m4s") {
            return Some(path);
        }
    }
    None
}

fn bundle_stage(root: &Path) -> Option<PathBuf> {
    if root.join("dash/manifest.mpd").is_file() {
        return Some(root.into());
    }
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = path.symlink_metadata().unwrap();
        assert!(!metadata.is_symlink());
        if metadata.is_dir()
            && let Some(path) = bundle_stage(&path)
        {
            return Some(path);
        }
    }
    None
}

impl Sink for CorruptOutput {
    fn emit(&self, record: &Record) {
        let expected = match self.point {
            MutationPoint::AfterFirstChild => "closed",
            MutationPoint::BeforeBundlePublication => "started",
        };
        if self.fired.load(Ordering::SeqCst)
            || record.get("activity").and_then(serde_json::Value::as_str) != Some("publication")
            || record.get("state").and_then(serde_json::Value::as_str) != Some(expected)
        {
            return;
        }
        if let Some(stage) = bundle_stage(&self.root) {
            if matches!(self.point, MutationPoint::BeforeBundlePublication)
                && !stage.join("hls/master.m3u8").is_file()
            {
                return;
            }
            let media =
                media_file(&stage.join("dash")).expect("DASH media was published privately");
            let mut bytes = std::fs::read(&media).unwrap();
            let payload = bytes.windows(4).position(|word| word == b"mdat").unwrap() + 4;
            bytes[payload + 8] ^= 1;
            std::fs::write(media, bytes).unwrap();
            self.fired.store(true, Ordering::SeqCst);
        }
    }
}

fn refuse_changed_output(point: MutationPoint) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    source(&input);
    let original = std::fs::read(&input).unwrap();
    let foreign = root.path().join("foreign");
    std::fs::write(&foreign, b"must remain").unwrap();
    let output = root.path().join("result");
    let outputs: Vec<_> = [OutputFormat::Dash, OutputFormat::Hls]
        .into_iter()
        .map(|format| BundleOutputIntent {
            dash_addressing: None,
            processing: None,
            id: format.as_str().into(),
            format,
            target: TargetProtection::Clear,
            drms: Vec::new(),
            conformance: ManifestValidation::None,
        })
        .collect();
    let plan = converter()
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output_root: &output,
            outputs: &outputs,
        })
        .unwrap();
    let observed = Arc::new(CorruptOutput {
        root: root.path().into(),
        point,
        fired: AtomicBool::new(false),
    });
    let result = converter()
        .with_activity_sink(observed.clone())
        .execute_bundle_plan(
            &plan,
            &BundleOperatorKeys::default(),
            &ManifestValidationTools::default(),
        );
    assert!(
        observed.fired.load(Ordering::SeqCst),
        "the requested publication window was reached"
    );
    assert!(
        result.is_err(),
        "changed output media was published as verified"
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(input).unwrap(), original);
    assert_eq!(std::fs::read(foreign).unwrap(), b"must remain");
    for entry in std::fs::read_dir(root.path()).unwrap() {
        assert!(
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".drmr-convert-")
        );
    }
}

#[test]
fn changed_media_after_a_verified_child_cannot_become_the_new_reference() {
    refuse_changed_output(MutationPoint::AfterFirstChild);
}

#[test]
fn changed_media_at_final_publication_cannot_be_committed() {
    refuse_changed_output(MutationPoint::BeforeBundlePublication);
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Interruption {
    Cancel,
    Fail,
    Panic,
}

#[cfg(unix)]
struct ReplaceBundle {
    root: PathBuf,
    foreign: PathBuf,
    retained: PathBuf,
    stop: Stop,
    interruption: Interruption,
    replaced: Mutex<Option<(PathBuf, TreeBytes)>>,
}

#[cfg(unix)]
type TreeBytes = BTreeMap<PathBuf, Vec<u8>>;

#[cfg(unix)]
fn tree_bytes(root: &Path) -> TreeBytes {
    let mut result = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = path.symlink_metadata().unwrap();
        assert!(!metadata.is_symlink());
        if metadata.is_dir() {
            for (child, bytes) in tree_bytes(&path) {
                result.insert(Path::new(path.file_name().unwrap()).join(child), bytes);
            }
        } else {
            result.insert(
                path.file_name().unwrap().into(),
                std::fs::read(path).unwrap(),
            );
        }
    }
    result
}

#[cfg(unix)]
fn dash_plan(input: &Path, output: &Path) -> drmr_core::conversion::plan::bundle::BundlePlan {
    converter()
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output_root: output,
            outputs: &[BundleOutputIntent {
                dash_addressing: None,
                processing: None,
                id: "dash".into(),
                format: OutputFormat::Dash,
                target: TargetProtection::Clear,
                drms: Vec::new(),
                conformance: ManifestValidation::None,
            }],
        })
        .unwrap()
}

#[cfg(unix)]
impl Sink for ReplaceBundle {
    fn emit(&self, record: &Record) {
        if record.get("activity").and_then(serde_json::Value::as_str) != Some("publication")
            || record.get("state").and_then(serde_json::Value::as_str) != Some("closed")
            || self.replaced.lock().unwrap().is_some()
        {
            return;
        }
        if let Some(stage) = bundle_stage(&self.root) {
            let original = tree_bytes(&stage);
            std::fs::rename(&stage, &self.retained).unwrap();
            std::fs::rename(&self.foreign, &stage).unwrap();
            *self.replaced.lock().unwrap() = Some((stage, original));
            match self.interruption {
                Interruption::Cancel => self.stop.set(),
                Interruption::Fail => {}
                Interruption::Panic => panic!("interrupt after replacing the private bundle"),
            }
        }
    }
}

#[cfg(unix)]
fn preserve_replaced_bundle(interruption: Interruption) {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    source(&input);
    let original = std::fs::read(&input).unwrap();
    let output = root.path().join("result");
    let foreign = root.path().join("foreign");
    drmr_core::native::create_private_dir_new(&foreign).unwrap();
    std::fs::create_dir(foreign.join("nested")).unwrap();
    std::fs::write(
        foreign.join("nested/sentinel"),
        b"preexisting foreign bytes",
    )
    .unwrap();
    let foreign_bytes = tree_bytes(&foreign);
    let foreign_metadata = std::fs::metadata(&foreign).unwrap();
    assert_eq!(foreign_metadata.permissions().mode() & 0o777, 0o700);
    let plan = dash_plan(&input, &output);
    let observed = Arc::new(ReplaceBundle {
        root: root.path().into(),
        foreign,
        retained: root.path().join("retained-bundle"),
        stop: Stop::new(),
        interruption,
        replaced: Mutex::new(None),
    });
    let command = converter()
        .with_stop(observed.stop.clone())
        .with_activity_sink(observed.clone());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        command.execute_bundle_plan(
            &plan,
            &BundleOperatorKeys::default(),
            &ManifestValidationTools::default(),
        )
    }));
    match interruption {
        Interruption::Cancel => assert_eq!(result.unwrap().unwrap_err().0, "cancelled"),
        Interruption::Fail => assert!(result.unwrap().is_err()),
        Interruption::Panic => assert!(result.is_err()),
    }
    let replaced = observed.replaced.lock().unwrap();
    let (replacement, retained_bytes) = replaced.as_ref().expect("a child was published privately");
    assert!(!output.exists());
    assert_eq!(std::fs::read(input).unwrap(), original);
    assert_eq!(tree_bytes(&observed.retained), *retained_bytes);
    assert!(replacement.is_dir(), "the foreign replacement was removed");
    assert_eq!(tree_bytes(replacement), foreign_bytes);
    let metadata = std::fs::metadata(replacement).unwrap();
    assert_eq!(metadata.uid(), foreign_metadata.uid());
    assert_eq!(metadata.dev(), foreign_metadata.dev());
    assert_eq!(metadata.ino(), foreign_metadata.ino());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
}

#[test]
#[cfg(unix)]
fn cancelled_bundle_preserves_a_foreign_replacement_and_the_moved_original() {
    preserve_replaced_bundle(Interruption::Cancel);
}

#[test]
#[cfg(unix)]
fn failed_bundle_preserves_a_foreign_replacement_and_the_moved_original() {
    preserve_replaced_bundle(Interruption::Fail);
}

#[test]
#[cfg(unix)]
fn panicking_bundle_preserves_a_foreign_replacement_and_the_moved_original() {
    preserve_replaced_bundle(Interruption::Panic);
}

#[cfg(unix)]
struct ReplaceAfterCommit {
    root: PathBuf,
    foreign: PathBuf,
    output: PathBuf,
    stage: Mutex<Option<PathBuf>>,
    replaced: AtomicBool,
}

#[cfg(unix)]
impl Sink for ReplaceAfterCommit {
    fn emit(&self, record: &Record) {
        if record.get("activity").and_then(serde_json::Value::as_str) != Some("publication")
            || record.get("state").and_then(serde_json::Value::as_str) != Some("closed")
            || self.replaced.load(Ordering::SeqCst)
        {
            return;
        }
        if self.output.exists() {
            let stage = self.stage.lock().unwrap();
            let stage = stage.as_ref().expect("a child was published privately");
            assert!(!stage.exists());
            std::fs::rename(&self.foreign, stage).unwrap();
            self.replaced.store(true, Ordering::SeqCst);
        } else {
            *self.stage.lock().unwrap() = bundle_stage(&self.root);
        }
    }
}

#[test]
#[cfg(unix)]
fn a_successful_bundle_never_recursively_cleans_a_postcommit_replacement() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    source(&input);
    let original = std::fs::read(&input).unwrap();
    let foreign = root.path().join("foreign");
    drmr_core::native::create_private_dir_new(&foreign).unwrap();
    std::fs::write(foreign.join("sentinel"), b"foreign postcommit bytes").unwrap();
    let foreign_bytes = tree_bytes(&foreign);
    let output = root.path().join("result");
    let plan = dash_plan(&input, &output);
    let observed = Arc::new(ReplaceAfterCommit {
        root: root.path().into(),
        foreign,
        output: output.clone(),
        stage: Mutex::new(None),
        replaced: AtomicBool::new(false),
    });
    let report = converter()
        .with_activity_sink(observed.clone())
        .execute_bundle_plan(
            &plan,
            &BundleOperatorKeys::default(),
            &ManifestValidationTools::default(),
        )
        .unwrap();
    assert!(observed.replaced.load(Ordering::SeqCst));
    assert!(report.outputs[0].conversion.verification.passed);
    assert!(output.join("dash/manifest.mpd").is_file());
    assert_eq!(std::fs::read(input).unwrap(), original);
    let replacement = observed.stage.lock().unwrap();
    let replacement = replacement.as_ref().unwrap();
    assert!(replacement.is_dir(), "the foreign replacement was removed");
    assert_eq!(tree_bytes(replacement), foreign_bytes);
}
