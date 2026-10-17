//! Synthetic catalog assets and dependency stand-ins for pipeline tests.
#![allow(
    dead_code,
    reason = "shared by the pipeline and the cli test binaries; each uses a subset"
)]

use drmr_core::checks::CheckId;
use drmr_core::config::{Asset, KeyMaterial, Rendition};
use drmr_core::events::{Collector, NoSink};
use drmr_core::manifest_dash::DashRewriteResult;
use drmr_core::manifest_hls::{HlsRewriteResult, HlsVariantMapping};
use drmr_core::media::{FragmentBoundary, HexSecret};
use drmr_core::packager::PackagerResult;
use drmr_core::pipeline::PipelineDeps;
use drmr_core::pipeline_deps::{
    Config, EncryptCbcs, Manifests, MediaInspect, Packager, PlayReady, PyError,
    TranscryptCbcsPerSegment,
};
use drmr_core::playready::{ParsedPro, PlayReadyObjects};
use drmr_core::toolrun::{RunOptions, Secrets, ToolRunner};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// The synthetic KID (public signaling).
pub const KID: &str = "000102030405060708090a0b0c0d0e0f";
/// The synthetic content key.
pub const KEY: &str = "10101010101010101010101010101010";
/// The synthetic constant IV.
pub const IV: &str = "20202020202020202020202020202020";
/// The synthetic license URL.
pub const LA_URL: &str = "https://license.example.invalid/rightsmanager.asmx";
/// `Synthetic Widevine initialization bytes`.
pub const FAKE_WIDEVINE_PSSH: &[u8] = b"FAKE-PSSH-NOT-A-REAL-BOX-JUST-TEST-BYTES";

pub fn key() -> KeyMaterial {
    KeyMaterial::new(KID, KEY, IV).unwrap()
}

/// `Synthetic DASH representation settings`.
#[derive(Clone, Debug)]
pub struct RepSpec {
    pub id: String,
    pub codec: String,
    pub content_type: String,
    pub timescale: i64,
    pub duration_ticks: i64,
    pub start_number: i64,
    pub segment_count: usize,
}

impl RepSpec {
    pub fn segments(mut self, count: usize) -> Self {
        self.segment_count = count;
        self
    }

    pub fn ticks(mut self, ticks: i64) -> Self {
        self.duration_ticks = ticks;
        self
    }
}

pub fn h264_rep(id: &str) -> RepSpec {
    RepSpec {
        id: id.into(),
        codec: "avc1.42C00C".into(),
        content_type: "video".into(),
        timescale: 24000,
        duration_ticks: 144_000,
        start_number: 0,
        segment_count: 3,
    }
}

pub fn audio_rep(id: &str) -> RepSpec {
    RepSpec {
        id: id.into(),
        codec: "mp4a.40.2".into(),
        content_type: "audio".into(),
        timescale: 48000,
        duration_ticks: 288_000,
        start_number: 0,
        segment_count: 3,
    }
}

/// What `build_synthetic_dash_asset` wrote, with the renditions `config.enumerate_renditions`
/// yields for it.
#[derive(Clone, Debug)]
pub struct SyntheticAsset {
    pub mpd_path: PathBuf,
    pub title_id: String,
    pub download_root: PathBuf,
    pub renditions: Vec<Rendition>,
}

impl SyntheticAsset {
    /// `config.load_asset(mpd_path, KEY, output_root)`.
    pub fn asset(&self, output_root: &Path) -> Asset {
        self.asset_as(output_root, &self.title_id)
    }

    /// `config.load_asset(..., asset_id=asset_id)`.
    pub fn asset_as(&self, output_root: &Path, asset_id: &str) -> Asset {
        Asset {
            asset_id: asset_id.into(),
            mpd_path: self.mpd_path.clone(),
            input_base_dir: fs::canonicalize(&self.download_root).unwrap(),
            output_dir: output_root.join(asset_id),
            key: key(),
            renditions: self.renditions.clone(),
        }
    }
}

