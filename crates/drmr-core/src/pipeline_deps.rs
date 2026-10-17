//! Dependency interfaces for catalog configuration, media, signaling, and manifests.
//!
//! Production adapters call the core modules; tests inject stand-ins. Tool-running
//! methods receive the job's shared runner, secret registry, and search path explicitly.
//! Error classification lets command handlers distinguish configuration failures
//! from operational failures.

use crate::config::{self, Asset, ConfigError, KeyMaterial, Rendition};
use crate::manifest_dash::{self, DashError, DashRewriteResult};
use crate::manifest_hls::{self, GateMedia, HlsError, HlsRewriteResult, HlsVariantMapping};
use crate::manifest_support::py::PyErr;
use crate::media::{self, FragmentBoundary, HexSecret, MediaEngine, MediaError};
use crate::packager::{self, PackagerResult};
use crate::playready::{self, ParsedPro, PlayReadyError, PlayReadyObjects};
use crate::toolrun::ToolRunner;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The Python exceptions the pipeline and the CLI tell apart. `Display` is `str(exc)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PyError {
    /// `ValueError` and its subclasses (`json.JSONDecodeError`, `UnicodeDecodeError`,
    /// `DashRewriteError`, `HlsRewriteError`, `PlayReadyError`): the CLI maps it to exit 2.
    Value(String),
    /// `RuntimeError`, `toolrun.ToolError` included: the checks that catch it record
    /// `str(exc)` in their status.
    Runtime(String),
    /// `packager.PackagerError` (a spawn failure or a timeout of Shaka Packager): an
    /// environmental fault that aborts the asset; the CLI maps it to exit 2. A `RuntimeError`
    /// subclass in Python ([`PyError::is_runtime`]).
    Packager(String),
    /// `OSError` and its subclasses: never caught by the pipeline or the CLI (exit 1).
    Os(String),
    /// Any other exception (`ZeroDivisionError`, `KeyError`, `ParseError`, …): never caught.
    Other {
        /// The exception class name.
        kind: String,
        /// `str(exc)`.
        message: String,
    },
}

impl PyError {
    /// The exception class name as a traceback's last line names it.
    pub fn kind(&self) -> &str {
        match self {
            PyError::Value(_) => "ValueError",
            PyError::Runtime(_) => "RuntimeError",
            PyError::Packager(_) => "PackagerError",
            PyError::Os(_) => "OSError",
            PyError::Other { kind, .. } => kind,
        }
    }

    /// `str(exc)`.
    pub fn message(&self) -> &str {
        match self {
            PyError::Value(message)
            | PyError::Runtime(message)
            | PyError::Packager(message)
            | PyError::Os(message)
            | PyError::Other { message, .. } => message,
        }
    }

    /// Whether `except RuntimeError` catches it (`PackagerError` is a `RuntimeError`).
    pub fn is_runtime(&self) -> bool {
        matches!(self, PyError::Runtime(_) | PyError::Packager(_))
    }

    /// `OSError` from an I/O failure on `path`, worded like Python's
    /// (`[Errno 2] No such file or directory: '<path>'`).
    pub fn os(error: &std::io::Error, path: &Path) -> PyError {
        let text = error.to_string();
        let message = match error.raw_os_error() {
            Some(code) => {
                let description = text
                    .strip_suffix(&format!(" (os error {code})"))
                    .unwrap_or(&text);
                format!("[Errno {code}] {description}: '{}'", path.display())
            }
            None => format!("{text}: '{}'", path.display()),
        };
        PyError::Os(message)
    }

    /// `OSError` from an I/O failure whose file name the failing module did not keep
    /// (`[Errno 2] No such file or directory`).
    pub fn os_unnamed(error: &std::io::Error) -> PyError {
        let text = error.to_string();
        PyError::Os(match error.raw_os_error() {
            Some(code) => {
                let description = text
                    .strip_suffix(&format!(" (os error {code})"))
                    .unwrap_or(&text);
                format!("[Errno {code}] {description}")
            }
            None => text,
        })
    }

    fn other(kind: &str, message: impl Into<String>) -> PyError {
        PyError::Other {
            kind: kind.to_string(),
            message: message.into(),
        }
    }
}

impl fmt::Display for PyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PyError {}

/// `config`: a malformed manifest is `ParseError` (a `SyntaxError`, not a `ValueError`), a
/// symlink loop `RuntimeError`, a zero timescale `ZeroDivisionError`.
impl From<ConfigError> for PyError {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::Value(message) => PyError::Value(message),
            ConfigError::Xml(message) => PyError::other("ParseError", message),
            ConfigError::Runtime(message) => PyError::Runtime(message),
            ConfigError::ZeroDivision => PyError::other("ZeroDivisionError", "division by zero"),
            ConfigError::Io(error) => PyError::os_unnamed(&error),
        }
    }
}

/// `packager`: `PackagerError` (spawn failure, timeout), `ValueError`, `OSError`.
impl From<packager::Error> for PyError {
    fn from(error: packager::Error) -> Self {
        match error {
            packager::Error::Packager(error) => PyError::Packager(error.message().to_string()),
            packager::Error::Value(message) => PyError::Value(message),
            packager::Error::Io(error) => PyError::os_unnamed(&error),
        }
    }
}

/// `media_inspect`: its tool failures are `RuntimeError`s (the class the checks catch).
impl From<MediaError> for PyError {
    fn from(error: MediaError) -> Self {
        match error {
            MediaError::Hls(_) | MediaError::Runtime(_) | MediaError::Tool(_) => {
                PyError::Runtime(error.to_string())
            }
            MediaError::Value(message) => PyError::Value(message),
            MediaError::Key(message) => PyError::other("KeyError", message),
            MediaError::Os(error) => PyError::os_unnamed(&error),
            MediaError::Uncaught { class, message } => PyError::other(class, message),
        }
    }
}

/// The manifest modules' built-in exceptions.
impl From<PyErr> for PyError {
    fn from(error: PyErr) -> Self {
        match error {
            PyErr::Value(message) | PyErr::UnicodeDecode(message) => PyError::Value(message),
            PyErr::Runtime(message) => PyError::Runtime(message),
            PyErr::Os(error) => PyError::Os(error.message),
            other => PyError::other(other.class_name(), other.to_string()),
        }
    }
}

/// `DashRewriteError` and `PlayReadyError` are `ValueError`s.
impl From<DashError> for PyError {
    fn from(error: DashError) -> Self {
        match error {
            DashError::Rewrite(error) => PyError::Value(error.to_string()),
            DashError::PlayReady(error) => PyError::Value(error.to_string()),
            DashError::Py(error) => error.into(),
        }
    }
}

/// `HlsRewriteError` is a `ValueError`.
impl From<HlsError> for PyError {
    fn from(error: HlsError) -> Self {
        match error {
            HlsError::Rewrite(error) => PyError::Value(error.to_string()),
            HlsError::Py(error) => error.into(),
        }
    }
}

/// `PlayReadyError` is a `ValueError`.
impl From<PlayReadyError> for PyError {
    fn from(error: PlayReadyError) -> Self {
        PyError::Value(error.message().to_string())
    }
}

/// Check ids with their status strings in insertion order (a Python `dict[str, str]`).
pub type Checks = Vec<(crate::checks::CheckId, String)>;

/// `config`: key material and the asset model.
pub trait Config: Send + Sync {
    /// `load_key_material(env)`: `DRM_KID` / `DRM_KEY` / `DRM_IV` from `env` (an empty value
    /// counts as missing); `ValueError` when one is missing or not 16 bytes of hex.
    fn load_key_material(&self, env: &HashMap<String, String>) -> Result<KeyMaterial, PyError>;

    /// `load_asset(mpd_path, key, output_root, *, asset_id=None)`: the asset with its renditions
    /// enumerated from the manifest. An empty `asset_id` is Python's falsy value: the
    /// id is then derived from the manifest, as for `None`.
    fn load_asset(
        &self,
        mpd_path: &Path,
        key: KeyMaterial,
        output_root: &Path,
        asset_id: Option<&str>,
    ) -> Result<Asset, PyError>;
}

/// `packager.encrypt_cbcs(input_mp4, init_output, segment_template, key, *,
/// segment_duration_seconds, pssh_hex=None, packager_bin="packager")`.
#[derive(Clone, Copy, Debug)]
pub struct EncryptCbcs<'a> {
    /// The clear input.
    pub input_mp4: &'a Path,
    /// The CBCS init segment to write.
    pub init_output: &'a Path,
    /// Shaka's 1-origin staging template (`…/seg_$Number$.m4s`).
    pub segment_template: &'a str,
    /// The key material.
    pub key: &'a KeyMaterial,
    /// `--segment_duration`, derived from the source (6 s; never 4).
    pub segment_duration_seconds: f64,
    /// The `pssh` boxes as hex, concatenated into one `--pssh`; none when `None` or empty.
    pub pssh_hex: Option<&'a [String]>,
    /// The packager binary.
    pub packager_bin: &'a str,
}

