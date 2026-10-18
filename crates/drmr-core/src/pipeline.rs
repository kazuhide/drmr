//! Catalog-asset repackaging and verification.
//!
//! Each rendition is merged, decrypted, encrypted as CBCS, and checked before manifest
//! rewriting. Bounded parallel work stops starting new tasks after failure. Reports
//! preserve check order and use stable status strings and JSON serialization.

use crate::canonical::{Dumps, python_float_repr};
use crate::checks::CheckId;
use crate::config::{Asset, KeyMaterial, Rendition, segment_duration_seconds};
use crate::events::{Record, Sink, pipeline_record};
use crate::manifest_dash::DashRewriteResult;
use crate::manifest_hls::{HlsRewriteResult, HlsVariantMapping};
use crate::media::python::py_str as py_value_str;
use crate::packager::PackagerResult;
use crate::pipeline_deps::{
    Checks, EncryptCbcs, Manifests, MediaInspect, Packager, PyError, TranscryptCbcsPerSegment,
};
use crate::playready::PlayReadyObjects;
use crate::toolrun::ToolRunner;
use serde_json::{Map, Value};
use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;
use zeroize::Zeroizing;

/// `AssetResult.schema_version` (additive-only across schema revisions).
pub const SCHEMA_VERSION: &str = "1";
/// The per-rendition scratch directory under `output/<asset_id>/`.
pub const WORK_DIRNAME: &str = "_work";
/// AAC-LC emits 1024 samples per frame (the only in-scope audio codec).
pub const AAC_LC_FRAME_SAMPLES: i64 = 1024;
/// 6-4 runs only when these three are `PASS` (checklist 6-4: it presupposes 6-1).
pub const ROUND_TRIP_GATE_CHECKS: [CheckId; 3] = [
    CheckId::SegmentDuration,
    CheckId::NumberOrigin,
    CheckId::BoundaryAgreement,
];

/// The modules the pipeline calls into, and the job's tool runner (Python's process-wide
/// `toolrun` sink). `packager_bin` is what Python's `packager` resolves to on `PATH`.
#[derive(Clone, Copy)]
pub struct PipelineDeps<'a> {
    /// `packager`.
    pub packager: &'a dyn Packager,
    /// `media_inspect`.
    pub media_inspect: &'a dyn MediaInspect,
    /// `manifest_dash` / `manifest_hls`.
    pub manifests: &'a dyn Manifests,
    /// The job's tool runner: its sink, its registered secrets and the `PATH` its tools
    /// resolve on.
    pub runner: &'a Arc<ToolRunner>,
    /// The Shaka Packager binary (Python's `packager_bin`): the CLI passes the bare `packager`,
    /// which `--packager-bin` makes resolve through the runner's `PATH`.
    pub packager_bin: &'a str,
}

/// One rendition's outcome (`RenditionResult`). No key or IV field.
#[derive(Clone, Debug, PartialEq)]
pub struct RenditionResult {
    /// `Representation@id`.
    pub rendition_id: String,
    /// `video` or `audio`.
    pub content_type: String,
    /// The retained clear file (6-4), `output/<asset_id>/clear/<rel_dir>/clear.mp4`.
    pub clear_path: PathBuf,
    /// The CBCS init segment.
    pub cbcs_init_path: PathBuf,
    /// The CBCS segments, renumbered to the source origin.
    pub cbcs_segment_paths: Vec<PathBuf>,
    /// Source segments.
    pub source_segment_count: usize,
    /// Output segments (must equal the source count).
    pub output_segment_count: usize,
    /// `(shaka $Number$, source segment number)` pairs, sorted.
    pub number_origin_map: Vec<(i64, i64)>,
    /// `--segment_duration` as derived from the source (6.0 for the source presentation).
    pub requested_segment_duration_s: f64,
    /// Check id → status, in insertion order.
    pub checks: Checks,
}

/// The manifest rewrite's outcome (`ManifestResult`).
#[derive(Clone, Debug, PartialEq)]
pub struct ManifestResult {
    /// The DASH patch.
    pub dash: DashRewriteResult,
    /// The HLS map and patch.
    pub hls: HlsRewriteResult,
    /// Both modules' checks plus the cross-module gates, in insertion order.
    pub checks: Checks,
}

/// One asset's outcome (`AssetResult`), serialized to `reports/phase1-summary.{json,md}`.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetResult {
    /// [`SCHEMA_VERSION`].
    pub schema_version: String,
    /// The asset id.
    pub asset_id: String,
    /// One result per rendition, in `asset.renditions` order.
    pub renditions: Vec<RenditionResult>,
    /// Every check that ran passed (SKIPs do not fail it), with at least one PASS.
    pub all_pass: bool,
    /// False on a `--limit-renditions` or `--no-manifests` run.
    pub full_enumeration: bool,
    /// False when `--no-round-trip` was used.
    pub round_trip_verified: bool,
    /// `None` when the manifest rewrite was not requested.
    pub manifests: Option<ManifestResult>,
}

/// The keyword arguments of `repackage_rendition`.
#[derive(Clone, Copy)]
pub struct RenditionOptions<'a> {
    /// The `--pssh` boxes (hex) for this rendition.
    pub pssh_hex: Option<&'a [String]>,
    /// Keep the clear file (default true).
    pub keep_clear: bool,
    /// Run the 6-4 round trip (default true).
    pub verify_round_trip: bool,
    /// The job-log sink.
    pub on_log: Option<&'a dyn Sink>,
}

impl Default for RenditionOptions<'_> {
    fn default() -> Self {
        RenditionOptions {
            pssh_hex: None,
            keep_clear: true,
            verify_round_trip: true,
            on_log: None,
        }
    }
}

/// The keyword arguments of `rewrite_asset_manifests`.
#[derive(Clone, Copy)]
pub struct ManifestOptions<'a> {
    /// Every rendition's result, in `asset.renditions` order.
    pub rendition_results: &'a [RenditionResult],
    /// The source HLS master playlist.
    pub hls_master_path: &'a Path,
    /// When set, also emit the locally-servable form under `manifests/local/`.
    pub local_url_base: Option<&'a str>,
    /// The job-log sink.
    pub on_log: Option<&'a dyn Sink>,
}

/// The keyword arguments of `repackage_asset`.
#[derive(Clone, Copy)]
pub struct AssetOptions<'a> {
    /// Extra `--pssh` boxes per rendition id (the copied Widevine box).
    pub pssh_hex_by_rendition: Option<&'a BTreeMap<String, Vec<String>>>,
    /// Run the 6-4 round trip.
    pub verify_round_trip: bool,
    /// Recorded in the report.
    pub full_enumeration: bool,
    /// With `playready_objects`: run the manifest rewrite.
    pub hls_master_path: Option<&'a Path>,
    /// With `hls_master_path`: run the manifest rewrite; its `pssh_hex` joins every
    /// rendition's `--pssh` set.
    pub playready_objects: Option<&'a PlayReadyObjects>,
    /// See [`ManifestOptions::local_url_base`].
    pub manifest_local_url_base: Option<&'a str>,
    /// The job-log sink.
    pub on_log: Option<&'a dyn Sink>,
    /// The rendition pool bound; `None` is the CPU bound.
    pub max_workers: Option<i64>,
}

impl Default for AssetOptions<'_> {
    fn default() -> Self {
        AssetOptions {
            pssh_hex_by_rendition: None,
            verify_round_trip: true,
            full_enumeration: true,
            hls_master_path: None,
            playready_objects: None,
            manifest_local_url_base: None,
            on_log: None,
            max_workers: None,
        }
    }
}

/// What one rendition step returns.
pub type StepResult = Result<RenditionResult, PyError>;

/// Injectable per-rendition operation used by `repackage_asset`.
pub type RenditionStep<'a> =
    dyn Fn(&Rendition, &KeyMaterial, &Path, &RenditionOptions<'_>) -> StepResult + Sync + 'a;

/// `_emit`: one pipeline record to the sink, when there is one.
#[derive(Clone, Copy)]
struct Log<'a>(Option<&'a dyn Sink>);