/// `Synthetic DASH asset layout`: `download/<manifest host>/<title>/dash/
/// manifest.mpd` and, per rep, `init.mp4` and `seg_<i>.m4s` under
/// `download/<cdn host>/<title>/<content type>/<id>/`.
pub fn build_synthetic_dash_asset(root: &Path, reps: &[RepSpec], title_id: &str) -> SyntheticAsset {
    let download_root = root.join("download");
    let mpd_path = download_root
        .join("manifest.example.test")
        .join(title_id)
        .join("dash/manifest.mpd");
    fs::create_dir_all(mpd_path.parent().unwrap()).unwrap();
    let body = reps
        .iter()
        .map(|rep| {
            format!(
                "<AdaptationSet mimeType=\"{}/mp4\" contentType=\"{}\"><Representation id=\"{}\" \
                 codecs=\"{}\" bandwidth=\"1\"><SegmentTemplate timescale=\"{}\" startNumber=\"{}\" \
                 duration=\"{}\" media=\"{}/{}/seg_$Number$.m4s\" initialization=\"{}/{}/init.mp4\"/>\
                 </Representation></AdaptationSet>",
                rep.content_type,
                rep.content_type,
                rep.id,
                rep.codec,
                rep.timescale,
                rep.start_number,
                rep.duration_ticks,
                rep.content_type,
                rep.id,
                rep.content_type,
                rep.id
            )
        })
        .collect::<Vec<_>>()
        .concat();
    fs::write(
        &mpd_path,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" \
             type=\"static\"><BaseURL>https://cdn.example.test/{title_id}/</BaseURL><Period \
             id=\"p0\">{body}</Period></MPD>\n"
        ),
    )
    .unwrap();
    let mut renditions = Vec::new();
    for rep in reps {
        let rep_dir = download_root
            .join("cdn.example.test")
            .join(title_id)
            .join(&rep.content_type)
            .join(&rep.id);
        fs::create_dir_all(&rep_dir).unwrap();
        fs::write(rep_dir.join("init.mp4"), format!("INIT:{}", rep.id)).unwrap();
        for index in 0..rep.segment_count {
            fs::write(
                rep_dir.join(format!("seg_{index}.m4s")),
                format!("SEG:{}:{index}", rep.id),
            )
            .unwrap();
        }
        let rep_dir = fs::canonicalize(&rep_dir).unwrap();
        renditions.push(Rendition {
            rep_id: rep.id.clone(),
            content_type: rep.content_type.clone(),
            codec: rep.codec.clone(),
            timescale: rep.timescale,
            segment_duration_ticks: rep.duration_ticks,
            start_number: rep.start_number,
            init_path: rep_dir.join("init.mp4"),
            segment_paths: (0..rep.segment_count)
                .map(|index| rep_dir.join(format!("seg_{index}.m4s")))
                .collect(),
            rel_dir: PathBuf::from(format!("{}/{}", rep.content_type, rep.id)),
            widevine_pssh_hex: Some(hex::encode(FAKE_WIDEVINE_PSSH)),
            width: None,
            height: None,
            frame_rate: None,
            lang: None,
        });
    }
    SyntheticAsset {
        mpd_path,
        title_id: title_id.into(),
        download_root,
        renditions,
    }
}

/// One synthetic MPEG transport-stream H.264 variant without audio.
pub fn write_minimal_hls_master(root: &Path, title_id: &str, segment_count: usize) -> PathBuf {
    let download = root.join("download");
    let segments = download
        .join("vod.example.test")
        .join(title_id)
        .join("video/hv0");
    fs::create_dir_all(&segments).unwrap();
    let mut playlist = vec![
        "#EXTM3U".to_string(),
        "#EXT-X-VERSION:7".into(),
        "#EXT-X-PLAYLIST-TYPE:VOD".into(),
        "#EXT-X-TARGETDURATION:6".into(),
        "#EXT-X-MEDIA-SEQUENCE:0".into(),
        format!(
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",IV=0x{},KEYFORMAT=\"com.apple.streamingkeydelivery\"",
            IV.to_uppercase()
        ),
    ];
    for index in 0..segment_count {
        fs::write(segments.join(format!("seg_{index}.ts")), [0u8; 100]).unwrap();
        playlist.push("#EXTINF:6,".into());
        playlist.push(format!(
            "https://vod.example.test/{title_id}/video/hv0/seg_{index}.ts"
        ));
    }
    playlist.push("#EXT-X-ENDLIST".into());
    let variants = download
        .join("variants.example.test")
        .join(title_id)
        .join("video/hv0");
    fs::create_dir_all(&variants).unwrap();
    fs::write(
        variants.join("video_100_x.m3u8"),
        playlist.join("\n") + "\n",
    )
    .unwrap();
    let master_dir = download
        .join("manifest.example.test")
        .join(title_id)
        .join("hls");
    fs::create_dir_all(&master_dir).unwrap();
    let master = master_dir.join("manifest.m3u8");
    fs::write(
        &master,
        format!(
            "#EXTM3U\n#EXT-X-VERSION:4\n#EXT-X-STREAM-INF:BANDWIDTH=100000,CODECS=\"avc1.42C00C,\
             mp4a.40.2\",RESOLUTION=320x180,FRAME-RATE=24.000\n\
             https://variants.example.test/{title_id}/video/hv0/video_100_x.m3u8\n"
        ),
    )
    .unwrap();
    master
}

/// Every file under `root` with its bytes and modification time.
pub fn tree_state(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut state = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                state.insert(path.clone(), (fs::read(&path).unwrap(), modified));
            }
        }
    }
    state
}

/// A native packager-status fixture on the runner's private `PATH`.
pub fn write_stub_packager(
    dir: &Path,
    exit_code: i32,
    stderr: Option<&str>,
    hang: Option<u32>,
) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let spec = drmr_test_support::ToolSpec {
        exit_code,
        stderr: stderr.map_or_else(Vec::new, |value| format!("{value}\n").into_bytes()),
        sleep_ms: u64::from(hang.unwrap_or(0)) * 1000,
        ..Default::default()
    };
    let status = dir.join("packager.status");
    spec.encode(fs::File::create(&status).unwrap()).unwrap();
    let executable = drmr_test_support::program_path(&dir.join("packager"));
    if executable.is_file() {
        return executable;
    }
    let immutable =
        drmr_test_support::ToolSpec::default().with("status_file", status.to_str().unwrap());
    drmr_test_support::install(&dir.join("packager"), &immutable).unwrap()
}