/// `packager.transcrypt_cbcs_per_segment(init_path, segment_paths, init_output,
/// segment_template, key, *, pssh_hex=None, packager_bin="packager", work_dir=None)`.
#[derive(Clone, Copy, Debug)]
pub struct TranscryptCbcsPerSegment<'a> {
    /// The source init segment.
    pub init_path: &'a Path,
    /// The source segments.
    pub segment_paths: &'a [PathBuf],
    /// The CBCS init segment to write.
    pub init_output: &'a Path,
    /// Shaka's 1-origin staging template.
    pub segment_template: &'a str,
    /// The key material.
    pub key: &'a KeyMaterial,
    /// The `pssh` boxes as hex.
    pub pssh_hex: Option<&'a [String]>,
    /// The packager binary.
    pub packager_bin: &'a str,
    /// The scratch directory of the per-segment runs.
    pub work_dir: Option<&'a Path>,
}

/// `packager`: the Shaka Packager wrapper.
pub trait Packager: Send + Sync {
    /// `merge_segments(init_path, segment_paths, output_path)`: `init || seg_0 || …` in numeric
    /// order; returns `output_path`.
    fn merge_segments(
        &self,
        init_path: &Path,
        segment_paths: &[PathBuf],
        output_path: &Path,
    ) -> Result<PathBuf, PyError>;

