//! Synthetic HLS masters, media playlists, and sized segments with matching DASH renditions.
//! Signaling values are synthetic and deterministic.

use super::common::{FAIRPLAY_URI, IV_HEX};
use drmr_core::config::Rendition;
use std::path::{Path, PathBuf};

const MASTER_HOST: &str = "manifest.test";
const VARIANTS_HOST: &str = "variants.test";
const VOD_HOST: &str = "vod.test";
/// A fake signed token (never a captured value).
pub const FAKE_VT_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.TEST-ONLY-NOT-A-REAL-TOKEN.sig0";

/// `VariantSpec`.
#[derive(Clone, Debug)]
pub struct VariantSpec {
    pub codec: String,
    pub bandwidth: u64,
    pub audio_group: String,
    pub uuid: String,
    pub seg_size: usize,
    pub name_bitrate: u64,
    pub container: String,
    pub resolution: String,
    pub frame_rate: String,
    pub seg_count: usize,
}

/// `VariantSpec(codec, bandwidth, audio_group, uuid, seg_size, name_bitrate, container)`.
pub fn variant(
    codec: &str,
    bandwidth: u64,
    audio_group: &str,
    uuid: &str,
    seg_size: usize,
    name_bitrate: u64,
    container: &str,
) -> VariantSpec {
    VariantSpec {
        codec: codec.to_string(),
        bandwidth,
        audio_group: audio_group.to_string(),
        uuid: uuid.to_string(),
        seg_size,
        name_bitrate,
        container: container.to_string(),
        resolution: "320x180".to_string(),
        frame_rate: "24.000".to_string(),
        seg_count: 3,
    }
}

/// `AudioSpec`.
#[derive(Clone, Debug)]
pub struct AudioSpec {
    pub group_id: String,
    pub uuid: String,
    pub seg_size: usize,
    pub container: String,
    pub name: String,
    pub seg_count: usize,
}

/// `AudioSpec(group_id, uuid, seg_size, container)`.
pub fn audio(group_id: &str, uuid: &str, seg_size: usize, container: &str) -> AudioSpec {
    AudioSpec {
        group_id: group_id.to_string(),
        uuid: uuid.to_string(),
        seg_size,
        container: container.to_string(),
        name: "pro_test_audio".to_string(),
        seg_count: 3,
    }
}

fn extinf_lines(seg_count: usize, container: &str, ct: &str, prefix: &str) -> Vec<String> {
    let ext = if container == "ts" { "ts" } else { "m4s" };
    let audio_durations = ["5.994666666666666", "5.994666666666666", "6.016"];
    let mut lines = Vec::new();
    for index in 0..seg_count {
        let duration = if ct == "audio" {
            audio_durations
                .get(index)
                .copied()
                .unwrap_or("5.994666666666666")
        } else {
            "6"
        };
        lines.push(format!("#EXTINF:{duration},"));
        lines.push(format!("{prefix}/seg_{index}.{ext}"));
    }
    lines
}

fn write_sized_segments(dir: &Path, count: usize, size: usize, container: &str, with_init: bool) {
    std::fs::create_dir_all(dir).unwrap();
    let ext = if container == "ts" { "ts" } else { "m4s" };
    if with_init {
        std::fs::write(dir.join("init.mp4"), [0u8; 32]).unwrap();
    }
    for index in 0..count {
        std::fs::write(dir.join(format!("seg_{index}.{ext}")), vec![0u8; size]).unwrap();
    }
}

fn media_playlist_header(iv_hex: &str, init_url: Option<&str>) -> String {
    let mut lines = vec![
        "#EXTM3U".to_string(),
        "#EXT-X-VERSION:7".to_string(),
        "#EXT-X-PLAYLIST-TYPE:VOD".to_string(),
        "#EXT-X-TARGETDURATION:6".to_string(),
        "#EXT-X-MEDIA-SEQUENCE:0".to_string(),
    ];
    if let Some(init_url) = init_url {
        lines.push(format!("#EXT-X-MAP:URI=\"{init_url}\""));
    }
    lines.push(format!(
        "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{FAIRPLAY_URI}\",IV=0x{},KEYFORMAT=\"com.apple.streamingkeydelivery\"",
        iv_hex.to_uppercase()
    ));
    lines.join("\n")
}

