#![allow(dead_code)] // reason: each test binary uses its own subset of these helpers

use base64::Engine as _;
use drmr_server::config::{Clock, Hooks, QueueRunner};
use drmr_server::{RunningServer, ServerConfig};
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const KID: &str = "000102030405060708090a0b0c0d0e0f";
pub const KEY: &str = "10101010101010101010101010101010";
pub const IV: &str = "20202020202020202020202020202020";
/// A second, distinct synthetic set (cross-asset isolation).
pub const B_KID: &str = "101112131415161718191a1b1c1d1e1f";
pub const B_KEY: &str = "30303030303030303030303030303030";
pub const B_IV: &str = "40404040404040404040404040404040";
pub const LA_URL: &str = "https://license.example.invalid/rightsmanager.asmx";
pub const FAIRPLAY: &str = "skd://sample-content-id";

/// Unpadded base64url of 16 hex-encoded bytes (the W3C `ClearKey` wire form), computed here
/// independently of the server.
pub fn b64url(hex_text: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hex::decode(hex_text).unwrap())
}

// ------------------------------------------------------------------------------------------
// The fake CLI
// ------------------------------------------------------------------------------------------

/// The fake CLI and its control directory. Every launch records its argv and DRM variables
/// under `calls/<n>/` (n = launch order, claimed with `mkdir`), prints the `lines` file, then:
/// with a `convert` file it writes a converted output tree (the stub packager's role); with a
/// `hold` file it waits for a `release` file (holding the exit code) and removes it; otherwise
/// it exits with the code in `code` (0 by default).
pub struct FakeCli {
    pub dir: PathBuf,
    pub script: PathBuf,
}

impl FakeCli {
    /// A fake CLI under `root/cli`.
    pub fn new(root: &Path) -> FakeCli {
        let dir = root.join("cli");
        drmr_core::native::create_private_dir_all(&dir.join("calls")).unwrap();
        let spec = drmr_test_support::ToolSpec {
            mode: drmr_test_support::Mode::ServerCli,
            ..Default::default()
        }
        .with("control_dir", dir.to_str().unwrap());
        let script = drmr_test_support::install(&dir.join("drmr"), &spec).unwrap();
        FakeCli { dir, script }
    }

    /// Lines the next launches print.
    pub fn lines(&self, lines: &[&str]) {
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::write(self.dir.join("lines"), text).unwrap();
    }

    /// Exit with this code (no hold).
    pub fn exit_code(&self, code: i32) {
        std::fs::write(self.dir.join("code"), code.to_string()).unwrap();
    }

    /// Hold every launch until [`FakeCli::release`].
    pub fn hold(&self) {
        std::fs::write(self.dir.join("hold"), "").unwrap();
    }

    /// Stop holding new launches.
    pub fn unhold(&self) {
        let _ = std::fs::remove_file(self.dir.join("hold"));
    }

    /// Let the held launch exit with this code.
    pub fn release(&self, code: i32) {
        let temporary = self.dir.join("release.pending");
        std::fs::write(&temporary, code.to_string()).unwrap();
        std::fs::rename(temporary, self.dir.join("release")).unwrap();
    }

    /// Write a converted output tree (the stub packager's role).
    pub fn convert(&self) {
        std::fs::write(self.dir.join("convert"), "").unwrap();
    }

    /// The number of launches so far.
    pub fn calls(&self) -> usize {
        std::fs::read_dir(self.dir.join("calls")).unwrap().count()
    }