    /// `renumber_to_source_origin(staging_dir, source_start_number)` with the default
    /// `prefix="seg_"`, `suffix=".m4s"`: the `{shaka_number: source_number}` map, in
    /// ascending Shaka order; `ValueError` when the staged numbers are not `1..N`.
    fn renumber_to_source_origin(
        &self,
        staging_dir: &Path,
        source_start_number: i64,
    ) -> Result<Vec<(i64, i64)>, PyError>;

    /// `decrypt_cenc(input_mp4, output_mp4, key, *, packager_bin)`.
    fn decrypt_cenc(
        &self,
        runner: &Arc<ToolRunner>,
        input_mp4: &Path,
        output_mp4: &Path,
        key: &KeyMaterial,
        packager_bin: &str,
    ) -> Result<PackagerResult, PyError>;

    /// `encrypt_cbcs(...)`.
    fn encrypt_cbcs(
        &self,
        runner: &Arc<ToolRunner>,
        call: &EncryptCbcs<'_>,
    ) -> Result<PackagerResult, PyError>;

    /// `transcrypt_cbcs_per_segment(...)`.
    fn transcrypt_cbcs_per_segment(
        &self,
        runner: &Arc<ToolRunner>,
        call: &TranscryptCbcsPerSegment<'_>,
    ) -> Result<PackagerResult, PyError>;
}

/// `media_inspect`: the ffmpeg / mp4dump readers behind the 6-1 and 6-4 checks (the default
/// `ffmpeg` / `mp4dump` binaries; the pipeline never overrides them).
pub trait MediaInspect: Send + Sync {
    /// `compute_framemd5(path)`: ffmpeg's `-f framemd5` report; `RuntimeError` on a failed run.
    fn compute_framemd5(&self, runner: &Arc<ToolRunner>, path: &Path) -> Result<String, PyError>;

    /// `read_tenc_constant_iv_hex(init_segment_path)`: the `tenc` constant IV as lower-case
    /// hex (key material: compared, never printed), `None` without one; `RuntimeError` on a
    /// failed mp4dump.
    fn read_tenc_constant_iv_hex(
        &self,
        runner: &Arc<ToolRunner>,
        init_segment_path: &Path,
    ) -> Result<Option<HexSecret>, PyError>;

    /// `read_fragment_boundary(segment_path)`: `None` when the file is not a fragment;
    /// `RuntimeError` on a failed mp4dump.
    fn read_fragment_boundary(
        &self,
        runner: &Arc<ToolRunner>,
        segment_path: &Path,
    ) -> Result<Option<FragmentBoundary>, PyError>;