/// `SyntheticHlsAsset`.
pub struct SyntheticHlsAsset {
    pub master_path: PathBuf,
    pub download_root: PathBuf,
    pub renditions: Vec<Rendition>,
    pub title: String,
    pub hls_sub: String,
}

impl SyntheticHlsAsset {
    /// `media_playlist_path(uuid, ct)`.
    pub fn media_playlist_path(&self, uuid: &str, ct: &str) -> PathBuf {
        let dir = self
            .download_root
            .join(VARIANTS_HOST)
            .join(format!("v5/demo-qa/{}/{}", self.title, self.hls_sub))
            .join(ct)
            .join(uuid);
        let mut matches: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "m3u8"))
            .collect();
        matches.sort();
        matches.remove(0)
    }

    /// `rendition_by_id`.
    pub fn rendition(&self, rep_id: &str) -> &Rendition {
        self.renditions.iter().find(|r| r.rep_id == rep_id).unwrap()
    }
}

/// `build_synthetic_hls_asset` keyword arguments beyond the specs.
pub struct HlsOptions {
    pub include_independent_segments: bool,
    pub extra_master_lines: Vec<String>,
}

impl Default for HlsOptions {
    fn default() -> Self {
        HlsOptions {
            include_independent_segments: true,
            extra_master_lines: Vec::new(),
        }
    }
}