impl Log<'_> {
    fn emit(
        self,
        stage: &str,
        level: &str,
        message: &str,
        rendition_id: Option<&str>,
        check_id: Option<CheckId>,
        duration_s: Option<f64>,
    ) {
        if let Some(sink) = self.0 {
            sink.emit(&pipeline_record(
                stage,
                level,
                message,
                rendition_id,
                check_id,
                duration_s,
            ));
        }
    }

    fn info(self, stage: &str, message: &str, rendition_id: Option<&str>) {
        self.emit(stage, "info", message, rendition_id, None, None);
    }

    /// A timed gate's record (`stage="manifest"`, `check_id`, `duration_s`).
    fn gate(self, check_id: CheckId, rendition_id: Option<&str>, elapsed: f64, message: &str) {
        self.emit(
            "manifest",
            "info",
            message,
            rendition_id,
            Some(check_id),
            Some(elapsed),
        );
    }

    /// One `level="error"` record per FAIL status, in sorted check-id order.
    fn fails(self, checks: &[(CheckId, String)], rendition_id: Option<&str>) {
        for (check_id, status) in sorted_checks(checks) {
            if status.starts_with("FAIL") {
                self.emit(
                    "report",
                    "error",
                    status,
                    rendition_id,
                    Some(*check_id),
                    None,
                );
            }
        }
    }
}

/// `checks[id] = status`: a new id is appended, an existing one keeps its place.
fn set_check(checks: &mut Checks, id: CheckId, status: impl Into<String>) {
    let status = status.into();
    match checks.iter_mut().find(|(key, _)| *key == id) {
        Some(entry) => entry.1 = status,
        None => checks.push((id, status)),
    }
}

/// `checks.get(id)`.
fn get_check(checks: &[(CheckId, String)], id: CheckId) -> Option<&str> {
    checks
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, status)| status.as_str())
}

/// `sorted(checks.items)`: by the id string, as Python sorts the keys.
fn sorted_checks(checks: &[(CheckId, String)]) -> Vec<&(CheckId, String)> {
    let mut sorted: Vec<&(CheckId, String)> = checks.iter().collect();
    sorted.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    sorted
}

/// `str(Path(path))`'s path: `.` components dropped, separators collapsed, no trailing
/// separator, `.` for an empty path (`PurePosixPath`).
pub fn py_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        if component != Component::CurDir {
            out.push(component.as_os_str());
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// `str(Path(path))`.
pub fn py_str(path: &Path) -> String {
    crate::pycompat::PurePosixPath::from_path(&py_path(path)).to_string()
}

/// `Path.resolve` (non-strict): absolute, with every existing symlink resolved and `..`
/// applied after it, the missing remainder appended as written (`os.path.realpath`).
pub fn py_resolve(path: &Path) -> Result<PathBuf, PyError> {
    #[cfg(windows)]
    {
        crate::native::resolve_path(path).map_err(|error| PyError::os(&error, path))
    }
    #[cfg(unix)]
    py_resolve_unix(path)
}

#[cfg(unix)]
fn py_resolve_unix(path: &Path) -> Result<PathBuf, PyError> {
    let absolute = if path.has_root() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| PyError::os(&error, Path::new(".")))?
            .join(path)
    };
    let mut resolved = PathBuf::new();
    join_real(&mut resolved, &absolute, &mut HashMap::new());
    Ok(resolved)
}

/// `posixpath._joinrealpath`: resolve `rest` onto `base`; false on a symlink loop (the rest is
/// then appended unresolved, as Python's non-strict mode does).
#[cfg(unix)]
fn join_real(
    base: &mut PathBuf,
    rest: &Path,
    seen: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> bool {
    let mut components = rest.components();
    while let Some(component) = components.next() {
        match component {
            Component::RootDir | Component::Prefix(_) => base.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                base.pop();
            }
            Component::Normal(name) => {
                let candidate = base.join(name);
                let is_link = fs::symlink_metadata(&candidate)
                    .is_ok_and(|meta| meta.file_type().is_symlink());
                if !is_link {
                    *base = candidate;
                    continue;
                }
                match seen.get(&candidate) {
                    Some(Some(target)) => {
                        *base = target.clone();
                        continue;
                    }
                    Some(None) => {
                        *base = candidate;
                        base.extend(components);
                        return false;
                    }
                    None => {}
                }
                seen.insert(candidate.clone(), None);
                let Ok(target) = fs::read_link(&candidate) else {
                    *base = candidate;
                    continue;
                };
                if !join_real(base, &target, seen) {
                    base.extend(components);
                    return false;
                }
                seen.insert(candidate, Some(base.clone()));
            }
        }
    }
    true
}

