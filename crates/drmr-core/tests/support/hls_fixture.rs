//! Synthetic HLS presentations, audio-group mappings, and frozen source records.

use std::path::{Path, PathBuf};

/// Synthetic IV (16 bytes): never a real one.
pub const FAKE_IV_HEX: &str = "20202020202020202020202020202020";
/// Synthetic `FairPlay` key URI.
pub const FAKE_SKD_URI: &str = "skd://sample-content-id";
/// A synthetic signed token for the `?vt=` query of the master's references.
pub const FAKE_VT_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.TEST-ONLY-NOT-A-REAL-TOKEN.sig0";

const MASTER_HOST: &str = "manifest.test";
const VARIANTS_HOST: &str = "variants.test";
const VOD_HOST: &str = "vod.test";

/// The DASH-side rendition a mapping resolves to.
// The fields of `config::Rendition` the fixture needs; a plain struct keeps the fixture free of
// the key material a `config::Rendition` carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureRendition {
    pub rep_id: String,
    pub content_type: String,
    pub codec: String,
    pub timescale: u64,
    pub segment_duration_ticks: u64,
    pub start_number: u64,
    pub init_path: PathBuf,
    pub segment_paths: Vec<PathBuf>,
    pub rel_dir: PathBuf,
    pub widevine_pssh_hex: Option<String>,
}

/// One `#EXT-X-STREAM-INF` video variant with its media playlist and segments.
#[derive(Clone, Debug)]
pub struct VariantSpec {
    pub codec: &'static str,
    pub bandwidth: u64,
    pub audio_group: &'static str,
    pub uuid: &'static str,
    pub seg_size: usize,
    pub name_bitrate: u64,
    /// `ts` (H.264) or `m4s` (H.265).
    pub container: &'static str,
    pub resolution: &'static str,
    pub frame_rate: &'static str,
    pub seg_count: usize,
}

impl VariantSpec {
    /// A variant with the Python defaults (`ts`, 320x180, 24.000, three segments).
    pub fn new(
        codec: &'static str,
        bandwidth: u64,
        audio_group: &'static str,
        uuid: &'static str,
        seg_size: usize,
        name_bitrate: u64,
        container: &'static str,
    ) -> Self {
        VariantSpec {
            codec,
            bandwidth,
            audio_group,
            uuid,
            seg_size,
            name_bitrate,
            container,
            resolution: "320x180",
            frame_rate: "24.000",
            seg_count: 3,
        }
    }
}

/// One `#EXT-X-MEDIA:TYPE=AUDIO` group with its media playlist and segments.
#[derive(Clone, Debug)]
pub struct AudioSpec {
    pub group_id: &'static str,
    pub uuid: &'static str,
    pub seg_size: usize,
    pub container: &'static str,
    pub name: &'static str,
    pub seg_count: usize,
}

impl AudioSpec {
    /// A group with the Python defaults (three segments).
    pub fn new(
        group_id: &'static str,
        uuid: &'static str,
        seg_size: usize,
        container: &'static str,
    ) -> Self {
        AudioSpec {
            group_id,
            uuid,
            seg_size,
            container,
            name: "pro_test_audio",
            seg_count: 3,
        }
    }
}

/// A written asset.
#[derive(Clone, Debug)]
pub struct SyntheticHlsAsset {
    pub master_path: PathBuf,
    pub download_root: PathBuf,
    pub renditions: Vec<FixtureRendition>,
    pub iv_hex: String,
    pub title: String,
    pub hls_sub: String,
    pub dash_sub: String,
    pub variant_specs: Vec<VariantSpec>,
    pub audio_specs: Vec<AudioSpec>,
}

impl SyntheticHlsAsset {
    /// The media playlist of `<ct>/<uuid>`.
    pub fn media_playlist_path(&self, uuid: &str, ct: &str) -> PathBuf {
        let mut matches: Vec<PathBuf> = files_under(&self.download_root.join(VARIANTS_HOST))
            .into_iter()
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "m3u8")
                    && path
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|name| name == uuid)
                    && path
                        .parent()
                        .and_then(Path::parent)
                        .and_then(Path::file_name)
                        .is_some_and(|name| name == ct)
            })
            .collect();
        matches.sort();
        matches
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no media playlist for {ct}/{uuid}"))
    }

    /// The DASH rendition `rep_id`.
    pub fn rendition_by_id(&self, rep_id: &str) -> &FixtureRendition {
        self.renditions
            .iter()
            .find(|rendition| rendition.rep_id == rep_id)
            .unwrap_or_else(|| panic!("{rep_id}"))
    }

    /// The on-disk total size of the segments a media playlist references, its
    /// initialization segment excluded (the mapping's size signal, `manifest_hls`).
    pub fn on_disk_total_size(&self, playlist: &Path) -> u64 {
        let text = std::fs::read_to_string(playlist).unwrap();
        text.lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|url| {
                let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
                let (host, path) = rest.split_once('/').unwrap();
                let path = path.split('?').next().unwrap();
                std::fs::metadata(self.download_root.join(host).join(path))
                    .unwrap()
                    .len()
            })
            .sum()
    }
}