fn write_text(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `build_synthetic_hls_asset(root, variants=…, audios=…, renditions=…)`.
pub fn build_synthetic_hls_asset(
    root: &Path,
    variants: &[VariantSpec],
    audios: &[AudioSpec],
    renditions: Vec<Rendition>,
    options: &HlsOptions,
) -> SyntheticHlsAsset {
    let (title, hls_sub) = ("title-hls", "hlssub0000");
    let download_root = root.join("download");
    let variants_base = format!("v5/demo-qa/{title}/{hls_sub}");
    let mut master = vec!["#EXTM3U".to_string(), "#EXT-X-VERSION:4".to_string()];
    if options.include_independent_segments {
        master.push("#EXT-X-INDEPENDENT-SEGMENTS".to_string());
    }
    for a in audios {
        let kind = if a.container == "ts" { "ts" } else { "fmp4" };
        let name = format!("{kind}_audio_1782279276.m3u8");
        let url = format!(
            "https://{VARIANTS_HOST}/{variants_base}/audio/{}/{name}?vt={FAKE_VT_TOKEN}",
            a.uuid
        );
        master.push(format!(
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"{}\",NAME=\"{}\",DEFAULT=YES,AUTOSELECT=YES,LANGUAGE=\"und\",URI=\"{url}\"",
            a.group_id, a.name
        ));
        let prefix = format!("https://{VOD_HOST}/{title}/{hls_sub}/audio/{}", a.uuid);
        let seg_dir = download_root
            .join(VOD_HOST)
            .join(title)
            .join(hls_sub)
            .join("audio")
            .join(&a.uuid);
        write_sized_segments(
            &seg_dir,
            a.seg_count,
            a.seg_size,
            &a.container,
            a.container != "ts",
        );
        let init = (a.container != "ts").then(|| format!("{prefix}/init.mp4"));
        let mut lines = vec![media_playlist_header(IV_HEX, init.as_deref())];
        lines.extend(extinf_lines(a.seg_count, &a.container, "audio", &prefix));
        lines.push("#EXT-X-ENDLIST".to_string());
        lines.push(String::new());
        let path = download_root
            .join(VARIANTS_HOST)
            .join(&variants_base)
            .join("audio")
            .join(&a.uuid)
            .join(&name);
        write_text(&path, &lines.join("\n"));
    }
    for v in variants {
        let name = format!("video_{}_1782279276.m3u8", v.name_bitrate);
        let url = format!(
            "https://{VARIANTS_HOST}/{variants_base}/video/{}/{name}?vt={FAKE_VT_TOKEN}",
            v.uuid
        );
        master.push(format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},CODECS=\"{},mp4a.40.2\",RESOLUTION={},AUDIO=\"{}\",CLOSED-CAPTIONS=NONE,FRAME-RATE={}",
            v.bandwidth, v.codec, v.resolution, v.audio_group, v.frame_rate
        ));
        master.push(url);
        let prefix = format!("https://{VOD_HOST}/{title}/{hls_sub}/video/{}", v.uuid);
        let seg_dir = download_root
            .join(VOD_HOST)
            .join(title)
            .join(hls_sub)
            .join("video")
            .join(&v.uuid);
        write_sized_segments(
            &seg_dir,
            v.seg_count,
            v.seg_size,
            &v.container,
            v.container != "ts",
        );
        let init = (v.container != "ts").then(|| format!("{prefix}/init.mp4"));
        let mut lines = vec![media_playlist_header(IV_HEX, init.as_deref())];
        lines.extend(extinf_lines(v.seg_count, &v.container, "video", &prefix));
        lines.push("#EXT-X-ENDLIST".to_string());
        lines.push(String::new());
        let path = download_root
            .join(VARIANTS_HOST)
            .join(&variants_base)
            .join("video")
            .join(&v.uuid)
            .join(&name);
        write_text(&path, &lines.join("\n"));
    }
    master.extend(options.extra_master_lines.iter().cloned());
    master.push(String::new());
    let master_path = download_root
        .join(MASTER_HOST)
        .join(format!("v4/demo-qa/{title}/{hls_sub}/hls/v6/manifest.m3u8"));
    write_text(&master_path, &master.join("\n"));
    SyntheticHlsAsset {
        master_path,
        download_root,
        renditions,
        title: title.to_string(),
        hls_sub: hls_sub.to_string(),
    }
}

/// `make_dash_rendition(root, rep_id=…, codec=…, content_type=…, uuid=…, seg_size=…)` with
/// the optional segment count and timing.
pub struct DashRenditionSpec<'a> {
    pub rep_id: &'a str,
    pub codec: &'a str,
    pub content_type: &'a str,
    pub uuid: &'a str,
    pub seg_size: usize,
    pub seg_count: usize,
    pub timescale: i64,
    pub segment_duration_ticks: i64,
}

impl<'a> DashRenditionSpec<'a> {
    pub fn new(
        rep_id: &'a str,
        codec: &'a str,
        content_type: &'a str,
        uuid: &'a str,
        seg_size: usize,
    ) -> Self {
        DashRenditionSpec {
            rep_id,
            codec,
            content_type,
            uuid,
            seg_size,
            seg_count: 3,
            timescale: 24000,
            segment_duration_ticks: 144_000,
        }
    }
}

/// `make_dash_rendition`: DASH-side sized segments and the `Rendition` pointing at them.
pub fn make_dash_rendition(root: &Path, spec: &DashRenditionSpec<'_>) -> Rendition {
    let seg_dir = root
        .join("download")
        .join(VOD_HOST)
        .join("title-hls/dashsub0000")
        .join(spec.content_type)
        .join(spec.uuid);
    write_sized_segments(&seg_dir, spec.seg_count, spec.seg_size, "m4s", true);
    let mut segment_paths: Vec<PathBuf> = (0..spec.seg_count)
        .map(|index| seg_dir.join(format!("seg_{index}.m4s")))
        .collect();
    segment_paths.sort();
    Rendition {
        rep_id: spec.rep_id.to_string(),
        content_type: spec.content_type.to_string(),
        codec: spec.codec.to_string(),
        timescale: spec.timescale,
        segment_duration_ticks: spec.segment_duration_ticks,
        start_number: 0,
        init_path: seg_dir.join("init.mp4"),
        segment_paths,
        rel_dir: Path::new(spec.content_type).join(spec.uuid),
        widevine_pssh_hex: None,
        width: None,
        height: None,
        frame_rate: None,
        lang: None,
    }
}

