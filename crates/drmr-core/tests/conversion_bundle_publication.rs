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
use sha2::Digest as _;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
#[cfg(unix)]
use {drmr_core::concurrency::Stop, std::collections::BTreeMap};

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
    mutation_lock: Mutex<()>,
    admitted: AtomicUsize,
    mutations: Mutex<Vec<MutationTrace>>,
    interleave: Option<MutationInterleave>,
}

#[derive(Clone, Debug)]
struct MutationTrace {
    ordinal: usize,
    thread: String,
    media: PathBuf,
    read_sha256: String,
    written_sha256: String,
}

struct MutationInterleave {
    admitted: Barrier,
    first_written: Barrier,
}

impl MutationInterleave {
    fn before_read(&self, ordinal: usize) {
        self.admitted.wait();
        if ordinal == 1 {
            self.first_written.wait();
        }
    }

    fn after_write(&self, ordinal: usize) {
        if ordinal == 0 {
            self.first_written.wait();
        }
    }
}

fn media_hash(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
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
    for entry in present_candidate(std::fs::read_dir(root))? {
        let Some(entry) = present_candidate(entry) else {
            continue;
        };
        let path = entry.path();
        let Some(metadata) = present_candidate(path.symlink_metadata()) else {
            continue;
        };
        assert!(!metadata.is_symlink());
        if metadata.is_dir()
            && let Some(path) = bundle_stage(&path)
        {
            return Some(path);
        }
    }
    None
}

fn present_candidate<T>(result: std::io::Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("bundle stage discovery failed: {error}"),
    }
}

#[test]
fn bundle_stage_discovery_skips_a_candidate_removed_before_descent() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let candidate = root.path().join("volatile");
    drmr_core::native::create_private_dir_new(&candidate).unwrap();
    let discovered = std::fs::read_dir(root.path())
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.path() == candidate)
        .unwrap()
        .path();
    let metadata = discovered.symlink_metadata().unwrap();
    assert!(metadata.is_dir());
    assert!(!metadata.is_symlink());
    std::fs::remove_dir(&discovered).unwrap();
    // This is the recursive descent after a candidate's metadata was inspected.
    assert_eq!(bundle_stage(&discovered), None);
    assert!(!discovered.exists());
}

#[test]
fn bundle_stage_discovery_finds_stable_bundles_and_refuses_other_io_errors() {
    use std::io::Write as _;
    let root = drmr_core::native::private_tempdir().unwrap();
    let stable = root.path().join("stable");
    drmr_core::native::create_private_dir_all(&stable.join("dash")).unwrap();
    drmr_core::native::create_private_new(&stable.join("dash/manifest.mpd"))
        .unwrap()
        .write_all(b"stable publication fixture")
        .unwrap();
    assert_eq!(bundle_stage(root.path()), Some(stable.clone()));
    let invalid = root.path().join("invalid\0candidate");
    let error = std::fs::read_dir(&invalid).unwrap_err();
    assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(std::panic::catch_unwind(|| bundle_stage(&invalid)).is_err());
    assert_eq!(
        std::fs::read(stable.join("dash/manifest.mpd")).unwrap(),
        b"stable publication fixture"
    );
}