/// One packager call as the fake saw it.
#[derive(Clone, Debug, PartialEq)]
pub enum PackagerCall {
    Merge {
        output: PathBuf,
    },
    Decrypt {
        input: PathBuf,
        output: PathBuf,
        packager_bin: String,
    },
    Encrypt {
        template: String,
        segment_duration_seconds: f64,
        pssh_hex: Option<Vec<String>>,
        packager_bin: String,
    },
    Transcrypt {
        template: String,
        segments: usize,
        pssh_hex: Option<Vec<String>>,
        work_dir: Option<PathBuf>,
    },
}

/// `packager` driving `stub_packager`: every Shaka run goes through the job's tool runner
/// (redacted argv, the call's key and IV as secrets) and fabricates the outputs the stub does.
pub struct FakePackager {
    /// What the bare `packager` resolves to; `None`: no packager on `PATH`.
    pub on_path: Option<PathBuf>,
    /// `STUB_PACKAGER_SEGMENT_COUNT`.
    pub segment_count: Option<usize>,
    /// `external packager timeout`.
    pub timeout: Option<Duration>,
    pub calls: Mutex<Vec<PackagerCall>>,
    /// What each run's program resolved to (`None`: nothing named `packager` was found).
    pub resolved: Mutex<Vec<Option<PathBuf>>>,
}

impl FakePackager {
    pub fn on_path(stub: PathBuf) -> FakePackager {
        FakePackager {
            on_path: Some(stub),
            segment_count: None,
            timeout: None,
            calls: Mutex::new(Vec::new()),
            resolved: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<PackagerCall> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: PackagerCall) {
        self.calls.lock().unwrap().push(call);
    }

    fn keys(key: &KeyMaterial, redact: bool) -> String {
        let shown = if redact {
            "***REDACTED***"
        } else {
            key.key_hex()
        };
        format!("label=:key_id={}:key={shown}", key.key_id_hex())
    }

    /// What the bare `packager` resolves to: the first `packager` on the runner's own `PATH`
    /// (`--packager-bin` prepended a directory to it), else the one on the process `PATH`.
    fn resolve(&self, runner: &ToolRunner) -> Option<PathBuf> {
        let on_runner_path = runner.path().and_then(|path| {
            std::env::split_paths(path)
                .map(|dir| drmr_test_support::program_path(&dir.join("packager")))
                .find(|candidate| candidate.is_file())
        });
        on_runner_path.or_else(|| self.on_path.clone())
    }

    fn run(
        &self,
        runner: &ToolRunner,
        packager_bin: &str,
        argv: Vec<String>,
        redacted: Vec<String>,
        key: &KeyMaterial,
    ) -> Result<PackagerResult, PyError> {
        let program = if packager_bin == "packager" {
            self.resolve(runner)
        } else {
            Some(PathBuf::from(packager_bin))
        };
        self.resolved.lock().unwrap().push(program.clone());
        // Native fixtures receive the original arguments directly, including spaces and secrets.
        let head = match program {
            Some(path) => vec![path.display().to_string()],
            None => vec!["/nonexistent-drmr-test/packager".to_string()],
        };
        let real: Vec<String> = head.into_iter().chain(argv).collect();
        let shown: Vec<String> = std::iter::once(packager_bin.to_string())
            .chain(redacted)
            .collect();
        let secrets = [key.key_hex(), key.iv_hex()];
        let options = RunOptions {
            argv_redacted: Some(&shown),
            secrets: &secrets,
            timeout: self.timeout,
            ..RunOptions::default()
        };
        match runner.run_tool(&real, "packager", &options) {
            Ok(completed) => {
                let mut tail = completed.stderr_text();
                for secret in secrets {
                    for variant in [
                        secret.to_string(),
                        secret.to_lowercase(),
                        secret.to_uppercase(),
                    ] {
                        tail = tail.replace(&variant, "***REDACTED***");
                    }
                }
                Ok(PackagerResult {
                    argv_redacted: shown,
                    returncode: completed.returncode.unwrap_or(-1),
                    duration_s: 0.0,
                    stderr_tail: tail,
                })
            }
            Err(error) if error.kind == "timeout" => Err(PyError::Packager(format!(
                "Shaka Packager timed out after {}s",
                self.timeout.map_or(0.0, |limit| limit.as_secs_f64())
            ))),
            Err(_) => Err(PyError::Packager("could not run Shaka Packager".into())),
        }
    }

    fn encrypt_count(&self, segment_duration_seconds: f64) -> usize {
        self.segment_count
            .unwrap_or(if segment_duration_seconds >= 10_000.0 {
                1
            } else {
                3
            })
    }
}

fn numeric_segment_key(path: &Path) -> Result<u64, PyError> {
    let stem = path.file_stem().unwrap().to_string_lossy().to_string();
    let digits: String = stem
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().map_err(|_| {
        PyError::Value(format!(
            "segment file {} has no numeric index",
            path.display()
        ))
    })
}

fn create_empty(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"").unwrap();
}