/// `repr(text)` of a Python `str`: single quotes unless the text holds a single quote and no
/// double quote; backslash, quote, `\t` `\n` `\r` and non-printable characters escaped.
/// Printability is exact for ASCII and approximated outside it (control, separator and the
/// common format and private-use characters are escaped).
pub fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for character in text.chars() {
        let code = u32::from(character);
        match character {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if character == quote => {
                out.push('\\');
                out.push(character);
            }
            _ if code < 0x20 || code == 0x7f => {
                write!(out, "\\x{code:02x}").expect("String never fails to write");
            }
            _ if !character.is_ascii() && !py_printable(character) => {
                if code < 0x100 {
                    write!(out, "\\x{code:02x}")
                } else if code < 0x1_0000 {
                    write!(out, "\\u{code:04x}")
                } else {
                    write!(out, "\\U{code:08x}")
                }
                .expect("String never fails to write");
            }
            _ => out.push(character),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable` for a non-ASCII character (see [`py_repr`]).
fn py_printable(character: char) -> bool {
    !(character.is_control()
        || character.is_whitespace()
        || matches!(
            u32::from(character),
            0xad | 0x600..=0x605
                | 0x61c
                | 0x6dd
                | 0x70f
                | 0x180e
                | 0x200b..=0x200f
                | 0x202a..=0x202e
                | 0x2060..=0x2064
                | 0x2066..=0x206f
                | 0xe000..=0xf8ff
                | 0xfeff
                | 0xfff9..=0xfffb
                | 0xf_0000..=0x10_ffff
        ))
}

/// `_needs_per_segment`: off-grid AAC audio (the 1024-sample frame does not divide the segment)
/// takes the boundary-preserving per-segment transcryption; video and grid-aligned audio keep
/// the whole-file re-encryption.
pub fn needs_per_segment(rendition: &Rendition) -> bool {
    rendition.content_type == "audio"
        && rendition.segment_duration_ticks % AAC_LC_FRAME_SAMPLES != 0
}

/// `_numeric_key`: the first run of digits in the file stem, -1 without one.
fn numeric_key(path: &Path) -> i128 {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default();
    let digits: String = stem
        .chars()
        .skip_while(|character| !character.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        -1
    } else {
        digits.parse().unwrap_or(i128::MAX)
    }
}

/// `_confine`: `path` itself, refused (`ValueError`) when it resolves outside `root`.
pub fn confine(path: &Path, root: &Path) -> Result<PathBuf, PyError> {
    let root_resolved = py_resolve(root)?;
    let path_resolved = py_resolve(path)?;
    if path_resolved != root_resolved && !path_resolved.starts_with(&root_resolved) {
        return Err(PyError::Value(format!(
            "refusing to write outside {}: {}",
            py_str(&root_resolved),
            py_str(&path_resolved)
        )));
    }
    Ok(path.to_path_buf())
}

/// `_rendition_all_pass`: every status is `PASS` or a SKIP, and at least one is `PASS`.
pub fn rendition_all_pass(checks: &[(CheckId, String)]) -> bool {
    checks
        .iter()
        .all(|(_, status)| status == "PASS" || status.starts_with("SKIP"))
        && checks.iter().any(|(_, status)| status == "PASS")
}

/// `_check_boundary_agreement` : each source segment's `tfdt` and `trun` sample
/// count against the output segment at the same index.
pub fn check_boundary_agreement(
    rendition: &Rendition,
    output_segment_paths: &[PathBuf],
    deps: &PipelineDeps<'_>,
) -> Result<String, PyError> {
    let source_segment_paths = &rendition.segment_paths;
    if output_segment_paths.len() != source_segment_paths.len() {
        return Ok(format!(
            "FAIL: segment count source={} output={}",
            source_segment_paths.len(),
            output_segment_paths.len()
        ));
    }
    let read = |path: &Path| deps.media_inspect.read_fragment_boundary(deps.runner, path);
    for (index, (source_seg, output_seg)) in source_segment_paths
        .iter()
        .zip(output_segment_paths)
        .enumerate()
    {
        let boundaries = read(source_seg.as_path())
            .and_then(|source| read(output_seg.as_path()).map(|output| (source, output)));
        let (source_boundary, output_boundary) = match boundaries {
            Ok(pair) => pair,
            Err(error) if error.is_runtime() => {
                return Ok(format!("FAIL: probe error at segment {index}: {error}"));
            }
            Err(error) => return Err(error),
        };
        let (Some(source_boundary), Some(output_boundary)) = (source_boundary, output_boundary)
        else {
            return Ok(format!(
                "FAIL: segment {index} has no fragment (moof/tfdt/trun) metadata"
            ));
        };
        if source_boundary != output_boundary {
            return Ok(format!(
                "FAIL: segment {index} boundary diverges (source sample_count={}, output \
                 sample_count={})",
                py_value_str(&source_boundary.sample_count),
                py_value_str(&output_boundary.sample_count)
            ));
        }
    }
    Ok("PASS".into())
}

/// `_check_round_trip_framemd5` : `framemd5(clear.mp4) ==
/// framemd5(clear_from_cbcs.mp4)`.
fn check_round_trip_framemd5(
    clear_path: &Path,
    cbcs_init_path: &Path,
    cbcs_segment_paths: &[PathBuf],
    key: &KeyMaterial,
    work_dir: &Path,
    deps: &PipelineDeps<'_>,
) -> Result<String, PyError> {
    let merged_cbcs = deps.packager.merge_segments(
        cbcs_init_path,
        cbcs_segment_paths,
        &work_dir.join("roundtrip_encrypted_cbcs.mp4"),
    )?;
    let clear_from_cbcs = work_dir.join("roundtrip_clear_from_cbcs.mp4");
    let redecrypt = deps.packager.decrypt_cenc(
        deps.runner,
        &merged_cbcs,
        &clear_from_cbcs,
        key,
        deps.packager_bin,
    )?;
    if redecrypt.returncode != 0 {
        return Ok(format!(
            "FAIL: round-trip re-decrypt exit {}",
            redecrypt.returncode
        ));
    }
    let framemd5 = |path: &Path| deps.media_inspect.compute_framemd5(deps.runner, path);
    match framemd5(clear_path)
        .and_then(|original| framemd5(&clear_from_cbcs).map(|roundtrip| (original, roundtrip)))
    {
        Ok((original, roundtrip)) => Ok(if original == roundtrip {
            "PASS".into()
        } else {
            "FAIL: framemd5 mismatch".into()
        }),
        Err(error) if error.is_runtime() => Ok(format!("FAIL: framemd5 error: {error}")),
        Err(error) => Err(error),
    }
}

/// Where one rendition writes.
struct RenditionPaths {
    work_dir: PathBuf,
    clear_path: PathBuf,
    cbcs_dir: PathBuf,
    init_output: PathBuf,
}

impl RenditionPaths {
    /// `output_dir/_work/<rep_id>`, `output_dir/clear/<rel_dir>/clear.mp4` and
    /// `output_dir/cbcs/<rel_dir>`, each confined to `output_dir` at the write site.
    fn new(rendition: &Rendition, output_dir: &Path) -> Result<RenditionPaths, PyError> {
        let under = |parts: &[&Path]| -> Result<PathBuf, PyError> {
            let mut path = output_dir.to_path_buf();
            for part in parts {
                path.push(part);
            }
            confine(&py_path(&path), output_dir)
        };
        let work_dir = under(&[Path::new(WORK_DIRNAME), Path::new(&rendition.rep_id)])?;
        let clear_path = under(&[
            Path::new("clear"),
            &rendition.rel_dir,
            Path::new("clear.mp4"),
        ])?;
        let cbcs_dir = under(&[Path::new("cbcs"), &rendition.rel_dir])?;
        Ok(RenditionPaths {
            init_output: cbcs_dir.join("init.mp4"),
            work_dir,
            clear_path,
            cbcs_dir,
        })
    }
}

/// A packager step exited non-zero: its FAIL row, an error record and its stderr tail as a
/// `raw` record.
fn record_step_failure(
    log: Log<'_>,
    rep_id: Option<&str>,
    checks: &mut Checks,
    check: CheckId,
    stage: &str,
    result: &PackagerResult,
) {
    set_check(checks, check, format!("FAIL: exit {}", result.returncode));
    log.emit(
        stage,
        "error",
        &format!("{stage} failed: exit {}", result.returncode),
        rep_id,
        None,
        None,
    );
    if !result.stderr_tail.is_empty() {
        log.info("raw", &result.stderr_tail, rep_id);
    }
}

/// `repackage_rendition`: merge → decrypt → CBCS re-encrypt (whole-file, or per source segment
/// for off-grid audio) → renumber → the 6-1 checks → the gated 6-4 round trip. A content
/// failure is a FAIL status, never an error; an environmental fault (`PackagerError`) or an
/// I/O failure propagates.
pub fn repackage_rendition(
    rendition: &Rendition,
    key: &KeyMaterial,
    output_dir: &Path,
    options: &RenditionOptions<'_>,
    deps: &PipelineDeps<'_>,
) -> Result<RenditionResult, PyError> {
    let paths = RenditionPaths::new(rendition, &py_path(output_dir))?;
    let duration_s = segment_duration_seconds(rendition)?;
    #[cfg(windows)]
    let _output_guard = crate::native::pin_private_directory(output_dir)
        .map_err(|error| PyError::os(&error, output_dir))?;
    let source_segment_count = rendition.segment_paths.len();
    let log = Log(options.on_log);
    let rep_id = Some(rendition.rep_id.as_str());
    let mut checks = Checks::new();

    log.info(
        "merge",
        &format!("merging {source_segment_count} source segments"),
        rep_id,
    );
    let merged_source = deps.packager.merge_segments(
        &rendition.init_path,
        &rendition.segment_paths,
        &paths.work_dir.join("source_merged.mp4"),
    )?;
    log.info("decrypt", "decrypting source to clear", rep_id);
    let decrypt_result = deps.packager.decrypt_cenc(
        deps.runner,
        &merged_source,
        &paths.clear_path,
        key,
        deps.packager_bin,
    )?;
    if decrypt_result.returncode != 0 {
        let check = CheckId::SourceDecrypt;
        record_step_failure(log, rep_id, &mut checks, check, "decrypt", &decrypt_result);
        return Ok(early_exit_result(
            rendition,
            paths,
            source_segment_count,
            duration_s,
            checks,
        ));
    }

    // Fresh directory every run: renumber_to_source_origin requires exactly the contiguous
    // 1..N set Shaka just wrote.
    fresh_dir(&paths.cbcs_dir)?;
    log.info("encrypt", "re-encrypting to cbcs", rep_id);
    let encrypt_result = encrypt(rendition, key, &paths, duration_s, options, deps)?;
    if encrypt_result.returncode != 0 {
        let check = CheckId::SegmentDuration;
        record_step_failure(log, rep_id, &mut checks, check, "encrypt", &encrypt_result);
        return Ok(early_exit_result(
            rendition,
            paths,
            source_segment_count,
            duration_s,
            checks,
        ));
    }
    set_check(&mut checks, CheckId::SegmentDuration, "PASS");
    let verified = verify(rendition, key, &paths, options, deps, &mut checks)?;

    if !options.keep_clear {
        remove_file_missing_ok(&paths.clear_path)?;
    }
    log.fails(&checks, rep_id);
    log.info(
        "report",
        if rendition_all_pass(&checks) {
            "PASS"
        } else {
            "FAIL"
        },
        rep_id,
    );
    Ok(RenditionResult {
        rendition_id: rendition.rep_id.clone(),
        content_type: rendition.content_type.clone(),
        clear_path: paths.clear_path,
        cbcs_init_path: paths.init_output,
        output_segment_count: verified.cbcs_segment_paths.len(),
        cbcs_segment_paths: verified.cbcs_segment_paths,
        source_segment_count,
        number_origin_map: verified.number_origin_map,
        requested_segment_duration_s: duration_s,
        checks,
    })
}

/// The CBCS re-encryption: per source segment for off-grid audio, whole-file otherwise.
fn encrypt(
    rendition: &Rendition,
    key: &KeyMaterial,
    paths: &RenditionPaths,
    duration_s: f64,
    options: &RenditionOptions<'_>,
    deps: &PipelineDeps<'_>,
) -> Result<PackagerResult, PyError> {
    let staging_template = py_str(&paths.cbcs_dir.join("seg_$Number$.m4s"));
    if needs_per_segment(rendition) {
        deps.packager.transcrypt_cbcs_per_segment(
            deps.runner,
            &TranscryptCbcsPerSegment {
                init_path: &rendition.init_path,
                segment_paths: &rendition.segment_paths,
                init_output: &paths.init_output,
                segment_template: &staging_template,
                key,
                pssh_hex: options.pssh_hex,
                packager_bin: deps.packager_bin,
                work_dir: Some(&paths.work_dir.join("persegment")),
            },
        )
    } else {
        deps.packager.encrypt_cbcs(
            deps.runner,
            &EncryptCbcs {
                input_mp4: &paths.clear_path,
                init_output: &paths.init_output,
                segment_template: &staging_template,
                key,
                segment_duration_seconds: duration_s,
                pssh_hex: options.pssh_hex,
                packager_bin: deps.packager_bin,
            },
        )
    }
}

/// What the checks after a successful re-encryption measured.
struct Verified {
    cbcs_segment_paths: Vec<PathBuf>,
    number_origin_map: Vec<(i64, i64)>,
}

/// The checks after a successful re-encryption: 6-1 number origin, IV, boundaries, the PSSH
/// SKIP, then 6-4 behind its gate.
fn verify(
    rendition: &Rendition,
    key: &KeyMaterial,
    paths: &RenditionPaths,
    options: &RenditionOptions<'_>,
    deps: &PipelineDeps<'_>,
    checks: &mut Checks,
) -> Result<Verified, PyError> {
    let mut number_origin_map = match deps
        .packager
        .renumber_to_source_origin(&paths.cbcs_dir, rendition.start_number)
    {
        Ok(map) => {
            set_check(checks, CheckId::NumberOrigin, "PASS");
            map
        }
        Err(PyError::Value(message)) => {
            set_check(checks, CheckId::NumberOrigin, format!("FAIL: {message}"));
            Vec::new()
        }
        Err(error) => return Err(error),
    };
    number_origin_map.sort_unstable();

    let cbcs_segment_paths = staged_segments(&paths.cbcs_dir)?;
    let (output_count, source_count) = (cbcs_segment_paths.len(), rendition.segment_paths.len());
    if output_count != source_count && get_check(checks, CheckId::NumberOrigin) == Some("PASS") {
        set_check(
            checks,
            CheckId::NumberOrigin,
            format!("FAIL: output count {output_count} != source count {source_count}"),
        );
    }

    // HARD INVARIANT (explicit --iv): the CBCS constant IV in the output init must be the
    // configured one. Never the IV itself in a status : a mismatch
    // is reimplemented as a fact; both sides of the comparison are wiped after it.
    let expected_iv = Zeroizing::new(key.iv_hex().to_lowercase());
    let iv_status = match deps
        .media_inspect
        .read_tenc_constant_iv_hex(deps.runner, &paths.init_output)
    {
        Ok(Some(actual)) if actual.expose() == expected_iv.as_str() => "PASS".to_string(),
        Ok(None) => "FAIL: no constant IV found in output init segment".to_string(),
        Ok(Some(_)) => "FAIL: constant IV does not match configured IV".to_string(),
        Err(error) if error.is_runtime() => format!("FAIL: {error}"),
        Err(error) => return Err(error),
    };
    set_check(checks, CheckId::IvAgreement, iv_status);

    let boundary = check_boundary_agreement(rendition, &cbcs_segment_paths, deps)?;
    set_check(checks, CheckId::BoundaryAgreement, boundary);
    set_check(
        checks,
        CheckId::PsshAgreement,
        "SKIP: verified during manifest validation",
    );

    let gate_ok = ROUND_TRIP_GATE_CHECKS
        .iter()
        .all(|id| get_check(checks, *id) == Some("PASS"));
    let framemd5 = if !options.verify_round_trip {
        "SKIP: verify_round_trip=False".to_string()
    } else if !gate_ok {
        "SKIP: gated on 6-1 duration/number-origin/boundary-agreement PASS".to_string()
    } else {
        check_round_trip_framemd5(
            &paths.clear_path,
            &paths.init_output,
            &cbcs_segment_paths,
            key,
            &paths.work_dir,
            deps,
        )?
    };
    set_check(checks, CheckId::Framemd5, framemd5);
    Ok(Verified {
        cbcs_segment_paths,
        number_origin_map,
    })
}

/// `cbcs_dir.glob("seg_*.m4s")` sorted (stably) by `_numeric_key`.
fn staged_segments(cbcs_dir: &Path) -> Result<Vec<PathBuf>, PyError> {
    let entries = fs::read_dir(cbcs_dir).map_err(|error| PyError::os(&error, cbcs_dir))?;
    let mut segments = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| PyError::os(&error, cbcs_dir))?;
        let name = entry.file_name();
        let bytes = name.as_encoded_bytes();
        if bytes.len() >= 8 && bytes.starts_with(b"seg_") && bytes.ends_with(b".m4s") {
            segments.push(cbcs_dir.join(&name));
        }
    }
    segments.sort_by_key(|path| numeric_key(path));
    Ok(segments)
}

