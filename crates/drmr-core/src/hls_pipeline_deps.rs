//! Dependency interface for source verification, media operations, and HLS packaging.
//!
//! Production dependencies delegate to the core modules; tests inject stand-ins.
//! `PlayReady` signaling uses one shared generator so manifest and initialization-segment
//! representations contain the same bytes.

use crate::config::KeyMaterial;
use crate::manifest_gen::{self, PackagedPresentation, PrivateFairPlaySignaling};
use crate::manifest_support::py::PyErr;
use crate::media::{self, InitProbe, MediaEngine, MediaError, PacketInfo};
use crate::mp4::audio_edit::AudioEdit;
use crate::packager::{self, PackagerResult};
use crate::pipeline_deps::EncryptCbcs;
use crate::resources::{Budget, Hardware, Workload};
use crate::source::{self, Failure, SourceAnalysis};
use crate::toolrun::ToolRunner;
use num_rational::BigRational;
use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The functions `hls_pipeline` takes from the other modules, with explicit parameters and the
/// core modules' types and errors.
pub trait PipelineDeps: Sync {
    /// Recommend workers within the validated caller maximum. Injected media backends
    /// may keep the supplied maximum; the subprocess backend uses hardware and content.
    fn resource_workers(
        &self,
        _analysis: &SourceAnalysis,
        maximum: usize,
    ) -> Result<usize, MediaError> {
        Ok(maximum)
    }

    /// `media::concatenate(paths, output)`: the files byte for byte into `output`.
    fn concatenate(&self, parts: &[PathBuf], output: &Path) -> Result<PathBuf, MediaError>;

    /// `media::probe_packets(path, stream_index)`.
    fn probe_packets(&self, path: &Path, stream_index: i64) -> Result<PacketInfo, MediaError>;

    /// `media::packet_payloads(path, stream_index)` (never logged).
    fn packet_payloads(&self, path: &Path, stream_index: i64) -> Result<Vec<Vec<u8>>, MediaError>;

    /// `_copy_imsc`'s `payloads(location, stream)`: the `data_hash` of every packet of the
    /// stream (`ffprobe -show_data_hash sha256`), as printed.
    fn packet_payload_hashes(
        &self,
        path: &Path,
        stream_index: i64,
    ) -> Result<Vec<Value>, MediaError>;

    /// `media::normalize_segment(path, destination, stream_index, offset=…,
    /// content_type=…)`.
    fn normalize_segment(
        &self,
        path: &Path,
        destination: &Path,
        stream_index: i64,
        offset: &BigRational,
        content_type: &str,
    ) -> Result<PathBuf, MediaError>;

    /// `media::same_timing(source, output, shift)` (the default tolerance).
    fn same_timing(
        &self,
        source: &PacketInfo,
        output: &PacketInfo,
        shift: &BigRational,
    ) -> Result<bool, MediaError>;

    /// `media::decoded_digest(path, content_type, stream_index)`.
    fn decoded_digest(
        &self,
        path: &Path,
        content_type: &str,
        stream_index: i64,
    ) -> Result<String, MediaError>;

    /// `media::codec_from_init(path, fallback)`.
    fn codec_from_init(&self, path: &Path, fallback: &str) -> Result<String, MediaError>;

    /// `media::split_fragmented_mp4(source, init)`: the media fragments; the init written.
    fn split_fragmented_mp4(&self, source: &Path, init: &Path) -> Result<Vec<Vec<u8>>, MediaError>;

    /// `_validate_init`'s `json.loads(run_media(["mp4dump", "--format", "json", path]))`.
    fn box_tree(&self, path: &Path) -> Result<Value, MediaError>;

    /// `media::probe_init_segment(path)`.
    fn probe_init_segment(&self, path: &Path) -> Result<InitProbe, MediaError>;

    /// `media::pssh_box_present(path, pssh_box)`.
    fn pssh_box_present(&self, path: &Path, pssh_box: &[u8]) -> Result<bool, MediaError>;