/// `build_bbb_like_asset`: 3 H.264 (TS) + 3 H.265 (fMP4) variants, 2 `ts_AUDIO` + 2
/// `fmp4_AUDIO` groups (the BBB inversion: `fmp4_AUDIO-0` is the larger group) and the eight
/// DASH renditions.
pub fn build_bbb_like_asset(root: &Path) -> SyntheticHlsAsset {
    let audio_spec = |rep_id, uuid, seg_size| DashRenditionSpec {
        timescale: 48000,
        segment_duration_ticks: 288_000,
        ..DashRenditionSpec::new(rep_id, "mp4a.40.2", "audio", uuid, seg_size)
    };
    let specs = [
        DashRenditionSpec::new("d-h264-lo", "avc1.42C00C", "video", "dv-h264-lo", 1000),
        DashRenditionSpec::new("d-h264-mid", "avc1.42C00C", "video", "dv-h264-mid", 2000),
        DashRenditionSpec::new("d-h264-hi", "avc1.42C00D", "video", "dv-h264-hi", 3500),
        DashRenditionSpec::new("d-h265-lo", "hvc1.1.6.L60.90", "video", "dv-h265-lo", 900),
        DashRenditionSpec::new(
            "d-h265-mid",
            "hvc1.1.6.L60.90",
            "video",
            "dv-h265-mid",
            1800,
        ),
        DashRenditionSpec::new("d-h265-hi", "hvc1.1.6.L60.90", "video", "dv-h265-hi", 3100),
        audio_spec("d-aud-64k", "da-64k", 500),
        audio_spec("d-aud-256k", "da-256k", 1900),
    ];
    let renditions = specs
        .iter()
        .map(|spec| make_dash_rendition(root, spec))
        .collect();
    let variants = [
        variant(
            "avc1.42C00C",
            655_005,
            "ts_AUDIO-0",
            "hv-h264-a",
            1990,
            266_780,
            "ts",
        ),
        variant(
            "avc1.42C00D",
            987_891,
            "ts_AUDIO-0",
            "hv-h264-b",
            3540,
            475_322,
            "ts",
        ),
        variant(
            "avc1.42C00C",
            313_952,
            "ts_AUDIO-1",
            "hv-h264-c",
            1148,
            154_016,
            "ts",
        ),
        variant(
            "hvc1.1.6.L60.90",
            580_417,
            "fmp4_AUDIO-0",
            "hv-h265-a",
            1733,
            232_473,
            "m4s",
        ),
        variant(
            "hvc1.1.6.L60.90",
            839_094,
            "fmp4_AUDIO-0",
            "hv-h265-b",
            3071,
            412_000,
            "m4s",
        ),
        variant(
            "hvc1.1.6.L60.90",
            242_463,
            "fmp4_AUDIO-1",
            "hv-h265-c",
            975,
            130_833,
            "m4s",
        ),
    ];
    let audios = [
        audio("ts_AUDIO-0", "ha-ts-0", 2189, "ts"),
        audio("ts_AUDIO-1", "ha-ts-1", 949, "ts"),
        audio("fmp4_AUDIO-0", "ha-fmp4-0", 1902, "m4s"),
        audio("fmp4_AUDIO-1", "ha-fmp4-1", 470, "m4s"),
    ];
    build_synthetic_hls_asset(root, &variants, &audios, renditions, &HlsOptions::default())
}
