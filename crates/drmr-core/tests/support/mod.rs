//! Shared clear-HLS fixture builders, injectable fetchers, and media probes.
// Each test binary uses a different subset of these helpers; the probe stand-ins keep the
// `Prober` signature (a `Result`) although they never fail; `fixed` and the playlist
// builders take their lists by value because every call site builds them inline.
#![allow(dead_code, clippy::unnecessary_wraps, clippy::needless_pass_by_value)]

pub mod hls_fixture;
pub mod server;

use drmr_core::events::Sink;
use drmr_core::source::platform::{Platform, SystemPlatform};
use drmr_core::source::{
    AnalyzeOptions, ByteRange, Direct, Failure, FetchHooks, FetchPolicy, Fetched, Fetcher,
    ProbeStream, Prober, SourceAnalysis, SourceRequest, analyze_hls_with,
};
use serde_json::Value;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Tests that measure time hold this lock: they run one at a time, as pytest runs them.
static TIMING: Mutex<()> = Mutex::new(());

/// Hold the timing lock for the rest of the test.
pub fn timing() -> MutexGuard<'static, ()> {
    TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A stream object from a JSON literal.
pub fn stream(value: Value) -> ProbeStream {
    match value {
        Value::Object(map) => map,
        other => panic!("a stream is an object, not {other}"),
    }
}

/// Streams from JSON literals.
pub fn streams(values: &[Value]) -> Vec<ProbeStream> {
    values.iter().cloned().map(stream).collect()
}

/// The `source` fixture's probe: `WEBVTT` is a `WebVTT` subtitle stream, anything else an
/// H.264 640x360 video stream.
pub fn default_probe(path: &Path, _init: Option<&Path>) -> Result<Vec<ProbeStream>, Failure> {
    let data = std::fs::read(path).unwrap();
    if data.starts_with(b"WEBVTT") {
        return Ok(streams(&[
            serde_json::json!({"index": 0, "codec_type": "subtitle", "codec_name": "webvtt"}),
        ]));
    }
    Ok(streams(&[
        serde_json::json!({"index": 0, "codec_type": "video", "codec_name": "h264", "width": 640, "height": 360}),
    ]))
}

/// A probe that always returns `values`.
pub fn fixed(values: Vec<Value>) -> impl Prober {
    let streams = streams(&values);
    move |_: &Path, _: Option<&Path>| Ok(streams.clone())
}

/// `_VIDEO`.
pub fn video() -> Vec<Value> {
    vec![
        serde_json::json!({"index": 0, "codec_type": "video", "codec_name": "h264", "width": 640, "height": 360}),
    ]
}

/// `media(root, body)`: `master.m3u8` (one 6 s TS segment by default) and `segment.ts`.
pub fn media(root: &Path, body: Option<&str>) {
    let body = body
        .unwrap_or("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n");
    std::fs::write(root.join("master.m3u8"), body).unwrap();
    let mut segment = vec![0x47_u8];
    segment.extend(std::iter::repeat_n(b'x', 187));
    std::fs::write(root.join("segment.ts"), segment).unwrap();
}

/// A temporary test directory with an `input` directory (`tmp_path` and the `source` fixture).
pub struct Workspace {
    pub tmp: tempfile::TempDir,
    pub source: PathBuf,
}

impl Workspace {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("input");
        std::fs::create_dir(&source).unwrap();
        Workspace { tmp, source }
    }

    /// `tmp_path`.
    pub fn path(&self) -> &Path {
        self.tmp.path()
    }

    /// The policy of `analyze`: the input directory as the one local root.
    pub fn policy(&self) -> FetchPolicy {
        FetchPolicy {
            allowed_local_roots: vec![self.source.clone()],
            ..FetchPolicy::default()
        }
    }

    /// `analyze(root, tmp_path, name, **limits)` with `probe`.
    pub fn analyze_with(
        &self,
        name: &str,
        policy: &FetchPolicy,
        probe: &dyn Prober,
    ) -> Result<SourceAnalysis, Failure> {
        Analysis::new(probe).run(
            &SourceRequest::new(self.source.join(name).to_string_lossy()),
            "test-asset",
            &self.path().join("output"),
            policy,
        )
    }

    /// `analyze(root, tmp_path)` with the `source` fixture's probe.
    pub fn analyze(&self) -> Result<SourceAnalysis, Failure> {
        self.analyze_with("master.m3u8", &self.policy(), &default_probe)
    }
}