/// `if dir.exists: shutil.rmtree(dir)` then `dir.mkdir(parents=True, exist_ok=True)`.
fn fresh_dir(dir: &Path) -> Result<(), PyError> {
    if dir.exists() {
        let is_link = fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink());
        if is_link {
            return Err(PyError::Os(
                "Cannot call rmtree on a symbolic link".to_string(),
            ));
        }
        fs::remove_dir_all(dir).map_err(|error| PyError::os(&error, dir))?;
    }
    crate::media::create_output_dir(dir).map_err(|error| PyError::os(&error, dir))
}

/// `path.unlink(missing_ok=True)`.
fn remove_file_missing_ok(path: &Path) -> Result<(), PyError> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(PyError::os(&error, path))
        }
        _ => Ok(()),
    }
}

/// `_early_exit_result`: a prerequisite step failed; everything downstream is SKIP so one bad
/// rendition does not abort the asset.
fn early_exit_result(
    rendition: &Rendition,
    paths: RenditionPaths,
    source_segment_count: usize,
    duration_s: f64,
    mut checks: Checks,
) -> RenditionResult {
    for id in [
        CheckId::SegmentDuration,
        CheckId::NumberOrigin,
        CheckId::IvAgreement,
        CheckId::BoundaryAgreement,
        CheckId::PsshAgreement,
        CheckId::Framemd5,
    ] {
        // `checks.setdefault(name,...)`
        if get_check(&checks, id).is_none() {
            checks.push((id, "SKIP: upstream step failed".to_string()));
        }
    }
    RenditionResult {
        rendition_id: rendition.rep_id.clone(),
        content_type: rendition.content_type.clone(),
        clear_path: paths.clear_path,
        cbcs_init_path: paths.init_output,
        cbcs_segment_paths: Vec::new(),
        source_segment_count,
        output_segment_count: 0,
        number_origin_map: Vec::new(),
        requested_segment_duration_s: duration_s,
        checks,
    }
}

/// `_worst_status`: any FAIL wins (with the count and the first failure's detail); all SKIP is
/// the first SKIP verbatim; otherwise PASS; `none_status` when there was nothing to gate.
pub fn worst_status(statuses: &[String], none_status: &str) -> String {
    if statuses.is_empty() {
        return none_status.to_string();
    }
    let fails: Vec<&String> = statuses
        .iter()
        .filter(|status| status.starts_with("FAIL"))
        .collect();
    if let Some(first) = fails.first() {
        return format!(
            "FAIL: {}/{} gate(s) failed; first: {first}",
            fails.len(),
            statuses.len()
        );
    }
    if statuses.iter().all(|status| status.starts_with("SKIP")) {
        return statuses[0].clone();
    }
    "PASS".to_string()
}