impl Packager for FakePackager {
    fn merge_segments(
        &self,
        init_path: &Path,
        segment_paths: &[PathBuf],
        output_path: &Path,
    ) -> Result<PathBuf, PyError> {
        self.record(PackagerCall::Merge {
            output: output_path.to_path_buf(),
        });
        let mut ordered = Vec::new();
        for segment in segment_paths {
            ordered.push((numeric_segment_key(segment)?, segment.clone()));
        }
        ordered.sort_by_key(|(number, _)| *number);
        fs::create_dir_all(output_path.parent().unwrap()).unwrap();
        let mut out = fs::File::create(output_path).map_err(|e| PyError::os(&e, output_path))?;
        out.write_all(&fs::read(init_path).map_err(|e| PyError::os(&e, init_path))?)
            .unwrap();
        for (_, segment) in ordered {
            out.write_all(&fs::read(&segment).map_err(|e| PyError::os(&e, &segment))?)
                .unwrap();
        }
        Ok(output_path.to_path_buf())
    }

    fn renumber_to_source_origin(
        &self,
        staging_dir: &Path,
        source_start_number: i64,
    ) -> Result<Vec<(i64, i64)>, PyError> {
        let mut staged: Vec<(i64, PathBuf)> = Vec::new();
        for entry in fs::read_dir(staging_dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let index = name
                .strip_prefix("seg_")
                .and_then(|rest| rest.strip_suffix(".m4s"));
            if let Some(index) = index.filter(|_| path.is_file()) {
                let number = index.parse().map_err(|_| {
                    PyError::Value(format!("invalid literal for int() with base 10: {index:?}"))
                })?;
                staged.push((number, path));
            }
        }
        staged.sort();
        let numbers: Vec<i64> = staged.iter().map(|(number, _)| *number).collect();
        if numbers != (1..=i64::try_from(staged.len()).unwrap()).collect::<Vec<_>>() {
            return Err(PyError::Value(format!(
                "staged segments are not the contiguous 1..N Shaka emits: {numbers:?}"
            )));
        }
        let shift = source_start_number - 1;
        let mut plan: Vec<(PathBuf, PathBuf)> = staged
            .iter()
            .map(|(number, path)| {
                (
                    path.clone(),
                    path.with_file_name(format!("seg_{}.m4s", number + shift)),
                )
            })
            .collect();
        if shift > 0 {
            plan.reverse();
        }
        for (from, to) in plan {
            if from != to {
                fs::rename(&from, &to).unwrap();
            }
        }
        Ok(staged
            .iter()
            .map(|(number, _)| (*number, number + shift))
            .collect())
    }

    fn decrypt_cenc(
        &self,
        runner: &Arc<ToolRunner>,
        input_mp4: &Path,
        output_mp4: &Path,
        key: &KeyMaterial,
        packager_bin: &str,
    ) -> Result<PackagerResult, PyError> {
        self.record(PackagerCall::Decrypt {
            input: input_mp4.to_path_buf(),
            output: output_mp4.to_path_buf(),
            packager_bin: packager_bin.into(),
        });
        fs::create_dir_all(output_mp4.parent().unwrap()).unwrap();
        let head = vec![
            format!(
                "input={},stream=0,output={}",
                input_mp4.display(),
                output_mp4.display()
            ),
            "--enable_raw_key_decryption".to_string(),
            "--keys".to_string(),
        ];
        let argv = [head.clone(), vec![Self::keys(key, false)]].concat();
        let redacted = [head, vec![Self::keys(key, true)]].concat();
        let result = self.run(runner, packager_bin, argv, redacted, key)?;
        create_empty(output_mp4);
        Ok(result)
    }

    fn encrypt_cbcs(
        &self,
        runner: &Arc<ToolRunner>,
        call: &EncryptCbcs<'_>,
    ) -> Result<PackagerResult, PyError> {
        self.record(PackagerCall::Encrypt {
            template: call.segment_template.into(),
            segment_duration_seconds: call.segment_duration_seconds,
            pssh_hex: call.pssh_hex.map(<[String]>::to_vec),
            packager_bin: call.packager_bin.into(),
        });
        let descriptor = format!(
            "input={},stream=0,init_segment={},segment_template={}",
            call.input_mp4.display(),
            call.init_output.display(),
            call.segment_template
        );
        let mut tail = vec![
            "--segment_duration".to_string(),
            format!("{}", call.segment_duration_seconds),
            "--clear_lead".into(),
            "0".into(),
        ];
        if let Some(boxes) = call.pssh_hex.filter(|boxes| !boxes.is_empty()) {
            tail.extend(["--pssh".to_string(), boxes.concat()]);
        }
        let head = vec![
            descriptor,
            "--enable_raw_key_encryption".into(),
            "--protection_scheme".into(),
            "cbcs".into(),
            "--iv".into(),
        ];
        let argv = [
            head.clone(),
            vec![call.key.iv_hex().to_string()],
            tail.clone(),
            vec!["--keys".into(), Self::keys(call.key, false)],
        ]
        .concat();
        let redacted = [
            head,
            vec!["***REDACTED***".into()],
            tail,
            vec!["--keys".into(), Self::keys(call.key, true)],
        ]
        .concat();
        let result = self.run(runner, call.packager_bin, argv, redacted, call.key)?;
        create_empty(call.init_output);
        for number in 1..=self.encrypt_count(call.segment_duration_seconds) {
            create_empty(Path::new(
                &call
                    .segment_template
                    .replace("$Number$", &number.to_string()),
            ));
        }
        Ok(result)
    }