    /// `pssh_box_present(init_segment_path, pssh_box)`: whether the exact bytes appear in the
    /// init segment.
    fn pssh_box_present(&self, init_segment_path: &Path, pssh_box: &[u8]) -> Result<bool, PyError>;
}

/// `manifest_dash` and `manifest_hls`: the surgical manifest patches and the gates.
pub trait Manifests: Send + Sync {
    /// `manifest_dash.parse_source_playready(mpd_path)`.
    fn parse_source_playready(&self, mpd_path: &Path) -> Result<ParsedPro, PyError>;

    /// `manifest_dash.rewrite_dash_manifest(source_mpd, output_mpd, playready, *, url_base)`.
    fn rewrite_dash_manifest(
        &self,
        source_mpd: &Path,
        output_mpd: &Path,
        playready: &PlayReadyObjects,
        url_base: Option<&str>,
    ) -> Result<DashRewriteResult, PyError>;

    /// `manifest_hls.find_audio_consolidation_pairs(master_path)`: `(ts, fmp4)` playlist pairs.
    fn find_audio_consolidation_pairs(
        &self,
        master_path: &Path,
    ) -> Result<Vec<(PathBuf, PathBuf)>, PyError>;

    /// `manifest_hls.audio_pcm_consolidation_gate(ts_playlist_path, fmp4_playlist_path, key,
    /// work_dir)`: a status string.
    fn audio_pcm_consolidation_gate(
        &self,
        runner: &Arc<ToolRunner>,
        ts_playlist_path: &Path,
        fmp4_playlist_path: &Path,
        key: &KeyMaterial,
        work_dir: &Path,
    ) -> Result<String, PyError>;

    /// `manifest_hls.rewrite_hls(master_path, dash_renditions, output_root, *, expected_iv_hex,
    /// url_base)`.
    fn rewrite_hls(
        &self,
        master_path: &Path,
        dash_renditions: &[Rendition],
        output_root: &Path,
        expected_iv_hex: &str,
        url_base: Option<&str>,
    ) -> Result<HlsRewriteResult, PyError>;

    /// `manifest_hls.content_identity_gate(mapping, dash_clear_path, key, work_dir)`.
    fn content_identity_gate(
        &self,
        runner: &Arc<ToolRunner>,
        mapping: &HlsVariantMapping,
        dash_clear_path: &Path,
        key: &KeyMaterial,
        work_dir: &Path,
    ) -> Result<String, PyError>;

    /// `manifest_hls.check_extinf_gate(mapping)` (default tolerance); `RuntimeError` when its
    /// probe fails.
    fn check_extinf_gate(
        &self,
        runner: &Arc<ToolRunner>,
        mapping: &HlsVariantMapping,
    ) -> Result<String, PyError>;
}

/// `playready`: the in-house PRO generation.
pub trait PlayReady: Send + Sync {
    /// `build_playready_v43(kid, la_url)`; `la_url` is `ParsedPro.la_url`, so it may be
    /// missing — `PlayReadyError` (a `ValueError`) then, as for an empty one.
    fn build_playready_v43(
        &self,
        kid: &str,
        la_url: Option<&str>,
    ) -> Result<PlayReadyObjects, PyError>;
}

/// The real modules behind every trait above: what the `drmr` binary runs. Each call that
/// runs a tool builds its engine over the job's runner — Shaka Packager as
/// [`packager::Packager`] (`packager_bin` as given, no timeout: Python's
/// `_SUBPROCESS_TIMEOUT_S`), the media readers as [`media::Subprocess`] (the default
/// `ffmpeg` / `ffprobe` / `mp4dump`, as the pipeline calls them).
#[derive(Clone, Copy, Debug, Default)]
pub struct Ports;

fn shaka(runner: &Arc<ToolRunner>, packager_bin: &str) -> packager::Packager {
    packager::Packager::new(Arc::clone(runner)).with_packager_bin(packager_bin)
}

/// `pssh_hex` as the packager interface takes it: `None` and an empty list both emit no `--pssh`.
fn pssh_list(pssh_hex: Option<&[String]>) -> Vec<&str> {
    pssh_hex
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect()
}

impl Config for Ports {
    fn load_key_material(&self, env: &HashMap<String, String>) -> Result<KeyMaterial, PyError> {
        Ok(config::load_key_material(Some(env))?)
    }