/// `status.split(':')[0]`.
fn verdict(status: &str) -> &str {
    status.split(':').next().unwrap_or_default()
}

/// `PurePath.relative_to` (Python 3.11's wording on failure).
fn relative_to(path: &Path, base: &Path) -> Result<PathBuf, PyError> {
    path.strip_prefix(base).map(Path::to_path_buf).map_err(|_| {
        PyError::Value(format!(
            "{} is not in the subpath of {} OR one path is relative and the other is \
                 absolute.",
            py_repr(&py_str(path)),
            py_repr(&py_str(base))
        ))
    })
}

/// One `rewrite_asset_manifests` call: its inputs and the gate scratch directory.
struct ManifestRun<'a> {
    asset: &'a Asset,
    playready_objects: &'a PlayReadyObjects,
    options: &'a ManifestOptions<'a>,
    deps: &'a PipelineDeps<'a>,
    log: Log<'a>,
    hls_master_path: PathBuf,
    manifests_root: PathBuf,
    dash_output_mpd: PathBuf,
    work_dir: PathBuf,
}

impl ManifestRun<'_> {
    /// : before the repoint is emitted.
    fn audio_pcm_gate(&self, checks: &mut Checks) -> Result<(), PyError> {
        let started = Instant::now();
        let pairs = self
            .deps
            .manifests
            .find_audio_consolidation_pairs(&self.hls_master_path)?;
        let mut statuses = Vec::with_capacity(pairs.len());
        for (ts_path, fmp4_path) in &pairs {
            statuses.push(self.deps.manifests.audio_pcm_consolidation_gate(
                self.deps.runner,
                ts_path,
                fmp4_path,
                &self.asset.key,
                &self.work_dir.join("audio_pcm"),
            )?);
        }
        set_check(
            checks,
            CheckId::AudioPcmIdentity,
            worst_status(
                &statuses,
                "SKIP: no ts_AUDIO/fmp4_AUDIO pairs to consolidate",
            ),
        );
        let elapsed = started.elapsed().as_secs_f64();
        self.log.gate(
            CheckId::AudioPcmIdentity,
            None,
            elapsed,
            &format!(
                "audio-PCM consolidation gate: {} pair(s) checked in {elapsed:.2}s",
                statuses.len()
            ),
        );
        Ok(())
    }

    /// The HLS map and patch (and both local forms); its checks merge into `checks` like
    /// `dict.update` — which overwrites the 6-2 gate's status with the module's SKIP row.
    fn hls(&self, checks: &mut Checks) -> Result<HlsRewriteResult, PyError> {
        self.log.info(
            "manifest",
            "mapping + patching HLS master and media playlists",
            None,
        );
        let manifests = self.deps.manifests;
        let hls_result = manifests.rewrite_hls(
            &self.hls_master_path,
            &self.asset.renditions,
            &self.manifests_root,
            self.asset.key.iv_hex(),
            None,
        )?;
        if let Some(url_base) = self.options.local_url_base {
            let local = self.manifests_root.join("local");
            let relative = relative_to(&self.dash_output_mpd, &self.manifests_root)?;
            manifests.rewrite_dash_manifest(
                &self.asset.mpd_path,
                &py_path(&local.join(relative)),
                self.playready_objects,
                Some(url_base),
            )?;
            manifests.rewrite_hls(
                &self.hls_master_path,
                &self.asset.renditions,
                &local,
                self.asset.key.iv_hex(),
                Some(url_base),
            )?;
        }
        for (id, status) in &hls_result.checks {
            set_check(checks, *id, status.clone());
        }
        Ok(hls_result)
    }

    /// /02: every available CBCS init segment carries the generated `PlayReady` box.
    fn pssh_gate(&self, checks: &mut Checks) -> Result<(), PyError> {
        let started = Instant::now();
        let (mut checked, mut failed) = (false, false);
        for result in self.options.rendition_results {
            if !result.cbcs_init_path.exists() {
                continue; // failed upstream; recorded in its own checks
            }
            checked = true;
            if !self
                .deps
                .media_inspect
                .pssh_box_present(&result.cbcs_init_path, &self.playready_objects.pssh_box)?
            {
                failed = true;
            }
        }
        let status = if !checked {
            "SKIP: no CBCS init segment available to check"
        } else if failed {
            "FAIL: a CBCS init segment does not carry the generated PlayReady pssh box"
        } else {
            "PASS"
        };
        set_check(checks, CheckId::PsshAgreement, status);
        let elapsed = started.elapsed().as_secs_f64();
        self.log.gate(
            CheckId::PsshAgreement,
            None,
            elapsed,
            &format!(
                "pssh agreement gate: {} rendition(s) checked in {elapsed:.2}s",
                self.options.rendition_results.len()
            ),
        );
        Ok(())
    }

    /// The content-identity and EXTINF gates, once per DASH ↔ HLS variant mapping.
    fn mapping_gates(
        &self,
        hls_result: &HlsRewriteResult,
        checks: &mut Checks,
    ) -> Result<(), PyError> {
        let result_by_id: HashMap<&str, &RenditionResult> = self
            .options
            .rendition_results
            .iter()
            .map(|result| (result.rendition_id.as_str(), result))
            .collect();
        let (mut content_statuses, mut extinf_statuses) = (Vec::new(), Vec::new());
        for mapping in &hls_result.variant_mapping {
            let rep_id = mapping.dash_rendition.rep_id.as_str();
            if let Some(result) = result_by_id.get(rep_id)
                && result.clear_path.exists()
            {
                content_statuses.push(self.content_gate(mapping, &result.clear_path, rep_id)?);
            }
            extinf_statuses.push(self.extinf_gate(mapping, rep_id)?);
        }
        set_check(
            checks,
            CheckId::HlsContentGate,
            worst_status(&content_statuses, "SKIP: no HLS variant mapping to gate"),
        );
        set_check(
            checks,
            CheckId::HlsExtinf,
            worst_status(&extinf_statuses, "SKIP: no HLS variant mapping to gate"),
        );
        Ok(())
    }

    fn content_gate(
        &self,
        mapping: &HlsVariantMapping,
        clear_path: &Path,
        rep_id: &str,
    ) -> Result<String, PyError> {
        let started = Instant::now();
        let status = self.deps.manifests.content_identity_gate(
            self.deps.runner,
            mapping,
            clear_path,
            &self.asset.key,
            &self.work_dir.join("content_gate"),
        )?;
        let elapsed = started.elapsed().as_secs_f64();
        self.log.gate(
            CheckId::HlsContentGate,
            Some(rep_id),
            elapsed,
            &format!(
                "content-identity gate: {} in {elapsed:.2}s",
                verdict(&status)
            ),
        );
        Ok(status)
    }

    fn extinf_gate(&self, mapping: &HlsVariantMapping, rep_id: &str) -> Result<String, PyError> {
        let started = Instant::now();
        // Its probe can raise on an unreadable fragment: that is a FAIL, never a crash.
        let status = match self
            .deps
            .manifests
            .check_extinf_gate(self.deps.runner, mapping)
        {
            Ok(status) => status,
            Err(error) if error.is_runtime() => format!("FAIL: EXTINF probe error: {error}"),
            Err(error) => return Err(error),
        };
        let elapsed = started.elapsed().as_secs_f64();
        self.log.gate(
            CheckId::HlsExtinf,
            Some(rep_id),
            elapsed,
            &format!("EXTINF gate: {} in {elapsed:.2}s", verdict(&status)),
        );
        Ok(status)
    }
}

