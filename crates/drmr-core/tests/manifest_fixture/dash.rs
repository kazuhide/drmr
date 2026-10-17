//! Small synthetic DASH assets with deterministic manifests and download layouts.

use super::common::{KID_UUID, fixture, text};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const WIDEVINE_SYSTEM_ID_URI: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
const PLAYREADY_SYSTEM_ID_URI: &str = "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95";

/// `RepSpec`.
#[derive(Clone, Debug)]
pub struct RepSpec {
    pub id: String,
    pub codec: String,
    pub content_type: String,
    pub mime_type: String,
    pub timescale: u32,
    pub duration_ticks: u32,
    pub start_number: u32,
    pub segment_count: u32,
    pub include_widevine_pssh: bool,
    pub include_playready_pssh: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub lang: Option<String>,
}

impl RepSpec {
    pub fn new(id: &str, codec: &str, content_type: &str, mime_type: &str) -> Self {
        RepSpec {
            id: id.to_string(),
            codec: codec.to_string(),
            content_type: content_type.to_string(),
            mime_type: mime_type.to_string(),
            timescale: 24000,
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
}

pub fn h264_rep(id: &str) -> RepSpec {
    RepSpec::new(id, "avc1.42C00C", "video", "video/mp4")
}

pub fn h265_rep(id: &str) -> RepSpec {
    RepSpec::new(id, "hvc1.1.6.L60.90", "video", "video/mp4")
}

pub fn vp9_rep(id: &str) -> RepSpec {
    let mut rep = RepSpec::new(id, "vp9", "video", "video/webm");
    rep.include_playready_pssh = false;
    rep
}

pub fn audio_rep(id: &str) -> RepSpec {
    let mut rep = RepSpec::new(id, "mp4a.40.2", "audio", "audio/mp4");
    rep.timescale = 48000;
    rep.duration_ticks = 288_000;
    rep
}

/// The fake `PlayReady` / Widevine blobs of `dash_fixture`.
pub struct Blobs {
    pub widevine: String,
    pub playready_pssh: String,
    pub playready_pro: String,
}

pub fn fake_blobs() -> Blobs {
    let cases = fixture("manifest_dash.json");
    Blobs {
        widevine: text(&cases["fake_widevine_b64"]).to_string(),
        playready_pssh: text(&cases["fake_pssh_b64"]).to_string(),
        playready_pro: text(&cases["fake_pro_b64"]).to_string(),
    }
}

/// `xml.sax.saxutils.quoteattr` for the simple ids and codecs the tests use.
fn quoteattr(value: &str) -> String {
    let escaped = value
        .replace('&', "&amp;")
        .replace('>', "&gt;")
        .replace('<', "&lt;");
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

fn adaptation_set_xml(rep: &RepSpec, pssh: &str, pro: &str, widevine: &str) -> String {
    let mut protection = format!(
        "<ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"cenc\" \
         cenc:default_KID=\"{KID_UUID}\" xmlns:cenc=\"urn:mpeg:cenc:2013\"/>"
    );
    if rep.include_widevine_pssh {
        write!(
            protection,
            "<ContentProtection schemeIdUri=\"{WIDEVINE_SYSTEM_ID_URI}\"><cenc:pssh \
             xmlns:cenc=\"urn:mpeg:cenc:2013\">{widevine}</cenc:pssh></ContentProtection>"
        )
        .unwrap();
    }
    if rep.include_playready_pssh {
        write!(
            protection,
            "<ContentProtection schemeIdUri=\"{PLAYREADY_SYSTEM_ID_URI}\"><cenc:pssh \
             xmlns:cenc=\"urn:mpeg:cenc:2013\">{pssh}</cenc:pssh><mspr:pro \
             xmlns:mspr=\"urn:microsoft:playready\">{pro}</mspr:pro></ContentProtection>"
        )
        .unwrap();
    }
    let ext = if rep.mime_type == "video/webm" {
        "hdr"
    } else {
        "mp4"
    };
    let size = match (rep.width, rep.height) {
        (Some(width), Some(height)) if width != 0 && height != 0 => {
            format!(" width=\"{width}\" height=\"{height}\"")
        }
        _ => String::new(),
    };
    let representation = format!(
        "<Representation id={} codecs={} bandwidth=\"1\"{size}><SegmentTemplate \
         timescale=\"{}\" startNumber=\"{}\" duration=\"{}\" media=\"{}/{}/seg_$Number$.m4s\" \
         initialization=\"{}/{}/init.{ext}\"/></Representation>",
        quoteattr(&rep.id),
        quoteattr(&rep.codec),
        rep.timescale,
        rep.start_number,
        rep.duration_ticks,
        rep.content_type,
        rep.id,
        rep.content_type,
        rep.id
    );
    let lang = rep
        .lang
        .as_deref()
        .filter(|lang| !lang.is_empty())
        .map_or_else(String::new, |lang| format!(" lang=\"{lang}\""));
    format!(
        "<AdaptationSet mimeType=\"{}\" contentType=\"{}\"{lang}>{protection}{representation}</AdaptationSet>",
        rep.mime_type, rep.content_type
    )
}

/// `build_synthetic_dash_asset`'s keyword arguments.
pub struct DashOptions {
    pub title_id: String,
    pub manifest_host: String,
    pub cdn_host: String,
    pub media_presentation_duration: String,
    pub playready_pssh_b64: Option<String>,
    pub playready_pro_b64: Option<String>,
}

impl Default for DashOptions {
    fn default() -> Self {
        DashOptions {
            title_id: "title-abc123".to_string(),
            manifest_host: "manifest.example.test".to_string(),
            cdn_host: "cdn.example.test".to_string(),
            media_presentation_duration: "PT18.0S".to_string(),
            playready_pssh_b64: None,
            playready_pro_b64: None,
        }
    }
}

impl DashOptions {
    pub fn titled(title_id: &str) -> Self {
        DashOptions {
            title_id: title_id.to_string(),
            ..DashOptions::default()
        }
    }
}

/// `build_synthetic_dash_asset`: the manifest and, per rep, its init and segments.
pub fn build_synthetic_dash_asset(root: &Path, reps: &[RepSpec], options: &DashOptions) -> PathBuf {
    let blobs = fake_blobs();
    let pssh = options
        .playready_pssh_b64
        .clone()
        .unwrap_or(blobs.playready_pssh);
    let pro = options
        .playready_pro_b64
        .clone()
        .unwrap_or(blobs.playready_pro);
    let download_root = root.join("download");
    let base_url = format!("https://{}/{}/", options.cdn_host, options.title_id);
    let body: String = reps
        .iter()
        .map(|rep| adaptation_set_xml(rep, &pssh, &pro, &blobs.widevine))
        .collect();
    let mpd = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" \
         type=\"static\" mediaPresentationDuration=\"{}\" minBufferTime=\"PT2S\" \
         profiles=\"urn:mpeg:dash:profile:isoff-live:2011\"><BaseURL>{base_url}</BaseURL><Period \
         id=\"p0\">{body}</Period></MPD>\n",
        options.media_presentation_duration
    );
    let mpd_path = download_root
        .join(&options.manifest_host)
        .join(&options.title_id)
        .join("dash/manifest.mpd");
    std::fs::create_dir_all(mpd_path.parent().unwrap()).unwrap();
    std::fs::write(&mpd_path, mpd).unwrap();
    for rep in reps {
        let ext = if rep.mime_type == "video/webm" {
            "hdr"
        } else {
            "mp4"
        };
        let rep_dir = download_root
            .join(&options.cdn_host)
            .join(&options.title_id)
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
    mpd_path
}
