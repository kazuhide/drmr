//! Short licensed source presentations shared by the fragmented-MP4 to TS cases.
use super::*;

pub const KID: &str = "000102030405060708090a0b0c0d0e0f";
pub const KEY: &str = "101112131415161718191a1b1c1d1e1f";
pub const IV: &str = "202122232425262728292a2b2c2d2e2f";
pub const URI: &str = "skd://synthetic-fmp4-ts-content";

pub fn source_keys(target: TargetProtection) -> InputKeys {
    if target == TargetProtection::Clear {
        InputKeys::default()
    } else {
        InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap()
    }
}

pub fn transport_source(root: &Path, layout: &str, rate: Option<u32>) -> PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture());
    if layout != "audio" {
        command.args(["-map", "0:v:0", "-c:v", "copy"]);
    }
    if layout != "video" {
        command.args(["-map", "0:a:0"]);
        if let Some(rate) = rate {
            command.args(["-c:a", "aac", "-threads", "1", "-ar"]);
            command.arg(rate.to_string());
        } else {
            command.args(["-c:a", "copy"]);
        }
    }
    let result = command
        .args([
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(root.join("segment-%d.ts"))
        .arg(root.join("media.m3u8"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "licensed short transport fixture failed"
    );
    root.join("media.m3u8")
}

pub fn fmp4_source(
    root: &Path,
    original: &Path,
    format: OutputFormat,
    target: TargetProtection,
    mode: ProcessingMode,
) -> PathBuf {
    let output_key =
        (target != TargetProtection::Clear).then(|| OutputKey::from_hex(KID, KEY).unwrap());
    let (command, _) = converter();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let report = command
        .with_processing(mode)
        .convert(ConversionRequest {
            input: original,
            output: root,
            format,
            target,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: output_key.as_ref(),
            hls_key_uri: (format == OutputFormat::Hls && target == TargetProtection::Cbcs)
                .then_some(URI),
            hls_output_key: None,
        })
        .unwrap();
    assert_native(&report.verification);
    root.join(if format == OutputFormat::Dash {
        "manifest.mpd"
    } else {
        "master.m3u8"
    })
}

pub fn fairplay_keys(root: &Path, name: &str, key: &str, iv: &str) -> key_file::OperatorKeys {
    use std::io::Write as _;
    let path = root.join(format!("{name}.json"));
    drmr_core::native::create_private_new(&path)
        .unwrap()
        .write_all(
            serde_json::json!({"schema_version":1,"output":{
                "key":key,"iv":iv,"hls_key_uri":URI,
                "signaling":{"system":"multi-drm","fairplay":true}
            }})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    key_file::read(&path).unwrap()
}

pub fn cenc_hls_source(root: &Path, original: &Path) -> PathBuf {
    use std::io::Write as _;
    drmr_core::native::create_media_dir_all(root).unwrap();
    let combined = root.with_extension("ts");
    let mut file = drmr_core::native::create_private_new(&combined).unwrap();
    let text = std::fs::read_to_string(original).unwrap();
    for resource in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        file.write_all(&std::fs::read(original.parent().unwrap().join(resource)).unwrap())
            .unwrap();
    }
    drop(file);
    let mut command = Command::new("packager");
    command.arg("--single_threaded");
    for role in ["video", "audio"] {
        command.arg(format!(
            "in={},stream={role},init_segment={},segment_template={},playlist_name={role}.m3u8",
            combined.display(),
            root.join(format!("{role}-init.mp4")).display(),
            root.join(format!("{role}-$Number$.m4s")).display()
        ));
    }
    let result = command
        .arg("--hls_master_playlist_output")
        .arg(root.join("master.m3u8"))
        .args([
            "--segment_duration",
            "2",
            "--clear_lead",
            "0",
            "--mp4_reset_initial_composition_offset_to_zero=false",
            "--enable_raw_key_encryption",
            "--keys",
            &format!("label=:key_id={KID}:key={KEY}"),
            "--protection_scheme",
            "cenc",
            "--protection_systems",
            "CommonSystem",
            "--hls_key_uri",
            URI,
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "independent short CENC HLS fixture failed"
    );
    // CommonSystem creates the encrypted MP4 records; declare the matching HLS
    // sample-encryption method explicitly instead of relying on an absent key tag.
    for playlist in media_playlists(root) {
        let text = std::fs::read_to_string(&playlist).unwrap();
        assert!(!text.contains("#EXT-X-KEY:"));
        let declaration =
            format!("#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"{URI}\",KEYFORMAT=\"identity\"\n");
        std::fs::write(
            playlist,
            text.replace("#EXT-X-MAP:", &(declaration + "#EXT-X-MAP:")),
        )
        .unwrap();
    }
    root.join("master.m3u8")
}

pub fn media_playlists(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut result = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            assert!(!path.is_symlink());
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "m3u8")
                && std::fs::read_to_string(&path).unwrap().contains("#EXTINF:")
            {
                result.push(path);
            }
        }
    }
    result.sort();
    assert_ne!(result, [] as [PathBuf; 0]);
    result
}

pub fn assert_transport_manifest(root: &Path, encrypted: bool, tracks: usize) {
    let playlists = media_playlists(root);
    assert_eq!(playlists.len(), tracks);
    for playlist in playlists {
        let text = std::fs::read_to_string(&playlist).unwrap();
        assert!(!text.contains("#EXT-X-MAP:"));
        assert!(text.contains("#EXT-X-ENDLIST"));
        if encrypted {
            assert!(text.contains("METHOD=SAMPLE-AES,"));
            assert!(text.contains("KEYFORMAT=\"com.apple.streamingkeydelivery\""));
            assert!(text.contains("KEYFORMATVERSIONS=\"1\""));
        } else {
            assert!(!text.contains("#EXT-X-KEY:"));
        }
        for resource in text
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            assert!(
                Path::new(resource)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
            );
            assert!(playlist.parent().unwrap().join(resource).is_file());
        }
    }
}