/// `rewrite_asset_manifests`: the DASH patch, the 6-2 audio-PCM gate, the HLS map and patch,
/// the PSSH agreement over every init segment, the content-identity and EXTINF gates per
/// mapping, and the 6-6 SKIP row. Writes only under `output/<asset_id>/manifests/`; the gates'
/// scratch (the raw-key file among it) lives in a temporary directory removed afterwards.
pub fn rewrite_asset_manifests(
    asset: &Asset,
    playready_objects: &PlayReadyObjects,
    options: &ManifestOptions<'_>,
    deps: &PipelineDeps<'_>,
) -> Result<ManifestResult, PyError> {
    let log = Log(options.on_log);
    let manifests_root = py_path(&asset.output_dir).join("manifests");
    // : manifests/ mirrors the source host + path structure under the download root.
    let relative = relative_to(
        &py_resolve(&asset.mpd_path)?,
        &py_resolve(&asset.input_base_dir)?,
    )?;
    let dash_output_mpd = py_path(&manifests_root.join(relative));
    #[cfg(windows)]
    let _output_guard = crate::native::pin_private_directory(&asset.output_dir)
        .map_err(|error| PyError::os(&error, &asset.output_dir))?;

    log.info("manifest", "patching DASH manifest", None);
    let dash_result = deps.manifests.rewrite_dash_manifest(
        &asset.mpd_path,
        &dash_output_mpd,
        playready_objects,
        None,
    )?;
    let mut checks = dash_result.checks.clone();

    #[cfg(unix)]
    let scratch = tempfile::Builder::new()
        .prefix("drmr-manifest-gates-")
        .tempdir()
        .map_err(|error| PyError::os(&error, &std::env::temp_dir()))?;
    #[cfg(windows)]
    let scratch = crate::native::private_tempdir()
        .map_err(|error| PyError::os(&error, &std::env::temp_dir()))?;
    let scratch_path = scratch.path().to_path_buf();
    let run = ManifestRun {
        asset,
        playready_objects,
        options,
        deps,
        log,
        hls_master_path: py_path(options.hls_master_path),
        manifests_root,
        dash_output_mpd,
        work_dir: scratch_path,
    };
    run.audio_pcm_gate(&mut checks)?;
    let hls_result = run.hls(&mut checks)?;
    run.pssh_gate(&mut checks)?;
    run.mapping_gates(&hls_result, &mut checks)?;
    let scratch_path = scratch.path().to_path_buf();
    scratch
        .close()
        .map_err(|error| PyError::os(&error, &scratch_path))?;

    // 6-6 needs a validator that is not integrated: an explicit SKIP row, never absent.
    set_check(
        &mut checks,
        CheckId::DashConformance,
        "SKIP: DASH-IF validator not yet integrated",
    );
    log.fails(&checks, None);
    log.info(
        "report",
        &format!(
            "manifests: {}",
            if rendition_all_pass(&checks) {
                "PASS"
            } else {
                "FAIL"
            }
        ),
        None,
    );
    Ok(ManifestResult {
        dash: dash_result,
        hls: hls_result,
        checks,
    })
}

/// `os.cpu_count`: the logical CPUs of the system (not the affinity-aware count Python's
/// `os.process_cpu_count` and Rust's `available_parallelism` give on Linux).
pub fn cpu_count() -> Option<usize> {
    #[cfg(target_os = "linux")]
    if let Some(count) = linux_online_cpus() {
        return Some(count);
    }
    std::thread::available_parallelism()
        .ok()
        .map(std::num::NonZero::get)
}

/// `/sys/devices/system/cpu/online` (`0-3,5`), what `sysconf(_SC_NPROCESSORS_ONLN)` reads.
#[cfg(target_os = "linux")]
fn linux_online_cpus() -> Option<usize> {
    let text = fs::read_to_string("/sys/devices/system/cpu/online").ok()?;
    let mut count = 0;
    for range in text.trim().split(',') {
        let (low, high) = range.split_once('-').unwrap_or((range, range));
        let (low, high): (usize, usize) = (low.parse().ok()?, high.parse().ok()?);
        count += high.checked_sub(low)? + 1;
    }
    (count > 0).then_some(count)
}

/// `_resolve_max_workers`: the rendition pool bound — one worker per rendition capped
/// at the CPU count by default; an explicit value clamped to the rendition count; at least 1.
pub fn resolve_max_workers(
    max_workers: Option<i64>,
    rendition_count: usize,
) -> Result<usize, PyError> {
    if let Some(requested) = max_workers {
        if requested < 1 {
            return Err(PyError::Value(format!(
                "max_workers must be >= 1, got {requested}"
            )));
        }
        let requested = usize::try_from(requested).unwrap_or(usize::MAX);
        return Ok(requested.min(rendition_count).max(1));
    }
    Ok(rendition_count.min(cpu_count().unwrap_or(4)).max(1))
}

/// A sink whose emits never overlap (`_synchronized_log_sink`): concurrent renditions cannot
/// enter the caller's sink at once.
struct SynchronizedSink<'a> {
    inner: &'a dyn Sink,
    lock: Mutex<()>,
}