    /// Wait until `count` launches have recorded themselves.
    pub fn wait_calls(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(15);
        wait_until(|| {
            if Instant::now() >= deadline {
                let calls: Vec<_> = std::fs::read_dir(self.dir.join("calls"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                panic!("native CLI did not publish call readiness: {calls:?}");
            }
            self.calls() >= count
                && self
                    .dir
                    .join("calls")
                    .join((count - 1).to_string())
                    .join("inherited")
                    .exists()
        });
    }

    /// The argv of launch `n` (after the program).
    pub fn argv(&self, n: usize) -> Vec<String> {
        self.wait_calls(n + 1);
        read_lines(&self.dir.join("calls").join(n.to_string()).join("argv"))
    }

    /// `DRM_KID`, `DRM_KEY`, `DRM_IV` of launch `n` (empty when unset).
    pub fn drm(&self, n: usize) -> Vec<String> {
        self.wait_calls(n + 1);
        read_lines(&self.dir.join("calls").join(n.to_string()).join("drm"))
    }

    /// `PATH` and `AMBIENT_MARKER` of launch `n`.
    pub fn inherited(&self, n: usize) -> Vec<String> {
        self.wait_calls(n + 1);
        read_lines(&self.dir.join("calls").join(n.to_string()).join("inherited"))
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Poll `condition` for up to 15 s.
pub fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not met within 15 s");
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ------------------------------------------------------------------------------------------
// Synthetic catalog staging
// ------------------------------------------------------------------------------------------

/// One Representation of the synthetic DASH asset (`RepSpec`).
#[derive(Clone, Debug)]
pub struct Rep {
    pub id: String,
    pub codec: String,
    pub content_type: String,
    pub mime_type: String,
    pub timescale: u32,
    pub duration_ticks: u32,
    pub start_number: u32,
    pub segment_count: usize,
    pub include_widevine: bool,
    pub include_playready: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub lang: Option<String>,
}

fn rep(id: &str, codec: &str, content_type: &str, mime_type: &str) -> Rep {
    Rep {
        id: id.into(),
        codec: codec.into(),
        content_type: content_type.into(),
        mime_type: mime_type.into(),
        timescale: 24_000,
        duration_ticks: 144_000,
        start_number: 0,
        segment_count: 3,
        include_widevine: true,
        include_playready: true,
        width: None,
        height: None,
        lang: None,
    }
}

pub fn h264_rep(id: &str) -> Rep {
    rep(id, "avc1.42C00C", "video", "video/mp4")
}

pub fn h265_rep(id: &str) -> Rep {
    rep(id, "hvc1.1.6.L60.90", "video", "video/mp4")
}

pub fn vp9_rep(id: &str) -> Rep {
    Rep {
        include_playready: false,
        ..rep(id, "vp9", "video", "video/webm")
    }
}

pub fn audio_rep(id: &str) -> Rep {
    Rep {
        timescale: 48_000,
        duration_ticks: 288_000,
        ..rep(id, "mp4a.40.2", "audio", "audio/mp4")
    }
}

/// A v4.3.0.0 / AESCBC `PlayReady` object for the synthetic KID and `LA_URL`, as `(pssh_b64,
/// pro_b64)`: WRMHEADER (LE-GUID KID), UTF-16LE, PRO framing, `pssh` box. Test fixture only.
pub fn playready_objects() -> (String, String) {
    let kid = hex::decode(KID).unwrap();
    let le: Vec<u8> = [&kid[0..4], &kid[4..6], &kid[6..8]]
        .iter()
        .flat_map(|part| part.iter().rev().copied())
        .chain(kid[8..].iter().copied())
        .collect();
    let value = base64::engine::general_purpose::STANDARD.encode(le);
    let header = format!(
        "<WRMHEADER xmlns=\"http://schemas.microsoft.com/DRM/2007/03/PlayReadyHeader\" version=\"4.3.0.0\"><DATA><PROTECTINFO><KIDS><KID ALGID=\"AESCBC\" VALUE=\"{value}\"></KID></KIDS></PROTECTINFO><LA_URL>{LA_URL}</LA_URL></DATA></WRMHEADER>"
    );
    let record: Vec<u8> = header.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut pro = Vec::new();
    pro.extend_from_slice(&u32::try_from(10 + record.len()).unwrap().to_le_bytes());
    pro.extend_from_slice(&1u16.to_le_bytes());
    pro.extend_from_slice(&1u16.to_le_bytes());
    pro.extend_from_slice(&u16::try_from(record.len()).unwrap().to_le_bytes());
    pro.extend_from_slice(&record);
    let system_id = hex::decode("9a04f07998404286ab92e65be0885f95").unwrap();
    let mut pssh = Vec::new();
    pssh.extend_from_slice(&u32::try_from(32 + pro.len()).unwrap().to_be_bytes());
    pssh.extend_from_slice(b"pssh");
    pssh.extend_from_slice(&[0, 0, 0, 0]);
    pssh.extend_from_slice(&system_id);
    pssh.extend_from_slice(&u32::try_from(pro.len()).unwrap().to_be_bytes());
    pssh.extend_from_slice(&pro);
    let b64 = |data: &[u8]| base64::engine::general_purpose::STANDARD.encode(data);
    (b64(&pssh), b64(&pro))
}

fn quoteattr(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    )
}

/// `build_synthetic_dash_asset`: `download/manifest.example.test/<title>/dash/manifest.mpd`
/// and, per rep, `init.<ext>` + `seg_<i>.m4s` under `download/cdn.example.test/<title>/<type>/<id>/`.
pub fn build_synthetic_dash_asset(root: &Path, reps: &[Rep], title_id: &str) -> PathBuf {
    let (pssh, pro) = playready_objects();
    let widevine = base64::engine::general_purpose::STANDARD
        .encode(b"FAKE-PSSH-NOT-A-REAL-BOX-JUST-TEST-BYTES");
    let kid_uuid = format!(
        "{}-{}-{}-{}-{}",
        &KID[..8],
        &KID[8..12],
        &KID[12..16],
        &KID[16..20],
        &KID[20..]
    );
    let mut body = String::new();
    for rep in reps {
        let mut protection = format!(
            "<ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"cenc\" cenc:default_KID=\"{kid_uuid}\" xmlns:cenc=\"urn:mpeg:cenc:2013\"/>"
        );
        if rep.include_widevine {
            write!(
                protection,
                "<ContentProtection schemeIdUri=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\"><cenc:pssh xmlns:cenc=\"urn:mpeg:cenc:2013\">{widevine}</cenc:pssh></ContentProtection>"
            )
            .unwrap();
        }
        if rep.include_playready {
            write!(
                protection,
                "<ContentProtection schemeIdUri=\"urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95\"><cenc:pssh xmlns:cenc=\"urn:mpeg:cenc:2013\">{pssh}</cenc:pssh><mspr:pro xmlns:mspr=\"urn:microsoft:playready\">{pro}</mspr:pro></ContentProtection>"
            )
            .unwrap();
        }
        let ext = if rep.mime_type == "video/webm" {
            "hdr"
        } else {
            "mp4"
        };
        let shape = match (rep.width, rep.height) {
            (Some(width), Some(height)) => format!(" width=\"{width}\" height=\"{height}\""),
            _ => String::new(),
        };
        let lang = rep
            .lang
            .as_ref()
            .map_or(String::new(), |lang| format!(" lang=\"{lang}\""));
        write!(
            body,
            "<AdaptationSet mimeType=\"{}\" contentType=\"{}\"{lang}>{protection}<Representation id={} codecs={} bandwidth=\"1\"{shape}><SegmentTemplate timescale=\"{}\" startNumber=\"{}\" duration=\"{}\" media=\"{}/{}/seg_$Number$.m4s\" initialization=\"{}/{}/init.{ext}\"/></Representation></AdaptationSet>",
            rep.mime_type, rep.content_type, quoteattr(&rep.id), quoteattr(&rep.codec), rep.timescale,
            rep.start_number, rep.duration_ticks, rep.content_type, rep.id, rep.content_type, rep.id,
        )
        .unwrap();
    }
    let mpd = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" mediaPresentationDuration=\"PT18.0S\" minBufferTime=\"PT2S\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\"><BaseURL>https://cdn.example.test/{title_id}/</BaseURL><Period id=\"p0\">{body}</Period></MPD>\n"
    );
    let download = root.join("download");
    let mpd_path = download
        .join("manifest.example.test")
        .join(title_id)
        .join("dash")
        .join("manifest.mpd");
    drmr_core::native::create_private_dir_all(mpd_path.parent().unwrap()).unwrap();
    std::fs::write(&mpd_path, mpd).unwrap();
    for rep in reps {
        let ext = if rep.mime_type == "video/webm" {
            "hdr"
        } else {
            "mp4"
        };
        let dir = download
            .join("cdn.example.test")
            .join(title_id)
            .join(&rep.content_type)
            .join(&rep.id);
        drmr_core::native::create_private_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("init.{ext}")), format!("INIT:{}", rep.id)).unwrap();
        for i in 0..rep.segment_count {
            std::fs::write(
                dir.join(format!("seg_{i}.m4s")),
                format!("SEG:{}:{i}", rep.id),
            )
            .unwrap();
        }
    }
    mpd_path
}