#[test]
#[cfg(unix)]
fn bundle_stage_discovery_refuses_symbolic_links() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let target = root.path().join("target");
    let search = root.path().join("search");
    drmr_core::native::create_private_dir_new(&target).unwrap();
    drmr_core::native::create_private_dir_new(&search).unwrap();
    std::os::unix::fs::symlink(&target, search.join("link")).unwrap();
    assert!(std::panic::catch_unwind(|| bundle_stage(&search)).is_err());
    assert!(search.join("link").symlink_metadata().unwrap().is_symlink());
    assert!(target.is_dir());
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
            let ordinal = self.admitted.fetch_add(1, Ordering::SeqCst);
            if let Some(interleave) = &self.interleave {
                interleave.before_read(ordinal);
            }
            let _mutation = self.mutation_lock.lock().unwrap();
            if self.fired.load(Ordering::SeqCst) {
                return;
            }
            let mut bytes = std::fs::read(&media).unwrap();
            let read_sha256 = media_hash(&bytes);
            let payload = bytes.windows(4).position(|word| word == b"mdat").unwrap() + 4;
            bytes[payload + 8] ^= 1;
            let written_sha256 = media_hash(&bytes);
            std::fs::write(&media, bytes).unwrap();
            self.mutations.lock().unwrap().push(MutationTrace {
                ordinal,
                thread: format!("{:?}", std::thread::current().id()),
                media: media.strip_prefix(&self.root).unwrap().into(),
                read_sha256,
                written_sha256,
            });
            if let Some(interleave) = &self.interleave {
                interleave.after_write(ordinal);
            }
            self.fired.store(true, Ordering::SeqCst);
        }
    }
}

#[test]
fn concurrent_publication_callbacks_mutate_media_exactly_once() {
    use std::io::Write as _;
    let root = drmr_core::native::private_tempdir().unwrap();
    let dash = root.path().join("bundle/dash");
    drmr_core::native::create_private_dir_all(&dash).unwrap();
    drmr_core::native::create_private_new(&dash.join("manifest.mpd")).unwrap();
    let media = dash.join("segment.m4s");
    let mut original = vec![0; 32];
    original[4..8].copy_from_slice(b"mdat");
    drmr_core::native::create_private_new(&media)
        .unwrap()
        .write_all(&original)
        .unwrap();
    let observed = Arc::new(CorruptOutput {
        root: root.path().into(),
        point: MutationPoint::AfterFirstChild,
        fired: AtomicBool::new(false),
        mutation_lock: Mutex::new(()),
        admitted: AtomicUsize::new(0),
        mutations: Mutex::new(Vec::new()),
        interleave: Some(MutationInterleave {
            admitted: Barrier::new(2),
            first_written: Barrier::new(2),
        }),
    });
    let record: Record =
        serde_json::from_value(serde_json::json!({"activity":"publication","state":"closed"}))
            .unwrap();
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let observed = observed.clone();
            let record = record.clone();
            std::thread::spawn(move || observed.emit(&record))
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let mutations = observed.mutations.lock().unwrap().clone();
    let final_sha256 = media_hash(&std::fs::read(&media).unwrap());
    let original_sha256 = media_hash(&original);
    assert!(observed.fired.load(Ordering::SeqCst));
    assert_eq!(observed.admitted.load(Ordering::SeqCst), 2);
    assert_eq!(
        mutations.len(),
        1,
        "admitted={}; mutations={mutations:?}; original_sha256={original_sha256}; final_sha256={final_sha256}",
        observed.admitted.load(Ordering::SeqCst)
    );
    assert_eq!(mutations[0].ordinal, 0);
    assert_ne!(mutations[0].thread, "");
    assert_eq!(mutations[0].media, Path::new("bundle/dash/segment.m4s"));
    assert_eq!(mutations[0].read_sha256, original_sha256);
    assert_eq!(mutations[0].written_sha256, final_sha256);
    assert_ne!(final_sha256, original_sha256);
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
        mutation_lock: Mutex::new(()),
        admitted: AtomicUsize::new(0),
        mutations: Mutex::new(Vec::new()),
        interleave: None,
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
        "the requested publication window was reached; conversion_error={:?}",
        result.as_ref().err().map(|error| error.0)
    );
    let mutations = observed.mutations.lock().unwrap().clone();
    assert_eq!(
        mutations.len(),
        1,
        "a completed media tamper was required: {mutations:?}"
    );
    assert!(
        result.is_err(),
        "changed output media was published as verified; admitted={}; completed={}; mutations={mutations:?}",
        observed.admitted.load(Ordering::SeqCst),
        mutations.len()
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