impl Sink for SynchronizedSink<'_> {
    fn emit(&self, record: &Record) {
        let _guard = lock(&self.lock);
        self.inner.emit(record);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One `(rendition_id, rel_dir)` value as a `Path`'s parts (`PurePosixPath.parts`).
fn py_parts(path: &Path) -> Vec<String> {
    py_path(path)
        .components()
        .filter(|component| *component != Component::CurDir)
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// `PurePosixPath.parents` as parts: nearest first, ending at `.` (no parts) or `/`.
fn py_parents(parts: &[String]) -> Vec<&[String]> {
    let floor = usize::from(parts.first().is_some_and(|part| part == "/"));
    (floor..parts.len())
        .rev()
        .map(|end| &parts[..end])
        .collect()
}

/// `_assert_disjoint_output_targets`: on the concurrent path every rendition must own a
/// distinct output subtree — no duplicate and no nested `rep_id` or `rel_dir` (nesting judged on
/// whole components). A loud `ValueError` before any rendition starts.
pub fn assert_disjoint_output_targets(renditions: &[Rendition]) -> Result<(), PyError> {
    const PREFIX: &str =
        "renditions must write to distinct output targets for safe concurrent processing";
    for attr in ["rep_id", "rel_dir"] {
        let mut targets: Vec<(Vec<String>, String)> = Vec::new();
        for rendition in renditions {
            let (target, shown) = if attr == "rep_id" {
                (
                    py_parts(Path::new(&rendition.rep_id)),
                    py_repr(&rendition.rep_id),
                )
            } else {
                (
                    py_parts(&rendition.rel_dir),
                    format!("PosixPath({})", py_repr(&py_str(&rendition.rel_dir))),
                )
            };
            if targets.iter().any(|(known, _)| *known == target) {
                return Err(PyError::Value(format!(
                    "{PREFIX}: duplicate {attr}={shown}"
                )));
            }
            targets.push((target, shown));
        }
        for (target, shown) in &targets {
            for ancestor in py_parents(target) {
                if let Some((_, outer)) = targets.iter().find(|(known, _)| known == ancestor) {
                    return Err(PyError::Value(format!(
                        "{PREFIX}: {attr}={shown} is nested inside {attr}={outer}"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// `_FailFastGate`: once the rendition at index `i` has failed, no rendition after `i` may
/// start (the sequential loop would never have reached it); renditions before `i` are never
/// refused. That keeps the propagated error the sequential path's.
#[derive(Debug)]
pub struct FailFastGate {
    first_failed_index: Mutex<usize>,
}

impl FailFastGate {
    /// A gate over `rendition_count` renditions, nothing failed yet.
    pub fn new(rendition_count: usize) -> Self {
        FailFastGate {
            first_failed_index: Mutex::new(rendition_count),
        }
    }

    /// False iff a rendition earlier than `index` has failed.
    pub fn may_start(&self, index: usize) -> bool {
        index <= *lock(&self.first_failed_index)
    }

    /// The rendition at `index` failed; only the earliest failure counts.
    pub fn record_failure(&self, index: usize) {
        let mut first = lock(&self.first_failed_index);
        *first = (*first).min(index);
    }
}

/// How one queued rendition ended.
enum Outcome {
    Done(RenditionResult),
    Raised(PyError),
    Panicked(Box<dyn Any + Send>),
    /// Turned away by the start gate: an earlier rendition had failed.
    Refused,
}

struct PoolState {
    outcomes: Vec<Option<Outcome>>,
    settled: usize,
    failed: bool,
}

/// Run one rendition behind the gate, recording a failure (an error or a panic: Python's
/// `except BaseException`) before anything else can dequeue.
fn run_gated(
    gate: &FailFastGate,
    index: usize,
    run: impl FnOnce() -> Result<RenditionResult, PyError>,
) -> Outcome {
    if !gate.may_start(index) {
        return Outcome::Refused;
    }
    match catch_unwind(AssertUnwindSafe(run)) {
        Ok(Ok(result)) => Outcome::Done(result),
        Ok(Err(error)) => {
            gate.record_failure(index);
            Outcome::Raised(error)
        }
        Err(payload) => {
            gate.record_failure(index);
            Outcome::Panicked(payload)
        }
    }
}

/// The pool: `workers` threads take renditions in order. The main thread wakes on the
/// first failure (`wait(FIRST_EXCEPTION)`) and cancels what is still queued; the start gate
/// turns away anything dequeued after an earlier failure; renditions in flight drain (the
/// scope joins them); then the first failure in rendition order is raised. Results come back
/// in `renditions` order, whatever the completion order.
fn run_pool(
    renditions: &[Rendition],
    workers: usize,
    run: &(dyn Fn(&Rendition) -> Result<RenditionResult, PyError> + Sync),
) -> Result<Vec<RenditionResult>, PyError> {
    let count = renditions.len();
    let gate = FailFastGate::new(count);
    let next = AtomicUsize::new(0);
    let cancelled = AtomicBool::new(false);
    let state = Mutex::new(PoolState {
        outcomes: (0..count).map(|_| None).collect(),
        settled: 0,
        failed: false,
    });
    let changed = Condvar::new();
    let spawn_error = std::thread::scope(|scope| {
        let worker = || {
            loop {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                let index = next.fetch_add(1, Ordering::SeqCst);
                if index >= count {
                    break;
                }
                let outcome = run_gated(&gate, index, || run(&renditions[index]));
                let failed = matches!(outcome, Outcome::Raised(_) | Outcome::Panicked(_));
                let mut shared = lock(&state);
                shared.outcomes[index] = Some(outcome);
                shared.settled += 1;
                shared.failed |= failed;
                drop(shared);
                changed.notify_all();
            }
        };
        for number in 0..workers {
            let spawned = std::thread::Builder::new()
                .name(format!("rendition_{number}"))
                .spawn_scoped(scope, worker);
            if let Err(error) = spawned {
                cancelled.store(true, Ordering::SeqCst);
                return Some(error);
            }
        }
        let mut shared = lock(&state);
        while !shared.failed && shared.settled < count {
            shared = changed.wait(shared).unwrap_or_else(PoisonError::into_inner);
        }
        if shared.failed {
            cancelled.store(true, Ordering::SeqCst); // cancel_futures: nothing queued starts
        }
        None
    });
    if let Some(error) = spawn_error {
        return Err(PyError::Runtime(format!("can't start new thread: {error}")));
    }
    let outcomes = state
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .outcomes;
    let mut results = Vec::with_capacity(count);
    for outcome in outcomes {
        match outcome {
            Some(Outcome::Done(result)) => results.push(result),
            Some(Outcome::Raised(error)) => return Err(error),
            Some(Outcome::Panicked(payload)) => resume_unwind(payload),
            Some(Outcome::Refused) | None => {}
        }
    }
    Ok(results)
}

/// The `--pssh` set of each rendition: the given boxes, then the regenerated `PlayReady` box
/// (appended once per rendition).
fn pssh_by_rendition(asset: &Asset, options: &AssetOptions<'_>) -> HashMap<String, Vec<String>> {
    let mut pssh_by_id: HashMap<String, Vec<String>> = options
        .pssh_hex_by_rendition
        .map(|given| {
            given
                .iter()
                .map(|(id, boxes)| (id.clone(), boxes.clone()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(objects) = options.playready_objects {
        for rendition in &asset.renditions {
            pssh_by_id
                .entry(rendition.rep_id.clone())
                .or_default()
                .push(objects.pssh_hex.clone());
        }
    }
    pssh_by_id
}

/// `repackage_asset`: every rendition of ONE asset, concurrently within the
/// bound, then the manifest rewrite when `hls_master_path` and `playready_objects` are both
/// given, then the reports. An environmental fault aborts the asset: no result, no report.
pub fn repackage_asset(
    asset: &Asset,
    options: &AssetOptions<'_>,
    deps: &PipelineDeps<'_>,
) -> Result<AssetResult, PyError> {
    #[cfg(windows)]
    let _output_guard = {
        let workers = resolve_max_workers(options.max_workers, asset.renditions.len())?;
        if workers > 1 && asset.renditions.len() > 1 {
            assert_disjoint_output_targets(&asset.renditions)?;
        }
        crate::native::pin_private_directory(&asset.output_dir)
            .map_err(|error| PyError::os(&error, &asset.output_dir))?
    };
    let step = |rendition: &Rendition,
                key: &KeyMaterial,
                output_dir: &Path,
                rendition_options: &RenditionOptions<'_>| {
        repackage_rendition(rendition, key, output_dir, rendition_options, deps)
    };
    repackage_asset_with(asset, options, deps, &step)
}

/// [`repackage_asset`] with the per-rendition step injected (the seam the tests
/// dependency injection to observe the pool: ordering, bound, fail-fast).
pub fn repackage_asset_with(
    asset: &Asset,
    options: &AssetOptions<'_>,
    deps: &PipelineDeps<'_>,
    step: &RenditionStep<'_>,
) -> Result<AssetResult, PyError> {
    let pssh_by_id = pssh_by_rendition(asset, options);
    let workers = resolve_max_workers(options.max_workers, asset.renditions.len())?;
    let run_one = |rendition: &Rendition, sink: Option<&dyn Sink>| {
        let rendition_options = RenditionOptions {
            pssh_hex: pssh_by_id.get(&rendition.rep_id).map(Vec::as_slice),
            keep_clear: true,
            verify_round_trip: options.verify_round_trip,
            on_log: sink,
        };
        step(rendition, &asset.key, &asset.output_dir, &rendition_options)
    };
    let results = if workers > 1 && asset.renditions.len() > 1 {
        // Disjoint writes are the precondition of concurrency: checked before anything runs.
        assert_disjoint_output_targets(&asset.renditions)?;
        let synchronized = options.on_log.map(|inner| SynchronizedSink {
            inner,
            lock: Mutex::new(()),
        });
        let sink = synchronized.as_ref().map(|sink| sink as &dyn Sink);
        run_pool(&asset.renditions, workers, &|rendition: &Rendition| {
            run_one(rendition, sink)
        })?
    } else {
        // The sequential path: the caller's thread, the caller's sink, stop at the first error.
        asset
            .renditions
            .iter()
            .map(|rendition| run_one(rendition, options.on_log))
            .collect::<Result<Vec<_>, _>>()?
    };

    let manifests = match (options.hls_master_path, options.playready_objects) {
        (Some(hls_master_path), Some(playready_objects)) => Some(rewrite_asset_manifests(
            asset,
            playready_objects,
            &ManifestOptions {
                rendition_results: &results,
                hls_master_path,
                local_url_base: options.manifest_local_url_base,
                on_log: options.on_log,
            },
            deps,
        )?),
        _ => None,
    };
    let all_pass = !results.is_empty()
        && results
            .iter()
            .all(|result| rendition_all_pass(&result.checks))
        && manifests
            .as_ref()
            .is_none_or(|manifests| rendition_all_pass(&manifests.checks));
    let asset_result = AssetResult {
        schema_version: SCHEMA_VERSION.to_string(),
        asset_id: asset.asset_id.clone(),
        renditions: results,
        all_pass,
        full_enumeration: options.full_enumeration,
        round_trip_verified: options.verify_round_trip,
        manifests,
    };
    write_report(&asset_result, &py_path(&asset.output_dir).join("reports"))?;
    Log(options.on_log).info(
        "report",
        &format!(
            "asset {}: {}",
            asset.asset_id,
            if all_pass { "PASS" } else { "FAIL" }
        ),
        None,
    );
    Ok(asset_result)
}

/// `_write_report`: `phase1-summary.json` (`json.dumps(..., indent=2)`) and
/// `phase1-summary.md`.
pub fn write_report(asset_result: &AssetResult, reports_dir: &Path) -> Result<(), PyError> {
    crate::media::create_output_dir(reports_dir)
        .map_err(|error| PyError::os(&error, reports_dir))?;
    let json_path = reports_dir.join("phase1-summary.json");
    crate::media::write_output_file(
        &json_path,
        Dumps::INDENT2
            .dumps(&asset_result_to_json(asset_result))
            .as_bytes(),
    )
    .map_err(|error| PyError::os(&error, &json_path))?;
    let markdown_path = reports_dir.join("phase1-summary.md");
    crate::media::write_output_file(
        &markdown_path,
        asset_result_to_markdown(asset_result).as_bytes(),
    )
    .map_err(|error| PyError::os(&error, &markdown_path))
}

fn object<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<Map<String, Value>>(),
    )
}

fn path_value(path: &Path) -> Value {
    Value::String(py_str(path))
}

fn paths_value(paths: &[PathBuf]) -> Value {
    Value::Array(paths.iter().map(|path| path_value(path)).collect())
}

fn optional<T: Into<Value> + Clone>(value: Option<&T>) -> Value {
    value.cloned().map_or(Value::Null, Into::into)
}

fn checks_value(checks: &[(CheckId, String)]) -> Value {
    Value::Object(
        checks
            .iter()
            .map(|(id, status)| (id.as_str().to_string(), Value::String(status.clone())))
            .collect(),
    )
}

fn rendition_value(rendition: &Rendition) -> Value {
    object([
        ("rep_id", rendition.rep_id.clone().into()),
        ("content_type", rendition.content_type.clone().into()),
        ("codec", rendition.codec.clone().into()),
        ("timescale", rendition.timescale.into()),
        (
            "segment_duration_ticks",
            rendition.segment_duration_ticks.into(),
        ),
        ("start_number", rendition.start_number.into()),
        ("init_path", path_value(&rendition.init_path)),
        ("segment_paths", paths_value(&rendition.segment_paths)),
        ("rel_dir", path_value(&rendition.rel_dir)),
        (
            "widevine_pssh_hex",
            optional(rendition.widevine_pssh_hex.as_ref()),
        ),
        ("width", optional(rendition.width.as_ref())),
        ("height", optional(rendition.height.as_ref())),
        ("frame_rate", optional(rendition.frame_rate.as_ref())),
        ("lang", optional(rendition.lang.as_ref())),
    ])
}

fn rendition_result_value(result: &RenditionResult) -> Value {
    object([
        ("rendition_id", result.rendition_id.clone().into()),
        ("content_type", result.content_type.clone().into()),
        ("clear_path", path_value(&result.clear_path)),
        ("cbcs_init_path", path_value(&result.cbcs_init_path)),
        (
            "cbcs_segment_paths",
            paths_value(&result.cbcs_segment_paths),
        ),
        ("source_segment_count", result.source_segment_count.into()),
        ("output_segment_count", result.output_segment_count.into()),
        (
            "number_origin_map",
            Value::Array(
                result
                    .number_origin_map
                    .iter()
                    .map(|(shaka, source)| Value::Array(vec![(*shaka).into(), (*source).into()]))
                    .collect(),
            ),
        ),
        (
            "requested_segment_duration_s",
            result.requested_segment_duration_s.into(),
        ),
        ("checks", checks_value(&result.checks)),
    ])
}

fn dash_value(dash: &DashRewriteResult) -> Value {
    object([
        ("output_mpd_path", path_value(&dash.output_mpd_path)),
        (
            "adaptation_sets_patched",
            dash.adaptation_sets_patched.into(),
        ),
        ("playready_pssh_b64", dash.playready_pssh_b64.clone().into()),
        ("playready_pro_b64", dash.playready_pro_b64.clone().into()),
        ("widevine_unchanged", dash.widevine_unchanged.into()),
        ("checks", checks_value(&dash.checks)),
    ])
}

fn mapping_value(mapping: &HlsVariantMapping) -> Value {
    object([
        ("hls_playlist", path_value(&mapping.hls_playlist)),
        ("dash_rendition", rendition_value(&mapping.dash_rendition)),
        (
            "signals",
            Value::Object(
                mapping
                    .signals
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect(),
            ),
        ),
        ("content_gate", mapping.content_gate.clone().into()),
    ])
}

fn hls_value(hls: &HlsRewriteResult) -> Value {
    object([
        ("output_master_path", path_value(&hls.output_master_path)),
        (
            "output_media_playlist_paths",
            paths_value(&hls.output_media_playlist_paths),
        ),
        (
            "audio_groups_consolidated",
            hls.audio_groups_consolidated.into(),
        ),
        (
            "ts_audio_groups_removed",
            hls.ts_audio_groups_removed.into(),
        ),
        ("ext_x_map_added", hls.ext_x_map_added.into()),
        ("segment_refs_repointed", hls.segment_refs_repointed.into()),
        ("iv_verified", hls.iv_verified.into()),
        (
            "variant_mapping",
            Value::Array(hls.variant_mapping.iter().map(mapping_value).collect()),
        ),
        ("checks", checks_value(&hls.checks)),
    ])
}

/// `_asset_result_to_json`: `asdict` with every path as `str(path)`, in field order.
pub fn asset_result_to_json(asset_result: &AssetResult) -> Value {
    let manifests = asset_result
        .manifests
        .as_ref()
        .map_or(Value::Null, |manifests| {
            object([
                ("dash", dash_value(&manifests.dash)),
                ("hls", hls_value(&manifests.hls)),
                ("checks", checks_value(&manifests.checks)),
            ])
        });
    object([
        ("schema_version", asset_result.schema_version.clone().into()),
        ("asset_id", asset_result.asset_id.clone().into()),
        (
            "renditions",
            Value::Array(
                asset_result
                    .renditions
                    .iter()
                    .map(rendition_result_value)
                    .collect(),
            ),
        ),
        ("all_pass", asset_result.all_pass.into()),
        ("full_enumeration", asset_result.full_enumeration.into()),
        (
            "round_trip_verified",
            asset_result.round_trip_verified.into(),
        ),
        ("manifests", manifests),
    ])
}

/// Python's `str(bool)`.
fn py_bool(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}

/// `_asset_result_to_markdown`.
pub fn asset_result_to_markdown(asset_result: &AssetResult) -> String {
    let mut lines = vec![
        format!("# Repackaging report: {}", asset_result.asset_id),
        String::new(),
        format!("schema_version: {}  ", asset_result.schema_version),
        format!(
            "full_enumeration: {}  ",
            py_bool(asset_result.full_enumeration)
        ),
        format!(
            "round_trip_verified: {}  ",
            py_bool(asset_result.round_trip_verified)
        ),
        format!("all_pass: {}", py_bool(asset_result.all_pass)),
        String::new(),
    ];
    for result in &asset_result.renditions {
        lines.push(format!(
            "## {} ({})",
            result.rendition_id, result.content_type
        ));
        lines.push(String::new());
        lines.push(format!(
            "- segments: source={} output={}",
            result.source_segment_count, result.output_segment_count
        ));
        lines.push(format!(
            "- requested duration: {}s",
            python_float_repr(result.requested_segment_duration_s)
        ));
        for (id, status) in sorted_checks(&result.checks) {
            lines.push(format!("- `{id}`: {status}"));
        }
        lines.push(String::new());
    }
    if let Some(manifests) = &asset_result.manifests {
        lines.push("## manifests".to_string());
        lines.push(String::new());
        lines.push(format!(
            "- dash: `{}`",
            py_str(&manifests.dash.output_mpd_path)
        ));
        lines.push(format!(
            "- hls master: `{}`",
            py_str(&manifests.hls.output_master_path)
        ));
        for (id, status) in sorted_checks(&manifests.checks) {
            lines.push(format!("- `{id}`: {status}"));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}