    /// `packager.encrypt_cbcs(...)` (the boxes of `pssh_hex` concatenated into one `--pssh`).
    fn encrypt_cbcs(&self, call: &EncryptCbcs<'_>) -> Result<PackagerResult, packager::Error>;

    /// `packager.decrypt_cenc(input_mp4, output_mp4, key, packager_bin=…)`.
    fn decrypt_cenc(
        &self,
        input_mp4: &Path,
        output_mp4: &Path,
        key: &KeyMaterial,
        packager_bin: &str,
    ) -> Result<PackagerResult, packager::Error>;

    /// `source::verify_source_snapshot(analysis)`.
    fn verify_source_snapshot(&self, analysis: &SourceAnalysis) -> Result<(), Failure>;

    /// `manifest_gen.write_dash(presentation, path)`.
    fn write_dash(
        &self,
        presentation: &PackagedPresentation,
        path: &Path,
    ) -> Result<PathBuf, PyErr>;

    /// `manifest_gen.write_hls(presentation, path, fairplay=…)`: the master, then every media
    /// playlist.
    fn write_hls(
        &self,
        presentation: &PackagedPresentation,
        path: &Path,
        fairplay: &PrivateFairPlaySignaling,
    ) -> Result<Vec<PathBuf>, PyErr>;
}

/// The real modules behind [`PipelineDeps`]: the subprocess media engine over the job's tool
/// runner (the default `ffmpeg` / `ffprobe` / `mp4dump`, as `hls_media` writes them), Shaka
/// Packager through the same runner (the call's `packager_bin`, no timeout: Python's
/// `_SUBPROCESS_TIMEOUT_S`), the snapshot verifier and the manifest generator.
pub struct Ports {
    engine: media::Subprocess,
}

impl fmt::Debug for Ports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ports")
            .field("engine", &self.engine)
            .finish()
    }
}

impl Ports {
    /// Every tool through `runner` (its sink, its secrets).
    pub fn new(runner: Arc<ToolRunner>) -> Self {
        Ports {
            engine: media::Subprocess::new(runner),
        }
    }

    fn packager(&self, packager_bin: &str) -> packager::Packager {
        packager::Packager::new(Arc::clone(self.engine.runner())).with_packager_bin(packager_bin)
    }
}

impl PipelineDeps for Ports {
    fn resource_workers(
        &self,
        analysis: &SourceAnalysis,
        maximum: usize,
    ) -> Result<usize, MediaError> {
        let workload = Workload::from_analysis(analysis)?;
        self.engine.runner().register_workload(workload);
        Budget::choose(Hardware::detect(), workload, Some(maximum))
            .map(|budget| {
                self.engine.runner().note_resource_budget(&budget);
                budget.workers
            })
            .map_err(|error| media::HlsMediaError::new(&error.to_string()).into())
    }

    fn concatenate(&self, parts: &[PathBuf], output: &Path) -> Result<PathBuf, MediaError> {
        media::concatenate(parts, output)
    }

    fn probe_packets(&self, path: &Path, stream_index: i64) -> Result<PacketInfo, MediaError> {
        self.engine.probe_packets(path, stream_index)
    }

    fn packet_payloads(&self, path: &Path, stream_index: i64) -> Result<Vec<Vec<u8>>, MediaError> {
        self.engine.packet_payloads(path, stream_index)
    }

    fn packet_payload_hashes(
        &self,
        path: &Path,
        stream_index: i64,
    ) -> Result<Vec<Value>, MediaError> {
        self.engine.packet_payload_hashes(path, stream_index)
    }

    fn normalize_segment(
        &self,
        path: &Path,
        destination: &Path,
        stream_index: i64,
        offset: &BigRational,
        content_type: &str,
    ) -> Result<PathBuf, MediaError> {
        let edit = if content_type == "audio" {
            AudioEdit::read(path, stream_index)?
        } else {
            None
        };
        let normalized =
            self.engine
                .normalize_segment(path, destination, stream_index, offset, content_type)?;
        if let Some(edit) = edit {
            edit.restore(&normalized, &[])?;
        }
        Ok(normalized)
    }