/// One `analyze_hls_with` call and its collaborators.
pub struct Analysis<'a> {
    pub prober: &'a dyn Prober,
    pub hooks: &'a dyn FetchHooks,
    pub platform: Arc<dyn Platform>,
    pub on_log: Option<&'a dyn Sink>,
}

impl<'a> Analysis<'a> {
    pub fn new(prober: &'a dyn Prober) -> Self {
        Analysis {
            prober,
            hooks: &Direct,
            platform: Arc::new(SystemPlatform),
            on_log: None,
        }
    }

    pub fn hooks(mut self, hooks: &'a dyn FetchHooks) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn platform(mut self, platform: Arc<dyn Platform>) -> Self {
        self.platform = platform;
        self
    }

    pub fn log(mut self, sink: &'a dyn Sink) -> Self {
        self.on_log = Some(sink);
        self
    }

    pub fn run(
        &self,
        source: &SourceRequest,
        asset_id: &str,
        output_root: &Path,
        policy: &FetchPolicy,
    ) -> Result<SourceAnalysis, Failure> {
        let options = AnalyzeOptions {
            on_log: self.on_log,
            prober: self.prober,
            platform: Arc::clone(&self.platform),
            hooks: self.hooks,
        };
        analyze_hls_with(source, asset_id, output_root, policy, &options)
    }
}

/// A stand-in for `_Fetcher.read`.
pub type ReadFn =
    Box<dyn Fn(&Fetcher, &str, bool) -> Result<(Vec<u8>, String), Failure> + Send + Sync>;
/// A stand-in for `_Fetcher.fetch`.
pub type FetchFn =
    Box<dyn Fn(&Fetcher, &str, &Path, Option<ByteRange>) -> Result<Fetched, Failure> + Send + Sync>;

/// The fetcher with `read` and / or `fetch` replaced, as the tests dependency injection them;
/// a replacement may call the original (`fetcher.read` / `fetcher.fetch`).
#[derive(Default)]
pub struct FnHooks {
    pub read: Option<ReadFn>,
    pub fetch: Option<FetchFn>,
}

impl FetchHooks for FnHooks {
    fn read(
        &self,
        fetcher: &Fetcher,
        url: &str,
        playlist: bool,
    ) -> Result<(Vec<u8>, String), Failure> {
        match &self.read {
            Some(read) => read(fetcher, url, playlist),
            None => fetcher.read(url, playlist, None),
        }
    }

    fn fetch(
        &self,
        fetcher: &Fetcher,
        url: &str,
        destination: &Path,
        byte_range: Option<ByteRange>,
    ) -> Result<Fetched, Failure> {
        match &self.fetch {
            Some(fetch) => fetch(fetcher, url, destination, byte_range),
            None => fetcher.fetch(url, destination, byte_range),
        }
    }
}

/// The token a failed analysis reports.
pub fn token<T: std::fmt::Debug>(result: Result<T, Failure>) -> String {
    match result {
        Err(Failure::Source(error)) => error.token().to_string(),
        other => panic!("expected a SourceError, got {other:?}"),
    }
}

/// The policy of the remote tests: the fixture server's origin, `http`, loopback allowed.
pub fn remote_policy(origin: &str) -> FetchPolicy {
    FetchPolicy {
        allowed_origins: vec![origin.to_string()],
        allow_http: true,
        allow_private_network: true,
        ..FetchPolicy::default()
    }
}

/// The system, with the free disk space of `shutil.disk_usage` replaced.
pub struct FreeSpace<F: Fn(&Path) -> u64 + Send + Sync>(pub F);

impl<F: Fn(&Path) -> u64 + Send + Sync> Platform for FreeSpace<F> {
    fn disk_free(&self, path: &Path) -> io::Result<u64> {
        Ok((self.0)(path))
    }
}

/// The system, with `socket.getaddrinfo` answering `addresses` for every host.
pub struct Resolves(pub Vec<SocketAddr>);

impl Platform for Resolves {
    fn resolve(&self, _host: &str, _port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(self.0.clone())
    }
}