/// Every file under `root`, recursively.
pub fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Indices of `values` in ascending order; a tie cannot be ranked (`_strict_order`).
pub fn size_rank(values: &[u64]) -> Result<Vec<usize>, String> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by_key(|index| values[*index]);
    for pair in order.windows(2) {
        if values[pair[0]] == values[pair[1]] {
            return Err(format!("a tie at value {}", values[pair[0]]));
        }
    }
    Ok(order)
}

/// `#EXTINF:6,` and a segment URL per segment; audio uses the BBB fractional-drift durations
/// for the first few, so the EXTINF list is not a constant.
fn extinf_lines(seg_count: usize, container: &str, ct: &str, seg_url_prefix: &str) -> Vec<String> {
    let ext = if container == "ts" { "ts" } else { "m4s" };
    let audio_durations = ["5.994666666666666", "5.994666666666666", "6.016"];
    let mut lines = Vec::new();
    for i in 0..seg_count {
        let duration = if ct == "audio" {
            audio_durations
                .get(i)
                .copied()
                .unwrap_or("5.994666666666666")
        } else {
            "6"
        };
        lines.push(format!("#EXTINF:{duration},"));
        lines.push(format!("{seg_url_prefix}/seg_{i}.{ext}"));
    }
    lines
}

fn write_sized_segments(
    seg_dir: &Path,
    seg_count: usize,
    seg_size: usize,
    container: &str,
    with_init: bool,
) {
    std::fs::create_dir_all(seg_dir).unwrap();
    let ext = if container == "ts" { "ts" } else { "m4s" };
    if with_init {
        std::fs::write(seg_dir.join("init.mp4"), vec![0_u8; 32]).unwrap();
    }
    for i in 0..seg_count {
        std::fs::write(seg_dir.join(format!("seg_{i}.{ext}")), vec![0_u8; seg_size]).unwrap();
    }
}

fn stream_inf_line(v: &VariantSpec) -> String {
    format!(
        "#EXT-X-STREAM-INF:BANDWIDTH={},CODECS=\"{},mp4a.40.2\",RESOLUTION={},AUDIO=\"{}\",CLOSED-CAPTIONS=NONE,FRAME-RATE={}",
        v.bandwidth, v.codec, v.resolution, v.audio_group, v.frame_rate
    )
}

fn media_line(a: &AudioSpec, uri: &str) -> String {
    format!(
        "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"{}\",NAME=\"{}\",DEFAULT=YES,AUTOSELECT=YES,LANGUAGE=\"und\",URI=\"{uri}\"",
        a.group_id, a.name
    )
}

/// The header block of a media playlist through its `EXT-X-KEY` line; `init_url` set means
/// fMP4 (an `EXT-X-MAP` is written).
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
        "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{FAKE_SKD_URI}\",IV=0x{},KEYFORMAT=\"com.apple.streamingkeydelivery\"",
        iv_hex.to_uppercase()
    ));
    lines.join("\n")
}

/// `build_synthetic_hls_asset`'s keyword arguments with their defaults.
#[derive(Clone, Debug)]
pub struct AssetOptions {
    pub title: String,
    pub hls_sub: String,
    pub dash_sub: String,
    pub iv_hex: String,
    pub include_independent_segments: bool,
    pub extra_master_lines: Vec<String>,
}

impl Default for AssetOptions {
    fn default() -> Self {
        AssetOptions {
            title: "title-hls".into(),
            hls_sub: "hlssub0000".into(),
            dash_sub: "dashsub0000".into(),
            iv_hex: FAKE_IV_HEX.into(),
            include_independent_segments: true,
            extra_master_lines: Vec::new(),
        }
    }
}

fn write_media_playlist(
    download_root: &Path,
    variants_base: &str,
    ct: &str,
    uuid: &str,
    name: &str,
    header: &str,
    body: &[String],
) {
    let mut lines = vec![header.to_string()];
    lines.extend(body.iter().cloned());
    lines.push("#EXT-X-ENDLIST".into());
    lines.push(String::new());
    let path = download_root
        .join(VARIANTS_HOST)
        .join(variants_base)
        .join(ct)
        .join(uuid)
        .join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, lines.join("\n")).unwrap();
}

