//! Small synthetic DASH presentations with deterministic manifests and media.
#![allow(dead_code)] // reason: a shared fixture; each test binary uses a different subset

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const FAKE_WIDEVINE_PSSH_BYTES: &[u8] = b"FAKE-PSSH-NOT-A-REAL-BOX-JUST-TEST-BYTES";
pub const FAKE_PLAYREADY_PSSH_BYTES: &[u8] = b"FAKE-PLAYREADY-PSSH-BOX-DISTINCT-TEST-BYTES-01";
pub const FAKE_PLAYREADY_PRO_BYTES: &[u8] = b"FAKE-PLAYREADY-PRO-DISTINCT-TEST-BYTES-VALUE-02";
const WIDEVINE_SYSTEM_ID_URI: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
const PLAYREADY_SYSTEM_ID_URI: &str = "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95";
const KID: &str = "00010203-0405-0607-0809-0a0b0c0d0e0f";

pub fn fake_widevine_pssh_b64() -> String {
    STANDARD.encode(FAKE_WIDEVINE_PSSH_BYTES)
}

pub fn fake_widevine_pssh_hex() -> String {
    hex::encode(FAKE_WIDEVINE_PSSH_BYTES)
}

/// One Representation of the synthetic asset (`RepSpec`).
#[derive(Clone, Debug)]
pub struct RepSpec {
    pub id: String,
    pub codec: String,
    pub content_type: String,
    pub mime_type: String,
    pub timescale: i64,
    pub duration_ticks: i64,
    pub start_number: i64,
    pub segment_count: usize,
    pub include_widevine_pssh: bool,
    pub include_playready_pssh: bool,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub lang: Option<String>,
}

impl RepSpec {
    pub fn new(id: &str, codec: &str, content_type: &str, mime_type: &str) -> Self {
        RepSpec {
            id: id.to_string(),
            codec: codec.to_string(),
            content_type: content_type.to_string(),
            mime_type: mime_type.to_string(),
            timescale: 24_000,
            duration_ticks: 144_000,
            start_number: 0,
            segment_count: 3,
            include_widevine_pssh: true,
            include_playready_pssh: true,
            width: None,
            height: None,
            lang: None,
        }
    }

    #[must_use]
    pub fn timing(mut self, timescale: i64, duration_ticks: i64) -> Self {
        self.timescale = timescale;
        self.duration_ticks = duration_ticks;
        self
    }

    #[must_use]
    pub fn segments(mut self, count: usize) -> Self {
        self.segment_count = count;
        self
    }

    #[must_use]
    pub fn without_widevine_pssh(mut self) -> Self {
        self.include_widevine_pssh = false;
        self
    }
}

pub fn h264_rep(id: &str) -> RepSpec {
    RepSpec::new(id, "avc1.42C00C", "video", "video/mp4")
}

pub fn h265_rep(id: &str) -> RepSpec {
    RepSpec::new(id, "hvc1.1.6.L60.90", "video", "video/mp4")
}

/// VP9 is out of scope and, like the source presentation, carries no `PlayReady`.
pub fn vp9_rep(id: &str) -> RepSpec {
    let mut rep = RepSpec::new(id, "vp9", "video", "video/webm");
    rep.include_playready_pssh = false;
    rep
}

pub fn audio_rep(id: &str) -> RepSpec {
    RepSpec::new(id, "mp4a.40.2", "audio", "audio/mp4").timing(48_000, 288_000)
}

/// `xml.sax.saxutils.quoteattr`.
fn quoteattr(value: &str) -> String {
    let escaped = value
        .replace('&', "&amp;")
        .replace('>', "&gt;")
        .replace('<', "&lt;")
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;");
    if escaped.contains('"') {
        if escaped.contains('\'') {
            format!("\"{}\"", escaped.replace('"', "&quot;"))
        } else {
            format!("'{escaped}'")
        }
    } else {
        format!("\"{escaped}\"")
    }
}