/// `_media_playlist(root, name, prefix, segments, fmp4=…)`: every segment with distinct bytes.
pub fn media_playlist(root: &Path, name: &str, prefix: &str, segments: usize, fmp4: bool) {
    let mut lines = vec!["#EXTM3U".to_string(), "#EXT-X-TARGETDURATION:6".to_string()];
    if fmp4 {
        let mut init = b"\x00\x00\x00\x08ftyp".to_vec();
        init.extend_from_slice(prefix.as_bytes());
        std::fs::write(root.join(format!("{prefix}_init.mp4")), init).unwrap();
        lines.push(format!("#EXT-X-MAP:URI=\"{prefix}_init.mp4\""));
    }
    let ext = if fmp4 { "m4s" } else { "ts" };
    for i in 0..segments {
        lines.push("#EXTINF:6,".into());
        lines.push(format!("{prefix}_{i}.{ext}"));
        let mut body: Vec<u8> = if fmp4 {
            b"\x00\x00\x00\x08moof".to_vec()
        } else {
            vec![0x47]
        };
        body.extend_from_slice(format!("{prefix}{i}").as_bytes());
        body.resize(188, b'x');
        std::fs::write(root.join(format!("{prefix}_{i}.{ext}")), body).unwrap();
    }
    lines.push("#EXT-X-ENDLIST".into());
    std::fs::write(root.join(format!("{name}.m3u8")), lines.join("\n") + "\n").unwrap();
}

/// `_ladder(root, variants, segments, audio=…, fmp4_variant=…)`: a master naming `variants`
/// TS video variants, one fMP4 variant and one audio rendition.
pub fn ladder(root: &Path, variants: usize, segments: usize, audio: bool, fmp4_variant: bool) {
    let mut master = vec!["#EXTM3U".to_string()];
    let audio_attribute = if audio { ",AUDIO=\"aud\"" } else { "" };
    if audio {
        master.push("#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Japanese\",LANGUAGE=\"ja\",DEFAULT=YES,URI=\"audio.m3u8\"".into());
        media_playlist(root, "audio", "a", segments, false);
    }
    for v in 0..variants {
        master.push(format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},CODECS=\"avc1.64001e\",RESOLUTION=640x360{audio_attribute}",
            100_000 * (v + 1)
        ));
        master.push(format!("video{v}.m3u8"));
        media_playlist(
            root,
            &format!("video{v}"),
            &format!("v{v}"),
            segments,
            false,
        );
    }
    if fmp4_variant {
        master.push(format!(
            "#EXT-X-STREAM-INF:BANDWIDTH=900000,CODECS=\"hvc1.1.6.L60.90\",RESOLUTION=640x360{audio_attribute}"
        ));
        master.push("videof.m3u8".into());
        media_playlist(root, "videof", "f", segments, true);
    }
    std::fs::write(root.join("master.m3u8"), master.join("\n") + "\n").unwrap();
}

/// `_ladder_probe`.
pub fn ladder_probe(path: &Path, init: Option<&Path>) -> Result<Vec<ProbeStream>, Failure> {
    let data = std::fs::read(path).unwrap();
    if data.first() == Some(&0x47) && data.get(1) == Some(&b'a') {
        return Ok(streams(&[
            serde_json::json!({"index": 0, "codec_type": "audio", "codec_name": "aac", "profile": "LC"}),
        ]));
    }
    if let Some(init) = init {
        assert!(
            std::fs::read(init)
                .unwrap()
                .starts_with(b"\x00\x00\x00\x08ftyp"),
            "the init is on disk before any probe of its playlist"
        );
        return Ok(streams(&[
            serde_json::json!({"index": 0, "codec_type": "video", "codec_name": "hevc", "width": 640, "height": 360}),
        ]));
    }
    Ok(streams(&[
        serde_json::json!({"index": 0, "codec_type": "video", "codec_name": "h264", "width": 640, "height": 360}),
    ]))
}

/// `(b'\x47' + text).ljust(188, fill)`.
pub fn ts_packet(text: &str, fill: u8) -> Vec<u8> {
    let mut data = vec![0x47_u8];
    data.extend_from_slice(text.as_bytes());
    data.resize(188, fill);
    data
}

/// `_ts_playlist(count, prefix)`.
pub fn ts_playlist(count: usize, prefix: &str) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut text = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:6\n");
    for i in 0..count {
        write!(text, "#EXTINF:6,\n{prefix}{i}.ts\n").expect("String never fails to write");
    }
    text.push_str("#EXT-X-ENDLIST\n");
    text.into_bytes()
}