/// `_write_minimal_hls_master`: one TS variant, enough for the CLI's `--hls-master`.
pub fn write_minimal_hls_master(root: &Path, title_id: &str, iv_hex: &str) -> PathBuf {
    let segments = root
        .join("download/vod.example.test")
        .join(title_id)
        .join("video/hv0");
    drmr_core::native::create_private_dir_all(&segments).unwrap();
    for i in 0..3 {
        std::fs::write(segments.join(format!("seg_{i}.ts")), [0u8; 100]).unwrap();
    }
    let variants = root
        .join("download/variants.example.test")
        .join(title_id)
        .join("video/hv0");
    drmr_core::native::create_private_dir_all(&variants).unwrap();
    let mut lines = vec![
        "#EXTM3U".to_string(),
        "#EXT-X-VERSION:7".into(),
        "#EXT-X-PLAYLIST-TYPE:VOD".into(),
        "#EXT-X-TARGETDURATION:6".into(),
        "#EXT-X-MEDIA-SEQUENCE:0".into(),
        format!(
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{FAIRPLAY}\",IV=0x{},KEYFORMAT=\"com.apple.streamingkeydelivery\"",
            iv_hex.to_uppercase()
        ),
    ];
    for i in 0..3 {
        lines.push("#EXTINF:6,".into());
        lines.push(format!(
            "https://vod.example.test/{title_id}/video/hv0/seg_{i}.ts"
        ));
    }
    lines.push("#EXT-X-ENDLIST".into());
    std::fs::write(variants.join("video_100_x.m3u8"), lines.join("\n") + "\n").unwrap();
    let master_dir = root
        .join("download/manifest.example.test")
        .join(title_id)
        .join("hls");
    drmr_core::native::create_private_dir_all(&master_dir).unwrap();
    let master = master_dir.join("manifest.m3u8");
    std::fs::write(
        &master,
        format!(
            "#EXTM3U\n#EXT-X-VERSION:4\n#EXT-X-STREAM-INF:BANDWIDTH=100000,CODECS=\"avc1.42C00C,mp4a.40.2\",RESOLUTION=320x180,FRAME-RATE=24.000\nhttps://variants.example.test/{title_id}/video/hv0/video_100_x.m3u8\n"
        ),
    )
    .unwrap();
    master
}