fn adaptation_set_xml(
    rep: &RepSpec,
    asset_dir_name: &str,
    pssh_b64: &str,
    pro_b64: &str,
) -> String {
    let mut protection = format!(
        "<ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"cenc\" \
         cenc:default_KID=\"{KID}\" xmlns:cenc=\"urn:mpeg:cenc:2013\"/>"
    );
    if rep.include_widevine_pssh {
        write!(
            protection,
            "<ContentProtection schemeIdUri=\"{WIDEVINE_SYSTEM_ID_URI}\">\
             <cenc:pssh xmlns:cenc=\"urn:mpeg:cenc:2013\">{}</cenc:pssh></ContentProtection>",
            fake_widevine_pssh_b64()
        )
        .unwrap();
    }
    if rep.include_playready_pssh {
        write!(
            protection,
            "<ContentProtection schemeIdUri=\"{PLAYREADY_SYSTEM_ID_URI}\">\
             <cenc:pssh xmlns:cenc=\"urn:mpeg:cenc:2013\">{pssh_b64}</cenc:pssh>\
             <mspr:pro xmlns:mspr=\"urn:microsoft:playready\">{pro_b64}</mspr:pro>\
             </ContentProtection>"
        )
        .unwrap();
    }
    let ext = if rep.mime_type == "video/webm" {
        "hdr"
    } else {
        "mp4"
    };
    let size = match (rep.width, rep.height) {
        (Some(width), Some(height)) => format!(" width=\"{width}\" height=\"{height}\""),
        _ => String::new(),
    };
    let representation = format!(
        "<Representation id={} codecs={} bandwidth=\"1\"{size}>\
         <SegmentTemplate timescale=\"{}\" startNumber=\"{}\" duration=\"{}\" \
         media=\"{asset_dir_name}/{}/seg_$Number$.m4s\" \
         initialization=\"{asset_dir_name}/{}/init.{ext}\"/></Representation>",
        quoteattr(&rep.id),
        quoteattr(&rep.codec),
        rep.timescale,
        rep.start_number,
        rep.duration_ticks,
        rep.id,
        rep.id
    );
    let lang = rep
        .lang
        .as_ref()
        .map_or(String::new(), |lang| format!(" lang=\"{lang}\""));
    format!(
        "<AdaptationSet mimeType=\"{}\" contentType=\"{}\"{lang}>{protection}{representation}\
         </AdaptationSet>",
        rep.mime_type, rep.content_type
    )
}

/// What `build_synthetic_dash_asset` wrote.
pub struct SyntheticAsset {
    pub mpd_path: PathBuf,
    pub title_id: String,
    pub reps: Vec<RepSpec>,
}

/// `build_synthetic_dash_asset(root, reps, title_id=...)` with the Python defaults for the
/// hosts, the duration and the placeholder `PlayReady` blobs: the manifest under
/// `download/manifest.example.test/<title>/dash/`, and per rep an init file and its segments
/// under `download/cdn.example.test/<title>/<content_type>/<id>/`.
pub fn build_synthetic_dash_asset(root: &Path, reps: &[RepSpec], title_id: &str) -> SyntheticAsset {
    let download_root = root.join("download");
    let base_url = format!("https://cdn.example.test/{title_id}/");
    let pssh_b64 = STANDARD.encode(FAKE_PLAYREADY_PSSH_BYTES);
    let pro_b64 = STANDARD.encode(FAKE_PLAYREADY_PRO_BYTES);
    let body: String = reps
        .iter()
        .map(|rep| adaptation_set_xml(rep, &rep.content_type, &pssh_b64, &pro_b64))
        .collect();
    let mpd_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" \
         mediaPresentationDuration=\"PT18.0S\" minBufferTime=\"PT2S\" \
         profiles=\"urn:mpeg:dash:profile:isoff-live:2011\">\
         <BaseURL>{base_url}</BaseURL><Period id=\"p0\">{body}</Period></MPD>\n"
    );
    let mpd_path = download_root
        .join("manifest.example.test")
        .join(title_id)
        .join("dash/manifest.mpd");
    std::fs::create_dir_all(mpd_path.parent().unwrap()).unwrap();
    std::fs::write(&mpd_path, mpd_xml).unwrap();
    for rep in reps {
        let ext = if rep.mime_type == "video/webm" {
            "hdr"
        } else {
            "mp4"
        };
        let rep_dir = download_root
            .join("cdn.example.test")
            .join(title_id)
            .join(&rep.content_type)
            .join(&rep.id);
        std::fs::create_dir_all(&rep_dir).unwrap();
        std::fs::write(
            rep_dir.join(format!("init.{ext}")),
            format!("INIT:{}", rep.id),
        )
        .unwrap();
        for index in 0..rep.segment_count {
            std::fs::write(
                rep_dir.join(format!("seg_{index}.m4s")),
                format!("SEG:{}:{index}", rep.id),
            )
            .unwrap();
        }
    }
    SyntheticAsset {
        mpd_path,
        title_id: title_id.to_string(),
        reps: reps.to_vec(),
    }
}