/// Write a full synthetic asset (master, media playlists, sized segments).
pub fn build_synthetic_hls_asset(
    root: &Path,
    variants: &[VariantSpec],
    audios: &[AudioSpec],
    renditions: Vec<FixtureRendition>,
    options: &AssetOptions,
) -> SyntheticHlsAsset {
    let download_root = root.join("download");
    let (title, hls_sub) = (&options.title, &options.hls_sub);
    let variants_base = format!("v5/demo-qa/{title}/{hls_sub}");
    let mut master_lines = vec!["#EXTM3U".to_string(), "#EXT-X-VERSION:4".to_string()];
    if options.include_independent_segments {
        master_lines.push("#EXT-X-INDEPENDENT-SEGMENTS".into());
    }
    for a in audios {
        let family = if a.container == "ts" { "ts" } else { "fmp4" };
        let name = format!("{family}_audio_1782279276.m3u8");
        let playlist_url = format!(
            "https://{VARIANTS_HOST}/{variants_base}/audio/{}/{name}?vt={FAKE_VT_TOKEN}",
            a.uuid
        );
        master_lines.push(media_line(a, &playlist_url));
        let seg_url_prefix = format!("https://{VOD_HOST}/{title}/{hls_sub}/audio/{}", a.uuid);
        let seg_dir = download_root
            .join(VOD_HOST)
            .join(title)
            .join(hls_sub)
            .join("audio")
            .join(a.uuid);
        write_sized_segments(
            &seg_dir,
            a.seg_count,
            a.seg_size,
            a.container,
            a.container != "ts",
        );
        let init_url = (a.container != "ts").then(|| format!("{seg_url_prefix}/init.mp4"));
        let header = media_playlist_header(&options.iv_hex, init_url.as_deref());
        let body = extinf_lines(a.seg_count, a.container, "audio", &seg_url_prefix);
        write_media_playlist(
            &download_root,
            &variants_base,
            "audio",
            a.uuid,
            &name,
            &header,
            &body,
        );
    }
    for v in variants {
        let name = format!("video_{}_1782279276.m3u8", v.name_bitrate);
        let playlist_url = format!(
            "https://{VARIANTS_HOST}/{variants_base}/video/{}/{name}?vt={FAKE_VT_TOKEN}",
            v.uuid
        );
        master_lines.push(stream_inf_line(v));
        master_lines.push(playlist_url);
        let seg_url_prefix = format!("https://{VOD_HOST}/{title}/{hls_sub}/video/{}", v.uuid);
        let seg_dir = download_root
            .join(VOD_HOST)
            .join(title)
            .join(hls_sub)
            .join("video")
            .join(v.uuid);
        write_sized_segments(
            &seg_dir,
            v.seg_count,
            v.seg_size,
            v.container,
            v.container != "ts",
        );
        let init_url = (v.container != "ts").then(|| format!("{seg_url_prefix}/init.mp4"));
        let header = media_playlist_header(&options.iv_hex, init_url.as_deref());
        let body = extinf_lines(v.seg_count, v.container, "video", &seg_url_prefix);
        write_media_playlist(
            &download_root,
            &variants_base,
            "video",
            v.uuid,
            &name,
            &header,
            &body,
        );
    }
    master_lines.extend(options.extra_master_lines.iter().cloned());
    master_lines.push(String::new());
    let master_path = download_root
        .join(MASTER_HOST)
        .join(format!("v4/demo-qa/{title}/{hls_sub}/hls/v6/manifest.m3u8"));
    std::fs::create_dir_all(master_path.parent().unwrap()).unwrap();
    std::fs::write(&master_path, master_lines.join("\n")).unwrap();
    SyntheticHlsAsset {
        master_path,
        download_root,
        renditions,
        iv_hex: options.iv_hex.clone(),
        title: title.clone(),
        hls_sub: hls_sub.clone(),
        dash_sub: options.dash_sub.clone(),
        variant_specs: variants.to_vec(),
        audio_specs: audios.to_vec(),
    }
}