/// How `stage_entry` stages an entry.
#[derive(Clone, Debug)]
pub struct Stage {
    pub label: String,
    pub kid: String,
    pub key: String,
    pub iv: String,
    pub reps: Vec<Rep>,
    pub dir_name: Option<String>,
}

impl Default for Stage {
    fn default() -> Self {
        Stage {
            label: "Test Asset".into(),
            kid: KID.into(),
            key: KEY.into(),
            iv: IV.into(),
            reps: vec![h264_rep("v0"), audio_rep("a0")],
            dir_name: None,
        }
    }
}

/// `_stage_entry(catalog_root, asset_id, …)`: `asset.json` + `keys.env` + the `download/` tree;
/// returns the entry directory and the manifest path.
pub fn stage_entry(catalog_root: &Path, asset_id: &str, stage: &Stage) -> (PathBuf, PathBuf) {
    let entry = catalog_root.join(stage.dir_name.as_deref().unwrap_or(asset_id));
    drmr_core::native::create_private_dir_all(&entry).unwrap();
    let mpd = build_synthetic_dash_asset(&entry, &stage.reps, asset_id);
    let master = write_minimal_hls_master(&entry, asset_id, &stage.iv);
    write_private(
        &entry.join("keys.env"),
        format!(
            "DRM_KID={}\nDRM_KEY={}\nDRM_IV={}\n",
            stage.kid, stage.key, stage.iv
        ),
    )
    .unwrap();
    let relative = |path: &Path| {
        path.strip_prefix(&entry)
            .unwrap()
            .to_str()
            .unwrap()
            .replace(std::path::MAIN_SEPARATOR, "/")
    };
    std::fs::write(
        entry.join("asset.json"),
        serde_json::to_string(&json!({
            "asset_id": asset_id,
            "label": stage.label,
            "mpd_path": relative(&mpd),
            "hls_master_path": relative(&master),
            "keys_file": "keys.env",
        }))
        .unwrap(),
    )
    .unwrap();
    (entry, mpd)
}