    fn load_asset(
        &self,
        mpd_path: &Path,
        key: KeyMaterial,
        output_root: &Path,
        asset_id: Option<&str>,
    ) -> Result<Asset, PyError> {
        Ok(config::load_asset(mpd_path, key, output_root, asset_id)?)
    }
}

impl Packager for Ports {
    fn merge_segments(
        &self,
        init_path: &Path,
        segment_paths: &[PathBuf],
        output_path: &Path,
    ) -> Result<PathBuf, PyError> {
        Ok(packager::merge_segments(
            init_path,
            segment_paths,
            output_path,
        )?)
    }

    fn renumber_to_source_origin(
        &self,
        staging_dir: &Path,
        source_start_number: i64,
    ) -> Result<Vec<(i64, i64)>, PyError> {
        let mapping = packager::renumber_to_source_origin(staging_dir, source_start_number)?;
        Ok(mapping.into_iter().collect())
    }

    fn decrypt_cenc(
        &self,
        runner: &Arc<ToolRunner>,
        input_mp4: &Path,
        output_mp4: &Path,
        key: &KeyMaterial,
        packager_bin: &str,
    ) -> Result<PackagerResult, PyError> {
        Ok(shaka(runner, packager_bin).decrypt_cenc(input_mp4, output_mp4, key)?)
    }

    fn encrypt_cbcs(
        &self,
        runner: &Arc<ToolRunner>,
        call: &EncryptCbcs<'_>,
    ) -> Result<PackagerResult, PyError> {
        Ok(shaka(runner, call.packager_bin).encrypt_cbcs(
            call.input_mp4,
            call.init_output,
            call.segment_template,
            call.key,
            call.segment_duration_seconds,
            &pssh_list(call.pssh_hex),
        )?)
    }

    fn transcrypt_cbcs_per_segment(
        &self,
        runner: &Arc<ToolRunner>,
        call: &TranscryptCbcsPerSegment<'_>,
    ) -> Result<PackagerResult, PyError> {
        Ok(
            shaka(runner, call.packager_bin).transcrypt_cbcs_per_segment(
                call.init_path,
                call.segment_paths,
                call.init_output,
                call.segment_template,
                call.key,
                &pssh_list(call.pssh_hex),
                call.work_dir,
            )?,
        )
    }
}

impl MediaInspect for Ports {
    fn compute_framemd5(&self, runner: &Arc<ToolRunner>, path: &Path) -> Result<String, PyError> {
        Ok(media::Subprocess::new(Arc::clone(runner)).compute_framemd5(path)?)
    }

    fn read_tenc_constant_iv_hex(
        &self,
        runner: &Arc<ToolRunner>,
        init_segment_path: &Path,
    ) -> Result<Option<HexSecret>, PyError> {
        Ok(media::Subprocess::new(Arc::clone(runner))
            .read_tenc_constant_iv_hex(init_segment_path)?)
    }

    fn read_fragment_boundary(
        &self,
        runner: &Arc<ToolRunner>,
        segment_path: &Path,
    ) -> Result<Option<FragmentBoundary>, PyError> {
        Ok(media::Subprocess::new(Arc::clone(runner)).read_fragment_boundary(segment_path)?)
    }

    fn pssh_box_present(&self, init_segment_path: &Path, pssh_box: &[u8]) -> Result<bool, PyError> {
        Ok(media::pssh_box_present(init_segment_path, pssh_box)?)
    }
}

impl Manifests for Ports {
    fn parse_source_playready(&self, mpd_path: &Path) -> Result<ParsedPro, PyError> {
        Ok(manifest_dash::parse_source_playready(mpd_path)?)
    }

    fn rewrite_dash_manifest(
        &self,
        source_mpd: &Path,
        output_mpd: &Path,
        playready: &PlayReadyObjects,
        url_base: Option<&str>,
    ) -> Result<DashRewriteResult, PyError> {
        Ok(manifest_dash::rewrite_dash_manifest(
            source_mpd, output_mpd, playready, url_base,
        )?)
    }

    fn find_audio_consolidation_pairs(
        &self,
        master_path: &Path,
    ) -> Result<Vec<(PathBuf, PathBuf)>, PyError> {
        Ok(manifest_hls::find_audio_consolidation_pairs(master_path)?)
    }