    fn transcrypt_cbcs_per_segment(
        &self,
        runner: &Arc<ToolRunner>,
        call: &TranscryptCbcsPerSegment<'_>,
    ) -> Result<PackagerResult, PyError> {
        self.record(PackagerCall::Transcrypt {
            template: call.segment_template.into(),
            segments: call.segment_paths.len(),
            pssh_hex: call.pssh_hex.map(<[String]>::to_vec),
            work_dir: call.work_dir.map(Path::to_path_buf),
        });
        let work = call.work_dir.map_or_else(
            || call.init_output.parent().unwrap().join("_persegment_work"),
            Path::to_path_buf,
        );
        fs::create_dir_all(&work).unwrap();
        for (index, source) in call.segment_paths.iter().enumerate() {
            let seg_work = work.join(format!("seg_{index}"));
            let _ = fs::remove_dir_all(&seg_work);
            fs::create_dir_all(&seg_work).unwrap();
            let merged = self.merge_segments(
                call.init_path,
                std::slice::from_ref(source),
                &seg_work.join("source.mp4"),
            )?;
            let clear = seg_work.join("clear.mp4");
            let decrypted =
                self.decrypt_cenc(runner, &merged, &clear, call.key, call.packager_bin)?;
            if decrypted.returncode != 0 {
                return Ok(decrypted);
            }
            let per_segment_init = seg_work.join("init.mp4");
            let template = seg_work.join("out_$Number$.m4s").display().to_string();
            let encrypted = self.encrypt_cbcs(
                runner,
                &EncryptCbcs {
                    input_mp4: &clear,
                    init_output: &per_segment_init,
                    segment_template: &template,
                    key: call.key,
                    segment_duration_seconds: 1_000_000.0,
                    pssh_hex: call.pssh_hex,
                    packager_bin: call.packager_bin,
                },
            )?;
            if encrypted.returncode != 0 {
                return Ok(encrypted);
            }
            let produced: Vec<PathBuf> = fs::read_dir(&seg_work)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("out_")
                })
                .collect();
            if produced.len() != 1 {
                return Ok(PackagerResult {
                    argv_redacted: encrypted.argv_redacted,
                    returncode: 251,
                    duration_s: 0.0,
                    stderr_tail: format!(
                        "per-segment isolation produced {} segments for source segment index {index} (expected exactly 1)",
                        produced.len()
                    ),
                });
            }
            let staged = PathBuf::from(
                call.segment_template
                    .replace("$Number$", &(index + 1).to_string()),
            );
            fs::create_dir_all(staged.parent().unwrap()).unwrap();
            fs::rename(&produced[0], &staged).unwrap();
            if index == 0 {
                fs::copy(&per_segment_init, call.init_output).unwrap();
            }
            fs::remove_dir_all(&seg_work).unwrap();
        }
        Ok(PackagerResult {
            argv_redacted: Vec::new(),
            returncode: 0,
            duration_s: 0.0,
            stderr_tail: String::new(),
        })
    }
}

/// A injected `media_inspect` reader.
pub type Reader<T> = Box<dyn Fn(&ToolRunner, &Path) -> Result<T, PyError> + Send + Sync>;

/// `media_inspect` as the tests see it against the stub's empty media: no `tenc`, no
/// fragment; framemd5 a digest of the file; `pssh_box_present` the real byte containment.
pub struct FakeMedia {
    pub framemd5: Reader<String>,
    pub tenc_iv: Reader<Option<String>>,
    pub boundary: Reader<Option<FragmentBoundary>>,
    pub calls: Mutex<Vec<(String, PathBuf)>>,
}

impl Default for FakeMedia {
    fn default() -> Self {
        FakeMedia {
            framemd5: Box::new(|_, path| {
                drmr_core::snapshot::sha256_file(path)
                    .map(|digest| format!("framemd5:{digest}"))
                    .map_err(|error| {
                        PyError::Runtime(format!(
                            "ffmpeg framemd5 failed for {}: {error}",
                            path.display()
                        ))
                    })
            }),
            tenc_iv: Box::new(|_, _| Ok(None)),
            boundary: Box::new(|_, _| Ok(None)),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl FakeMedia {
    pub fn calls(&self, function: &str) -> Vec<PathBuf> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == function)
            .map(|(_, path)| path.clone())
            .collect()
    }

    fn record(&self, function: &str, path: &Path) {
        self.calls
            .lock()
            .unwrap()
            .push((function.into(), path.to_path_buf()));
    }
}

impl MediaInspect for FakeMedia {
    fn compute_framemd5(&self, runner: &Arc<ToolRunner>, path: &Path) -> Result<String, PyError> {
        self.record("compute_framemd5", path);
        (self.framemd5)(runner, path)
    }

    fn read_tenc_constant_iv_hex(
        &self,
        runner: &Arc<ToolRunner>,
        init_segment_path: &Path,
    ) -> Result<Option<HexSecret>, PyError> {
        self.record("read_tenc_constant_iv_hex", init_segment_path);
        Ok((self.tenc_iv)(runner, init_segment_path)?.map(HexSecret::new))
    }