    fn same_timing(
        &self,
        source: &PacketInfo,
        output: &PacketInfo,
        shift: &BigRational,
    ) -> Result<bool, MediaError> {
        media::same_timing(source, output, shift, None)
    }

    fn decoded_digest(
        &self,
        path: &Path,
        content_type: &str,
        stream_index: i64,
    ) -> Result<String, MediaError> {
        self.engine.decoded_digest(path, content_type, stream_index)
    }

    fn codec_from_init(&self, path: &Path, fallback: &str) -> Result<String, MediaError> {
        media::codec_from_init(path, fallback)
    }

    fn split_fragmented_mp4(&self, source: &Path, init: &Path) -> Result<Vec<Vec<u8>>, MediaError> {
        media::split_fragmented_mp4(source, init)
    }

    fn box_tree(&self, path: &Path) -> Result<Value, MediaError> {
        self.engine.box_tree(path)
    }

    fn probe_init_segment(&self, path: &Path) -> Result<InitProbe, MediaError> {
        self.engine.probe_init_segment(path)
    }

    fn pssh_box_present(&self, path: &Path, pssh_box: &[u8]) -> Result<bool, MediaError> {
        media::pssh_box_present(path, pssh_box)
    }

    fn encrypt_cbcs(&self, call: &EncryptCbcs<'_>) -> Result<PackagerResult, packager::Error> {
        let pssh: Vec<&str> = call
            .pssh_hex
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .collect();
        let edit = AudioEdit::read(call.input_mp4, 0).map_err(edit_error)?;
        let result = self.packager(call.packager_bin).encrypt_cbcs(
            call.input_mp4,
            call.init_output,
            call.segment_template,
            call.key,
            call.segment_duration_seconds,
            &pssh,
        )?;
        if result.returncode == 0
            && let Some(edit) = edit
        {
            let template = Path::new(call.segment_template);
            let filename = template
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    packager::Error::Value("unsupported-audio-segment-template".into())
                })?;
            let (prefix, suffix) = filename.split_once("$Number$").ok_or_else(|| {
                packager::Error::Value("unsupported-audio-segment-template".into())
            })?;
            let mut segments = Vec::new();
            for entry in std::fs::read_dir(template.parent().unwrap_or(Path::new(".")))? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_prefix(prefix))
                    .and_then(|name| name.strip_suffix(suffix))
                    .is_some_and(|number| {
                        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                    })
                {
                    segments.push(entry.path());
                }
            }
            edit.restore(call.init_output, &segments)
                .map_err(edit_error)?;
        }
        Ok(result)
    }

    fn decrypt_cenc(
        &self,
        input_mp4: &Path,
        output_mp4: &Path,
        key: &KeyMaterial,
        packager_bin: &str,
    ) -> Result<PackagerResult, packager::Error> {
        let edit = AudioEdit::read(input_mp4, 0).map_err(edit_error)?;
        let result = self
            .packager(packager_bin)
            .decrypt_cenc(input_mp4, output_mp4, key)?;
        if result.returncode == 0
            && let Some(edit) = edit
        {
            edit.restore(output_mp4, &[]).map_err(edit_error)?;
        }
        Ok(result)
    }

    fn verify_source_snapshot(&self, analysis: &SourceAnalysis) -> Result<(), Failure> {
        source::verify_source_snapshot(analysis)
    }

    fn write_dash(
        &self,
        presentation: &PackagedPresentation,
        path: &Path,
    ) -> Result<PathBuf, PyErr> {
        manifest_gen::write_dash(presentation, path)
    }

    fn write_hls(
        &self,
        presentation: &PackagedPresentation,
        path: &Path,
        fairplay: &PrivateFairPlaySignaling,
    ) -> Result<Vec<PathBuf>, PyErr> {
        manifest_gen::write_hls(presentation, path, fairplay)
    }
}

fn edit_error(error: MediaError) -> packager::Error {
    match error {
        MediaError::Os(error) => packager::Error::Io(error),
        error => packager::Error::Value(error.to_string()),
    }
}