/// DASH-side sized segments (containing CBCS output) and the rendition pointing at
/// them (`make_dash_rendition` with three segments, `start_number` 0).
pub fn make_dash_rendition(
    root: &Path,
    rep_id: &str,
    codec: &str,
    content_type: &str,
    uuid: &str,
    seg_size: usize,
    timing: (u64, u64),
) -> FixtureRendition {
    let seg_count = 3;
    let seg_dir = root
        .join("download")
        .join(VOD_HOST)
        .join("title-hls")
        .join("dashsub0000")
        .join(content_type)
        .join(uuid);
    write_sized_segments(&seg_dir, seg_count, seg_size, "m4s", true);
    let mut segment_paths: Vec<PathBuf> = (0..seg_count)
        .map(|i| seg_dir.join(format!("seg_{i}.m4s")))
        .collect();
    segment_paths.sort();
    FixtureRendition {
        rep_id: rep_id.into(),
        content_type: content_type.into(),
        codec: codec.into(),
        timescale: timing.0,
        segment_duration_ticks: timing.1,
        start_number: 0,
        init_path: seg_dir.join("init.mp4"),
        segment_paths,
        rel_dir: Path::new(content_type).join(uuid),
        widevine_pssh_hex: None,
    }
}

/// The BBB-like asset: 3 H.264 (TS) + 3 H.265 (fMP4) video variants, two `ts_AUDIO` and two
/// `fmp4_AUDIO` groups, and the 8 matching DASH renditions; group `-0` is the larger (256k)
/// audio group, `-1` the smaller (64k).
pub fn build_bbb_like_asset(root: &Path) -> SyntheticHlsAsset {
    // DASH renditions (targets). Audio: -64k is small, -256k large.
    let dash: [(&str, &str, &str, &str, usize); 8] = [
        ("d-h264-lo", "avc1.42C00C", "video", "dv-h264-lo", 1000),
        ("d-h264-mid", "avc1.42C00C", "video", "dv-h264-mid", 2000),
        ("d-h264-hi", "avc1.42C00D", "video", "dv-h264-hi", 3500),
        ("d-h265-lo", "hvc1.1.6.L60.90", "video", "dv-h265-lo", 900),
        (
            "d-h265-mid",
            "hvc1.1.6.L60.90",
            "video",
            "dv-h265-mid",
            1800,
        ),
        ("d-h265-hi", "hvc1.1.6.L60.90", "video", "dv-h265-hi", 3100),
        ("d-aud-64k", "mp4a.40.2", "audio", "da-64k", 500),
        ("d-aud-256k", "mp4a.40.2", "audio", "da-256k", 1900),
    ];
    let renditions = dash
        .iter()
        .map(|(rep_id, codec, content_type, uuid, seg_size)| {
            let timing = if *content_type == "audio" {
                (48_000, 288_000)
            } else {
                (24_000, 144_000)
            };
            make_dash_rendition(root, rep_id, codec, content_type, uuid, *seg_size, timing)
        })
        .collect();
    // HLS video variants: name_bitrate ~ video only, bandwidth ~ muxed (with the audio group).
    let variants: Vec<VariantSpec> = [
        (
            "avc1.42C00C",
            655_005,
            "ts_AUDIO-0",
            "hv-h264-a",
            1990,
            266_780,
            "ts",
        ),
        (
            "avc1.42C00D",
            987_891,
            "ts_AUDIO-0",
            "hv-h264-b",
            3540,
            475_322,
            "ts",
        ),
        (
            "avc1.42C00C",
            313_952,
            "ts_AUDIO-1",
            "hv-h264-c",
            1148,
            154_016,
            "ts",
        ),
        (
            "hvc1.1.6.L60.90",
            580_417,
            "fmp4_AUDIO-0",
            "hv-h265-a",
            1733,
            232_473,
            "m4s",
        ),
        (
            "hvc1.1.6.L60.90",
            839_094,
            "fmp4_AUDIO-0",
            "hv-h265-b",
            3071,
            412_000,
            "m4s",
        ),
        (
            "hvc1.1.6.L60.90",
            242_463,
            "fmp4_AUDIO-1",
            "hv-h265-c",
            975,
            130_833,
            "m4s",
        ),
    ]
    .into_iter()
    .map(
        |(codec, bandwidth, group, uuid, size, name_bitrate, container)| {
            VariantSpec::new(codec, bandwidth, group, uuid, size, name_bitrate, container)
        },
    )
    .collect();
    // Audio groups. BBB inversion: group -0 is the larger (256k) group, -1 the smaller (64k).
    let audios = vec![
        AudioSpec::new("ts_AUDIO-0", "ha-ts-0", 2189, "ts"),
        AudioSpec::new("ts_AUDIO-1", "ha-ts-1", 949, "ts"),
        AudioSpec::new("fmp4_AUDIO-0", "ha-fmp4-0", 1902, "m4s"),
        AudioSpec::new("fmp4_AUDIO-1", "ha-fmp4-1", 470, "m4s"),
    ];
    build_synthetic_hls_asset(
        root,
        &variants,
        &audios,
        renditions,
        &AssetOptions::default(),
    )
}