/// `stage_entry` with the defaults.
pub fn stage(catalog_root: &Path, asset_id: &str) -> (PathBuf, PathBuf) {
    stage_entry(catalog_root, asset_id, &Stage::default())
}

// ------------------------------------------------------------------------------------------
// The server and the client
// ------------------------------------------------------------------------------------------

/// An HTTP answer.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Answer {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.text()))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A client for one server.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    pub base: String,
}

impl Client {
    pub fn new(base: &str) -> Client {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .new_agent();
        Client {
            agent,
            base: base.to_string(),
        }
    }

    /// Any request: method, path (sent as written), headers, body.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Answer {
        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        // Admission can reject a request before reading its body. Wait for the
        // server's decision instead of racing a large upload against its close.
        if body.is_some_and(|body| !body.is_empty())
            && !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("expect"))
        {
            builder = builder.header("Expect", "100-continue");
        }
        let response = match body {
            Some(body) => self.agent.run(builder.body(body.to_vec()).unwrap()),
            None => self.agent.run(builder.body(()).unwrap()),
        };
        let mut response = response.unwrap_or_else(|error| panic!("{method} {path}: {error}"));
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = if method == "HEAD" {
            Vec::new()
        } else {
            response
                .body_mut()
                .with_config()
                .limit(u64::MAX)
                .read_to_vec()
                .unwrap_or_default()
        };
        Answer {
            status,
            headers,
            body,
        }
    }

    pub fn get(&self, path: &str) -> Answer {
        self.request("GET", path, &[], None)
    }

    pub fn get_with(&self, path: &str, headers: &[(&str, &str)]) -> Answer {
        self.request("GET", path, headers, None)
    }

    pub fn delete(&self, path: &str) -> Answer {
        self.request("DELETE", path, &[], None)
    }

    pub fn post_json(&self, path: &str, value: &Value) -> Answer {
        let body = serde_json::to_vec(value).unwrap();
        self.request(
            "POST",
            path,
            &[("Content-Type", "application/json")],
            Some(&body),
        )
    }

    pub fn post_json_with(&self, path: &str, value: &Value, headers: &[(&str, &str)]) -> Answer {
        let body = serde_json::to_vec(value).unwrap();
        let mut all = vec![("Content-Type", "application/json")];
        all.extend_from_slice(headers);
        self.request("POST", path, &all, Some(&body))
    }

    pub fn post_raw(&self, path: &str, body: &[u8], headers: &[(&str, &str)]) -> Answer {
        self.request("POST", path, headers, Some(body))
    }
}

