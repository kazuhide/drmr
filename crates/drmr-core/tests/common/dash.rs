//! Synthetic DASH assets for batch tests, with deterministic download layouts.
//! Each representation contains an initialization file and three media segments.

use std::path::{Path, PathBuf};

/// One representation (`RepSpec`).
#[derive(Clone, Debug)]
pub struct Rep {
    pub id: String,
    pub codec: String,
    pub content_type: String,
    pub mime_type: String,
    pub timescale: u32,
    pub duration_ticks: u32,
    pub segment_count: usize,
}

/// `h264_rep(id)`.
pub fn h264_rep(id: &str) -> Rep {
    Rep {
        id: id.to_string(),
        codec: "avc1.42C00C".to_string(),
        content_type: "video".to_string(),
        mime_type: "video/mp4".to_string(),
        timescale: 24_000,
        duration_ticks: 144_000,
        segment_count: 3,
    }
}

/// `audio_rep(id)`.
pub fn audio_rep(id: &str) -> Rep {
    Rep {
        id: id.to_string(),
        codec: "mp4a.40.2".to_string(),
        content_type: "audio".to_string(),
        mime_type: "audio/mp4".to_string(),
        timescale: 48_000,
        duration_ticks: 288_000,
        segment_count: 3,
    }
}

/// What was built.
pub struct Built {
    pub mpd_path: PathBuf,
}

const WIDEVINE: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
const PLAYREADY: &str = "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95";
const DEFAULT_KID: &str = "00010203-0405-0607-0809-0a0b0c0d0e0f";
const FAKE_WIDEVINE_PSSH_B64: &str = "RkFLRS1QU1NILU5PVC1BLVJFQUwtQk9YLUpVU1QtVEVTVC1CWVRFUw==";
const FAKE_PLAYREADY_PSSH_B64: &str =
    "RkFLRS1QTEFZUkVBRFktUFNTSC1CT1gtRElTVElOQ1QtVEVTVC1CWVRFUy0wMQ==";
const FAKE_PLAYREADY_PRO_B64: &str =
    "RkFLRS1QTEFZUkVBRFktUFJPLURJU1RJTkNULVRFU1QtQllURVMtVkFMVUUtMDI=";

fn adaptation_set(rep: &Rep) -> String {
    let protection = format!(
        "<ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"cenc\" \
         cenc:default_KID=\"{DEFAULT_KID}\" xmlns:cenc=\"urn:mpeg:cenc:2013\"/>\
         <ContentProtection schemeIdUri=\"{WIDEVINE}\"><cenc:pssh xmlns:cenc=\"urn:mpeg:cenc:2013\">\
         {FAKE_WIDEVINE_PSSH_B64}</cenc:pssh></ContentProtection>\
         <ContentProtection schemeIdUri=\"{PLAYREADY}\"><cenc:pssh xmlns:cenc=\"urn:mpeg:cenc:2013\">\
         {FAKE_PLAYREADY_PSSH_B64}</cenc:pssh><mspr:pro xmlns:mspr=\"urn:microsoft:playready\">\
         {FAKE_PLAYREADY_PRO_B64}</mspr:pro></ContentProtection>"
    );
    let ct = &rep.content_type;
    let id = &rep.id;
    let representation = format!(
        "<Representation id=\"{id}\" codecs=\"{}\" bandwidth=\"1\"><SegmentTemplate timescale=\"{}\" \
         startNumber=\"0\" duration=\"{}\" media=\"{ct}/{id}/seg_$Number$.m4s\" \
         initialization=\"{ct}/{id}/init.mp4\"/></Representation>",
        rep.codec, rep.timescale, rep.duration_ticks
    );
    format!(
        "<AdaptationSet mimeType=\"{}\" contentType=\"{ct}\">{protection}{representation}</AdaptationSet>",
        rep.mime_type
    )
}

/// `build_synthetic_dash_asset(root, reps, title_id=…)`.
pub fn build_synthetic_dash_asset(root: &Path, reps: &[Rep], title_id: &str) -> Built {
    let download = root.join("download");
    let body: String = reps.iter().map(adaptation_set).collect();
    let mpd = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" \
         type=\"static\" mediaPresentationDuration=\"PT18.0S\" minBufferTime=\"PT2S\" \
         profiles=\"urn:mpeg:dash:profile:isoff-live:2011\"><BaseURL>https://cdn.example.test/{title_id}/</BaseURL>\
         <Period id=\"p0\">{body}</Period></MPD>\n"
    );
    let mpd_path = download
        .join("manifest.example.test")
        .join(title_id)
        .join("dash/manifest.mpd");
    super::write(&mpd_path, mpd);
    for rep in reps {
        let directory = download
            .join("cdn.example.test")
            .join(title_id)
            .join(&rep.content_type)
            .join(&rep.id);
        super::write(&directory.join("init.mp4"), format!("INIT:{}", rep.id));
        for index in 0..rep.segment_count {
            super::write(
                &directory.join(format!("seg_{index}.m4s")),
                format!("SEG:{}:{index}", rep.id),
            );
        }
    }
    Built { mpd_path }
}