    fn audio_pcm_consolidation_gate(
        &self,
        runner: &Arc<ToolRunner>,
        ts_playlist_path: &Path,
        fmp4_playlist_path: &Path,
        key: &KeyMaterial,
        work_dir: &Path,
    ) -> Result<String, PyError> {
        let engine = media::Subprocess::new(Arc::clone(runner));
        Ok(manifest_hls::audio_pcm_consolidation_gate(
            ts_playlist_path,
            fmp4_playlist_path,
            key,
            work_dir,
            &GateEngine(&engine),
        )?)
    }

    fn rewrite_hls(
        &self,
        master_path: &Path,
        dash_renditions: &[Rendition],
        output_root: &Path,
        expected_iv_hex: &str,
        url_base: Option<&str>,
    ) -> Result<HlsRewriteResult, PyError> {
        Ok(manifest_hls::rewrite_hls(
            master_path,
            dash_renditions,
            output_root,
            expected_iv_hex,
            url_base,
        )?)
    }

    fn content_identity_gate(
        &self,
        runner: &Arc<ToolRunner>,
        mapping: &HlsVariantMapping,
        dash_clear_path: &Path,
        key: &KeyMaterial,
        work_dir: &Path,
    ) -> Result<String, PyError> {
        let engine = media::Subprocess::new(Arc::clone(runner));
        Ok(manifest_hls::content_identity_gate(
            mapping,
            dash_clear_path,
            key,
            work_dir,
            &GateEngine(&engine),
        )?)
    }

    fn check_extinf_gate(
        &self,
        runner: &Arc<ToolRunner>,
        mapping: &HlsVariantMapping,
    ) -> Result<String, PyError> {
        let engine = media::Subprocess::new(Arc::clone(runner));
        Ok(manifest_hls::check_extinf_gate(
            mapping,
            manifest_hls::EXTINF_TOLERANCE_S,
            &GateEngine(&engine),
        )?)
    }
}

impl PlayReady for Ports {
    fn build_playready_v43(
        &self,
        kid: &str,
        la_url: Option<&str>,
    ) -> Result<PlayReadyObjects, PyError> {
        // `if not la_url` rejects `None` and `""` alike (PlayReadyError: LA_URL must be
        // non-empty); the LA_URL is the source manifest PRO's, carried forward verbatim.
        Ok(playready::build_playready_v43(kid, la_url.unwrap_or(""))?)
    }
}

/// The `media_inspect` functions the `manifest_hls` gates call, over a media engine: its
/// failures keep their Python class ([`MediaError`] → [`PyErr`]), so the gates catch the
/// `RuntimeError`s Python's catch and nothing else.
pub struct GateEngine<'a>(pub &'a dyn MediaEngine);

impl GateMedia for GateEngine<'_> {
    fn compute_framemd5(&self, path: &Path) -> Result<String, PyErr> {
        Ok(self.0.compute_framemd5(path)?)
    }

    fn decode_hls_media_playlist(
        &self,
        localized_playlist_path: &Path,
        output_path: &Path,
        mode: &str,
    ) -> Result<PathBuf, PyErr> {
        Ok(self
            .0
            .decode_hls_media_playlist(localized_playlist_path, output_path, mode)?)
    }

    fn decode_media_to_pcm(&self, input_path: &Path, output_path: &Path) -> Result<PathBuf, PyErr> {
        Ok(self.0.decode_media_to_pcm(input_path, output_path)?)
    }

    fn localize_hls_media_playlist(
        &self,
        playlist_text: &str,
        key_file_path: &Path,
        resolve_uri: &dyn Fn(&str) -> Result<PathBuf, PyErr>,
    ) -> Result<String, PyErr> {
        media::localize_hls_media_playlist(playlist_text, key_file_path, resolve_uri)
    }

    fn measure_segment_durations_s(
        &self,
        init_path: &Path,
        segment_paths: &[PathBuf],
        timescale: i64,
    ) -> Result<Vec<f64>, PyErr> {
        Ok(self
            .0
            .measure_segment_durations_s(init_path, segment_paths, timescale)?)
    }

    fn write_raw_key_file(&self, key_hex: &str, dest: &Path) -> Result<PathBuf, PyErr> {
        Ok(media::write_raw_key_file(key_hex, dest)?)
    }
}