/// A server over temporary roots with a fake CLI and collected diagnostic lines.
pub struct Harness {
    pub root: tempfile::TempDir,
    pub catalog: PathBuf,
    pub output: PathBuf,
    pub cli: FakeCli,
    pub log: Arc<Mutex<Vec<String>>>,
    pub server: Option<RunningServer>,
    pub options: Option<Options>,
    pub client: Client,
}

/// What a test changes in the server configuration.
pub struct Options {
    pub hls_profiles: Option<PathBuf>,
    pub max_workers: Option<u32>,
    pub monitor: bool,
    pub queue_runner: QueueRunner,
    pub hls_clock: Option<Clock>,
    pub ambient: Vec<(&'static str, &'static str)>,
    /// Every diagnostic write fails after being captured, as on a closed stderr pipe.
    pub log_fails: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            hls_profiles: None,
            max_workers: None,
            monitor: true,
            queue_runner: QueueRunner::Thread,
            hls_clock: None,
            ambient: Vec::new(),
            log_fails: false,
        }
    }
}

impl Harness {
    pub fn new() -> Harness {
        Harness::with(|_| {})
    }

    /// A harness whose roots and fake CLI exist before `configure` sets the options.
    pub fn with(configure: impl FnOnce(&mut HarnessSetup)) -> Harness {
        let root = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(root.path()).unwrap().join("private");
        drmr_core::native::create_private_dir_all(&base).unwrap();
        let catalog = base.join("catalog");
        let output = base.join("output");
        drmr_core::native::create_private_dir_all(&catalog).unwrap();
        drmr_core::native::create_private_dir_all(&output).unwrap();
        let cli = FakeCli::new(&base);
        let mut setup = HarnessSetup {
            base: base.clone(),
            catalog: catalog.clone(),
            output: output.clone(),
            options: Options::default(),
        };
        configure(&mut setup);
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut harness = Harness {
            root,
            catalog,
            output,
            cli,
            log,
            server: None,
            options: None,
            client: Client::new("http://127.0.0.1:1"),
        };
        harness.options = Some(setup.options);
        harness.start();
        harness
    }

    fn config(&self) -> ServerConfig {
        let options = self.options.as_ref().unwrap();
        let mut config = ServerConfig::new(self.catalog.clone(), self.output.clone());
        config.bind_port = 0;
        config.cli_argv = vec![OsString::from(self.cli.script.clone())];
        config.hls_profiles.clone_from(&options.hls_profiles);
        config.max_workers = options.max_workers;
        let log = self.log.clone();
        let log_fails = options.log_fails;
        let ambient = options.ambient.clone();
        config.hooks = Hooks {
            log: Arc::new(move |line: &str| {
                log.lock().unwrap().push(line.to_string());
                if log_fails {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                Ok(())
            }),
            monitor: options.monitor,
            queue_runner: options.queue_runner,
            hls_clock: options.hls_clock.clone(),
            queue_clock: None,
            tail_poll: Duration::from_millis(10),
            base_env: Arc::new(move || {
                let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
                env.extend(
                    ambient
                        .iter()
                        .map(|(name, value)| (OsString::from(name), OsString::from(value))),
                );
                env
            }),
        };
        config
    }

    /// Start (or restart: `create_app` again over the same roots) the server.
    pub fn start(&mut self) {
        if let Some(server) = self.server.take() {
            server.app.manager.kill_current();
            server.stop();
        }
        let server = drmr_server::spawn(self.config()).unwrap();
        self.client = Client::new(&server.url());
        self.server = Some(server);
    }

    /// A second server over the same roots (the first keeps running).
    pub fn second(&self) -> (RunningServer, Client) {
        let server = drmr_server::spawn(self.config()).unwrap();
        let client = Client::new(&server.url());
        (server, client)
    }

    pub fn app(&self) -> &Arc<drmr_server::App> {
        &self.server.as_ref().unwrap().app
    }

    pub fn origin(&self) -> String {
        self.client.base.clone()
    }

    pub fn logs(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// Poll `GET /api/jobs/<id>` until the job is no longer running; its answer.
    pub fn wait_job(&self, job_id: &str) -> Value {
        let mut last = Value::Null;
        wait_until(|| {
            last = self.client.get(&format!("/api/jobs/{job_id}")).json();
            last["state"] != "running"
        });
        last
    }
}

/// The options a harness is built with.
pub struct HarnessSetup {
    pub base: PathBuf,
    pub catalog: PathBuf,
    pub output: PathBuf,
    pub options: Options,
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.app.manager.kill_current();
            server.stop();
        }
    }
}