    fn read_fragment_boundary(
        &self,
        runner: &Arc<ToolRunner>,
        segment_path: &Path,
    ) -> Result<Option<FragmentBoundary>, PyError> {
        self.record("read_fragment_boundary", segment_path);
        (self.boundary)(runner, segment_path)
    }

    fn pssh_box_present(&self, init_segment_path: &Path, pssh_box: &[u8]) -> Result<bool, PyError> {
        self.record("pssh_box_present", init_segment_path);
        let data = fs::read(init_segment_path).map_err(|e| PyError::os(&e, init_segment_path))?;
        Ok(pssh_box.is_empty()
            || data
                .windows(pssh_box.len())
                .any(|window| window == pssh_box))
    }
}

/// `media::read_tenc_constant_iv_hex` over a real (stub) `mp4dump`: the run through the
/// tool runner, the JSON tree, the `tenc` box's bracketed constant IV.
pub fn mp4dump_tenc_iv(
    runner: &ToolRunner,
    mp4dump: &Path,
    init_segment_path: &Path,
) -> Result<Option<String>, PyError> {
    let argv = vec![
        mp4dump.display().to_string(),
        "--format".into(),
        "json".into(),
        init_segment_path.display().to_string(),
    ];
    let options = RunOptions {
        max_output_chars: 64 * 1024,
        ..RunOptions::default()
    };
    let completed = runner
        .run_tool(&argv, "mp4dump", &options)
        .map_err(|error| PyError::Runtime(error.to_string()))?;
    if completed.returncode != Some(0) {
        return Err(PyError::Runtime(format!(
            "mp4dump failed for {}",
            init_segment_path.display()
        )));
    }
    let tree: serde_json::Value = serde_json::from_slice(&completed.stdout)
        .map_err(|error| PyError::Value(error.to_string()))?;
    let mut pending = vec![&tree];
    while let Some(node) = pending.pop() {
        match node {
            serde_json::Value::Object(map) => {
                if map.get("name").and_then(serde_json::Value::as_str) == Some("tenc") {
                    let iv = map
                        .get("default_constant_IV")
                        .and_then(serde_json::Value::as_str);
                    return Ok(iv.map(|text| {
                        text.trim_matches(['[', ']'])
                            .replace(' ', "")
                            .to_lowercase()
                    }));
                }
                pending.extend(map.values());
            }
            serde_json::Value::Array(items) => pending.extend(items.iter()),
            _ => {}
        }
    }
    Ok(None)
}

/// The DASH checks `DASH manifest validation` writes, in its order.
pub fn dash_checks() -> Vec<(CheckId, String)> {
    [
        CheckId::DashSchemeCbcs,
        CheckId::DashPlayreadySync,
        CheckId::DashWidevineUnchanged,
        CheckId::DashSegmenttemplateUnchanged,
        CheckId::DashOutOfScopeUntouched,
    ]
    .into_iter()
    .map(|id| (id, "PASS".to_string()))
    .collect()
}

/// The checks `manifest_hls.rewrite_hls` writes, in its order.
pub fn hls_checks() -> Vec<(CheckId, String)> {
    vec![
        (CheckId::HlsIvAgreement, "PASS".into()),
        (CheckId::HlsSegmentCount, "PASS".into()),
        (CheckId::HlsVersionUnchanged, "PASS".into()),
        (CheckId::AudioConsolidationStructure, "PASS".into()),
        (
            CheckId::AudioPcmIdentity,
            "SKIP: run audio_pcm_consolidation_gate with key material".into(),
        ),
        (
            CheckId::HlsContentGate,
            "SKIP: run content_identity_gate with key material + DASH clear output".into(),
        ),
        (
            CheckId::HlsExtinf,
            "SKIP: run check_extinf_gate (mp4dump/ffprobe over the CBCS output)".into(),
        ),
        (CheckId::HlsMasterUntouched, "PASS".into()),
    ]
}

/// A injected `manifest_hls.check_extinf_gate`.
pub type ExtinfGate = Box<dyn Fn(&HlsVariantMapping) -> Result<String, PyError> + Send + Sync>;

/// `manifest_dash` / `manifest_hls` with the gates injected: the rewrites copy the source
/// under the output root (mirroring its download-relative path), one variant mapping per DASH
/// rendition, and every call is recorded in order.
pub struct FakeManifests {
    pub la_url: Option<String>,
    pub pairs: Vec<(PathBuf, PathBuf)>,
    pub pcm_status: String,
    pub content_status: String,
    pub extinf: ExtinfGate,
    pub calls: Mutex<Vec<String>>,
}

impl Default for FakeManifests {
    fn default() -> Self {
        FakeManifests {
            la_url: Some(LA_URL.into()),
            pairs: Vec::new(),
            pcm_status: "PASS".into(),
            content_status: "PASS".into(),
            extinf: Box::new(|_| Ok("PASS".into())),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl FakeManifests {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

/// `path` relative to its `download` ancestor.
fn download_relative(path: &Path) -> PathBuf {
    let resolved = fs::canonicalize(path).unwrap();
    let root = resolved
        .ancestors()
        .find(|parent| parent.file_name().is_some_and(|name| name == "download"))
        .unwrap();
    resolved.strip_prefix(root).unwrap().to_path_buf()
}

impl Manifests for FakeManifests {
    fn parse_source_playready(&self, mpd_path: &Path) -> Result<ParsedPro, PyError> {
        self.record(format!("parse_source_playready {}", mpd_path.display()));
        Ok(ParsedPro {
            version: "4.0.0.0".into(),
            kid_values_b64: Vec::new(),
            la_url: self.la_url.clone(),
            algid: Some("AESCTR".into()),
            wrmheader_raw: Vec::new(),
            wrmheader_xml: String::new(),
            has_custom_attributes: false,
        })
    }

    fn rewrite_dash_manifest(
        &self,
        source_mpd: &Path,
        output_mpd: &Path,
        playready: &PlayReadyObjects,
        url_base: Option<&str>,
    ) -> Result<DashRewriteResult, PyError> {
        self.record(format!(
            "rewrite_dash_manifest {} {url_base:?}",
            output_mpd.display()
        ));
        fs::create_dir_all(output_mpd.parent().unwrap()).unwrap();
        fs::copy(source_mpd, output_mpd).map_err(|e| PyError::os(&e, source_mpd))?;
        Ok(DashRewriteResult {
            output_mpd_path: output_mpd.to_path_buf(),
            adaptation_sets_patched: 1,
            playready_pssh_b64: playready.pssh_b64.clone(),
            playready_pro_b64: playready.pro_b64.clone(),
            widevine_unchanged: true,
            checks: dash_checks(),
        })
    }

    fn find_audio_consolidation_pairs(
        &self,
        _master_path: &Path,
    ) -> Result<Vec<(PathBuf, PathBuf)>, PyError> {
        self.record("find_audio_consolidation_pairs".into());
        Ok(self.pairs.clone())
    }

    fn audio_pcm_consolidation_gate(
        &self,
        _runner: &Arc<ToolRunner>,
        _ts: &Path,
        _fmp4: &Path,
        _key: &KeyMaterial,
        work_dir: &Path,
    ) -> Result<String, PyError> {
        self.record(format!(
            "audio_pcm_consolidation_gate {}",
            work_dir.display()
        ));
        Ok(self.pcm_status.clone())
    }

    fn rewrite_hls(
        &self,
        master_path: &Path,
        dash_renditions: &[Rendition],
        output_root: &Path,
        expected_iv_hex: &str,
        url_base: Option<&str>,
    ) -> Result<HlsRewriteResult, PyError> {
        self.record(format!(
            "rewrite_hls {} {url_base:?}",
            output_root.display()
        ));
        assert_eq!(expected_iv_hex, IV, "the expected IV is the asset's");
        let output_master = output_root.join(download_relative(master_path));
        fs::create_dir_all(output_master.parent().unwrap()).unwrap();
        fs::copy(master_path, &output_master).unwrap();
        let variant_mapping = dash_renditions
            .iter()
            .map(|rendition| HlsVariantMapping {
                hls_playlist: master_path.with_file_name(format!("{}.m3u8", rendition.rep_id)),
                dash_rendition: rendition.clone(),
                signals: vec![("dash_rep_id".into(), rendition.rep_id.clone())],
                content_gate: if rendition.content_type == "video" {
                    "waived:TS-H264"
                } else {
                    "pcm-eq"
                }
                .into(),
            })
            .collect();
        Ok(HlsRewriteResult {
            output_master_path: output_master,
            output_media_playlist_paths: Vec::new(),
            audio_groups_consolidated: 0,
            ts_audio_groups_removed: 0,
            ext_x_map_added: 1,
            segment_refs_repointed: 3,
            iv_verified: true,
            variant_mapping,
            checks: hls_checks(),
        })
    }

    fn content_identity_gate(
        &self,
        _runner: &Arc<ToolRunner>,
        mapping: &HlsVariantMapping,
        _clear: &Path,
        _key: &KeyMaterial,
        _work_dir: &Path,
    ) -> Result<String, PyError> {
        self.record(format!(
            "content_identity_gate {}",
            mapping.dash_rendition.rep_id
        ));
        if mapping.content_gate == "waived:TS-H264" {
            return Ok("SKIP: TS H.264 decryption is not supported; segment count, duration, and key signaling checked separately".into());
        }
        Ok(self.content_status.clone())
    }

    fn check_extinf_gate(
        &self,
        _runner: &Arc<ToolRunner>,
        mapping: &HlsVariantMapping,
    ) -> Result<String, PyError> {
        self.record(format!(
            "check_extinf_gate {}",
            mapping.dash_rendition.rep_id
        ));
        (self.extinf)(mapping)
    }
}

/// `playready.build_playready_v43` with recognisable fake bytes.
#[derive(Default)]
pub struct FakePlayReady {
    pub calls: Mutex<Vec<(String, Option<String>)>>,
}

pub fn playready_objects(kid: &str) -> PlayReadyObjects {
    let pssh_box = [b"FAKE-PLAYREADY-PSSH:".as_slice(), kid.as_bytes()].concat();
    let pro_bytes = [b"FAKE-PRO:".as_slice(), kid.as_bytes()].concat();
    PlayReadyObjects {
        wrmheader_xml: format!("<WRMHEADER>{kid}</WRMHEADER>"),
        pro_b64: base64_encode(&pro_bytes),
        pssh_b64: base64_encode(&pssh_box),
        pssh_hex: hex::encode(&pssh_box),
        pro_bytes,
        pssh_box,
    }
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

impl PlayReady for FakePlayReady {
    fn build_playready_v43(
        &self,
        kid: &str,
        la_url: Option<&str>,
    ) -> Result<PlayReadyObjects, PyError> {
        self.calls
            .lock()
            .unwrap()
            .push((kid.into(), la_url.map(Into::into)));
        match la_url {
            Some(url) if !url.is_empty() => Ok(playready_objects(kid)),
            _ => Err(PyError::Value("LA_URL must be non-empty".into())),
        }
    }
}

/// `config` over registered synthetic assets: `load_key_material` as Python validates it,
/// `load_asset` the registered enumeration (no manifest parsing — that is configuration's).
#[derive(Default)]
pub struct FakeConfig {
    pub assets: Mutex<Vec<SyntheticAsset>>,
    pub loads: Mutex<Vec<Load>>,
}

/// One `load_asset` call: `(mpd_path, key_hex, output_root, asset_id)` (synthetic keys only).
pub type Load = (PathBuf, String, PathBuf, Option<String>);

impl FakeConfig {
    pub fn register(&self, asset: &SyntheticAsset) {
        self.assets.lock().unwrap().push(asset.clone());
    }
}

impl Config for FakeConfig {
    fn load_key_material(&self, env: &HashMap<String, String>) -> Result<KeyMaterial, PyError> {
        let missing: Vec<&str> = ["DRM_KID", "DRM_KEY", "DRM_IV"]
            .into_iter()
            .filter(|name| env.get(*name).is_none_or(String::is_empty))
            .collect();
        if !missing.is_empty() {
            return Err(PyError::Value(format!(
                "missing required env var(s): {}",
                missing.join(", ")
            )));
        }
        for (name, field) in [
            ("DRM_KID", "key_id_hex"),
            ("DRM_KEY", "key_hex"),
            ("DRM_IV", "iv_hex"),
        ] {
            let Ok(raw) = hex::decode(&env[name]) else {
                return Err(PyError::Value(format!("{field} must be a hex string")));
            };
            if raw.len() != 16 {
                return Err(PyError::Value(format!(
                    "{field} must decode to 16 bytes, got {}",
                    raw.len()
                )));
            }
        }
        Ok(KeyMaterial::new(
            env["DRM_KID"].as_str(),
            env["DRM_KEY"].as_str(),
            env["DRM_IV"].as_str(),
        )?)
    }

    fn load_asset(
        &self,
        mpd_path: &Path,
        key: KeyMaterial,
        output_root: &Path,
        asset_id: Option<&str>,
    ) -> Result<Asset, PyError> {
        self.loads.lock().unwrap().push((
            mpd_path.to_path_buf(),
            key.key_hex().to_string(),
            output_root.to_path_buf(),
            asset_id.map(Into::into),
        ));
        let wanted = fs::canonicalize(mpd_path).unwrap();
        let assets = self.assets.lock().unwrap();
        let synthetic = assets
            .iter()
            .find(|asset| fs::canonicalize(&asset.mpd_path).unwrap() == wanted)
            .ok_or_else(|| PyError::Value(format!("{}: no <BaseURL> found", mpd_path.display())))?;
        let id = asset_id
            .filter(|id| !id.is_empty())
            .unwrap_or(&synthetic.title_id);
        Ok(Asset {
            key,
            mpd_path: mpd_path.to_path_buf(),
            ..synthetic.asset_as(output_root, id)
        })
    }
}

/// A tool runner with no sink (Python without `install_sink`).
pub fn quiet_runner() -> Arc<ToolRunner> {
    Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)))
}

/// A tool runner whose records are kept.
pub fn collecting_runner() -> (Arc<ToolRunner>, Arc<Collector>) {
    let collector = Arc::new(Collector::default());
    (
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::with([KEY, IV])),
            collector.clone(),
        )),
        collector,
    )
}

/// The fakes the pipeline runs on: the stub packager on `PATH`, empty media.
pub struct Fakes {
    pub packager: FakePackager,
    pub media: FakeMedia,
    pub manifests: FakeManifests,
    pub runner: Arc<ToolRunner>,
    pub bin: tempfile::TempDir,
}

impl Fakes {
    /// `packager_on_path` with `STUB_PACKAGER_EXIT=exit_code`.
    pub fn with_exit(exit_code: i32) -> Fakes {
        let bin = tempfile::tempdir().unwrap();
        let stub = write_stub_packager(bin.path(), exit_code, None, None);
        Fakes {
            packager: FakePackager::on_path(stub),
            media: FakeMedia::default(),
            manifests: FakeManifests::default(),
            runner: quiet_runner(),
            bin,
        }
    }

    pub fn new() -> Fakes {
        Fakes::with_exit(0)
    }

    pub fn deps(&self) -> PipelineDeps<'_> {
        PipelineDeps {
            packager: &self.packager,
            media_inspect: &self.media,
            manifests: &self.manifests,
            runner: &self.runner,
            packager_bin: "packager",
        }
    }
}

/// A shared, clonable byte sink standing in for stdout / stderr.
#[derive(Clone, Default)]
pub struct SharedBuf(pub Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