/// `{"error": token}` plus the detail.
pub fn error(token: &str) -> Value {
    json!({ "error": token })
}

/// Parse a `text/event-stream` body into `(event, data)` pairs (`message` for unnamed events).
pub fn sse_events(text: &str) -> Vec<(String, String)> {
    text.split("\n\n")
        .filter(|block| !block.is_empty())
        .map(|block| {
            let mut name = "message".to_string();
            let mut data = Vec::new();
            for line in block.split('\n') {
                if let Some(value) = line.strip_prefix("event: ") {
                    name = value.to_string();
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data.push(value.to_string());
                }
            }
            (name, data.join("\n"))
        })
        .collect()
}

/// A JSON object from pairs (insertion order kept).
pub fn object(pairs: &[(&str, Value)]) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    Value::Object(map)
}

/// Whether `name` ends in `.<extension>` (case-sensitive, as the server's own names are).
pub fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|found| found == extension)
}

/// Whether the tests run as root (permission denials are not enforced then).
pub fn is_root() -> bool {
    // `id -u` rather than `libc::geteuid` (unsafe code is forbidden in this workspace).
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "0")
}

/// Write a synthetic secret with the same private-file boundary as production.
pub fn write_private(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        drmr_core::native::create_private_dir_all(parent)?;
    }
    drmr_core::native::create_private_or_truncate(path)?.write_all(bytes.as_ref())
}

pub fn private_dir(path: impl AsRef<Path>) -> std::io::Result<()> {
    drmr_core::native::create_private_dir_all(path.as_ref())
}

#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Apply actual Windows access denials while retaining access to restore the DACL.
#[cfg(windows)]
pub fn set_mode(path: &Path, mode: u32) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_permissions::constants::{SeObjectType, SecurityInformation};
    use windows_permissions::{LocalBox, SecurityDescriptor};
    let mut control = std::fs::OpenOptions::new()
        .access_mode(0x0006_0000)
        .custom_flags(0x0200_0000)
        .open(path)
        .unwrap();
    let original = windows_permissions::wrappers::GetSecurityInfo(
        &control,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner | SecurityInformation::Dacl,
    )
    .unwrap();
    let denied = match mode {
        0o000 => 0x0001_0067,
        0o500 => 0x0001_0046,
        _ => 0,
    };
    let sid = original.owner().unwrap();
    let deny = if denied == 0 {
        String::new()
    } else {
        format!("(D;;0x{denied:08x};;;{sid})")
    };
    let descriptor: LocalBox<SecurityDescriptor> =
        format!("D:P{deny}(A;;FA;;;{sid})").parse().unwrap();
    windows_permissions::wrappers::SetSecurityInfo(
        &mut control,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        descriptor.dacl(),
        None,
    )
    .unwrap();
}

#[cfg(unix)]
pub fn directory_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}
#[cfg(windows)]
pub fn directory_link(target: &Path, link: &Path) {
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[cfg(unix)]
pub fn file_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}
#[cfg(windows)]
pub fn file_link(target: &Path, link: &Path) {
    std::fs::hard_link(target, link).unwrap();
}
