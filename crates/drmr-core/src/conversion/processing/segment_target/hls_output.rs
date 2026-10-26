//! Preserve an inherited HLS bound only after the generated durations are final.
use super::super::super::{ConversionError, Converter, files};
use std::{
    collections::BTreeMap,
    io::Write as _,
    ops::Range,
    path::{Path, PathBuf},
};

mod bandwidth;

const INVALID: ConversionError = ConversionError("hls-output-target-duration-mismatch");
const TAG: &str = "#EXT-X-TARGETDURATION:";

// Decimal text is compared exactly: a long fraction immediately below one half
// must not become one half through floating-point rounding.
fn rounded_seconds(value: &str) -> Result<u64, ConversionError> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if !whole
        .bytes()
        .chain(fraction.bytes())
        .all(|byte| byte.is_ascii_digit())
        || whole.is_empty() && fraction.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .any(|byte| byte != b'0')
    {
        return Err(INVALID);
    }
    let seconds = if whole.is_empty() {
        0
    } else {
        whole.parse::<u64>().map_err(|_| INVALID)?
    };
    seconds
        .checked_add(u64::from(
            fraction
                .as_bytes()
                .first()
                .is_some_and(|byte| *byte >= b'5'),
        ))
        .ok_or(INVALID)
}

fn admitted_header(text: &str, target: u64) -> Result<Range<usize>, ConversionError> {
    super::declared_hls_seconds(text)?.ok_or(INVALID)?;
    let mut header = None;
    let mut segments = 0usize;
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(value) = line.strip_prefix(TAG) {
            header = Some(offset + TAG.len()..offset + TAG.len() + value.len());
        }
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            let (duration, _) = value.split_once(',').ok_or(INVALID)?;
            if rounded_seconds(duration)? > target {
                return Err(INVALID);
            }
            segments = segments.checked_add(1).ok_or(INVALID)?;
        }
        offset += raw.len();
    }
    if segments == 0 {
        return Err(INVALID);
    }
    header.ok_or(INVALID)
}

impl Converter {
    fn inherited_hls_target(&self) -> Option<u32> {
        (self.hls_target_policy != super::HlsTargetPolicy::Legacy)
            .then_some(self.source_hls_target_ms)
            .flatten()
    }

    pub(in crate::conversion) fn finish_hls_segment_target(
        &self,
        root: &Path,
        count: usize,
    ) -> Result<(), ConversionError> {
        let Some(milliseconds) = self.inherited_hls_target() else {
            // An MP4/DASH source has no inherited HLS bound. Legacy saved plans
            // retain generated headers regardless of numeric request coincidences.
            return Ok(());
        };
        finish(root, count, milliseconds, &self.backend.stop)
    }

    pub(in crate::conversion) fn finish_flat_hls_segment_target(
        &self,
        root: &Path,
    ) -> Result<(), ConversionError> {
        let Some(milliseconds) = self.inherited_hls_target() else {
            return Ok(());
        };
        finish_playlists(
            root,
            &[root.join("media.m3u8")],
            milliseconds,
            &self.backend.stop,
        )
    }
}

pub(in crate::conversion) fn finish(
    root: &Path,
    count: usize,
    milliseconds: u32,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    if count == 0 || count > 64 {
        return Err(INVALID);
    }
    let paths = (0..count)
        .map(|ordinal| root.join(format!("track-{ordinal:04}/media.m3u8")))
        .collect::<Vec<_>>();
    finish_playlists(root, &paths, milliseconds, stop)
}

fn replace_file(
    path: &Path,
    text: &str,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    if stop.is_set() {
        return Err(ConversionError("cancelled"));
    }
    files::seal(path.parent().ok_or(INVALID)?)?;
    let mut file = crate::native::create_media_or_truncate(path).map_err(|_| INVALID)?;
    file.write_all(text.as_bytes()).map_err(|_| INVALID)?;
    file.sync_all().map_err(|_| INVALID)
}

fn finish_playlists(
    root: &Path,
    paths: &[PathBuf],
    milliseconds: u32,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    if milliseconds == 0 || !milliseconds.is_multiple_of(1000) {
        return Err(INVALID);
    }
    let target = u64::from(milliseconds / 1000);
    let target_text = target.to_string();
    let read = |path: &Path| -> Result<String, ConversionError> {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        String::from_utf8(files::read_manifest(path)?).map_err(|_| INVALID)
    };
    // Validate every sibling before changing any declaration. Read one
    // bounded playlist at a time rather than retaining the complete graph.
    let mut measured = BTreeMap::new();
    let mut budget = bandwidth::Budget::default();
    for path in paths {
        let text = read(path)?;
        admitted_header(&text, target)?;
        let rates = bandwidth::measure(path, &text, target, &mut budget, stop)?;
        measured.insert(path.canonicalize().map_err(|_| INVALID)?, rates);
    }
    let master = root.join("master.m3u8");
    let original_master = read(&master)?;
    let final_master = bandwidth::rewrite(root, &original_master, &measured, stop)?;
    for path in paths {
        let mut text = read(path)?;
        let range = admitted_header(&text, target)?;
        if text.get(range.clone()) == Some(target_text.as_str()) {
            continue;
        }
        text.replace_range(range, &target_text);
        if u64::try_from(text.len()).map_err(|_| INVALID)? > crate::inspection::MAX_MANIFEST_BYTES {
            return Err(INVALID);
        }
        replace_file(path, &text, stop)?;
    }
    if final_master != original_master {
        replace_file(&master, &final_master, stop)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::NoSink,
        toolrun::{Secrets, ToolRunner},
    };
    use std::fmt::Write as _;
    use std::sync::Arc;
    fn command(source: Option<u32>, selected: Option<u32>) -> Converter {
        let mut command = Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "must-not-run",
            "must-not-run",
        );
        command.source_hls_target_ms = source;
        command.segment_duration_ms = selected;
        command
    }
    fn fixture(root: &Path, ordinal: usize, duration: &str) -> std::path::PathBuf {
        let directory = root.join(format!("track-{ordinal:04}"));
        crate::native::create_media_dir_all(&directory).unwrap();
        let path = directory.join("media.m3u8");
        crate::native::create_media_new(&path).unwrap().write_all(format!("#EXTM3U\r\n{TAG}7\r\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd:synthetic\"\r\n#EXTINF:{duration},unchanged title\r\nseg_1.m4s\r\n#EXT-X-ENDLIST").as_bytes()).unwrap();
        crate::native::create_media_new(&directory.join("seg_1.m4s"))
            .unwrap()
            .write_all(&[0; 1000])
            .unwrap();
        let master = root.join("master.m3u8");
        let mut text = std::fs::read_to_string(&master).unwrap_or_else(|_| "#EXTM3U\r\n".into());
        write!(text, "#EXT-X-STREAM-INF:BANDWIDTH=1000,CODECS=\"avc1.64001f\"\r\ntrack-{ordinal:04}/media.m3u8\r\n").unwrap();
        crate::native::create_media_or_truncate(&master)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        path
    }
    #[test]
    fn decimal_rounding_is_exact_at_half_and_rejects_invalid_or_overflowing_values() {
        for (text, expected) in [
            ("6", 6),
            ("6.013977778", 6),
            ("6.499999999999999999999999999", 6),
            ("6.5", 7),
            ("6.5000000001", 7),
            (".5", 1),
            ("0.0001", 0),
            ("18446744073709551615.4", u64::MAX),
        ] {
            assert_eq!(rounded_seconds(text).unwrap(), expected, "{text}");
        }
        for text in [
            "",
            "0",
            "0.000",
            "+6",
            "-6",
            " 6",
            "6e0",
            "NaN",
            "6.1.0",
            "18446744073709551616",
            "18446744073709551615.5",
        ] {
            assert!(rounded_seconds(text).is_err(), "{text}");
        }
    }
    #[test]
    fn only_the_header_changes_after_all_siblings_fit_the_original_bound() {
        let root = files::Stage::new(None).unwrap();
        let video = fixture(root.path(), 0, "6.000");
        let audio = fixture(root.path(), 1, "6.013977778");
        let before = [
            std::fs::read_to_string(&video).unwrap(),
            std::fs::read_to_string(&audio).unwrap(),
        ];
        let command = command(Some(6000), Some(6000));
        command.finish_hls_segment_target(root.path(), 2).unwrap();
        for (path, text) in [(&video, &before[0]), (&audio, &before[1])] {
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                text.replace(&format!("{TAG}7"), &format!("{TAG}6"))
            );
        }
        command.finish_hls_segment_target(root.path(), 2).unwrap();
        assert!(
            std::fs::read_to_string(&audio)
                .unwrap()
                .contains("#EXTINF:6.013977778,unchanged title")
        );
    }
    #[test]
    fn legacy_targets_keep_generated_headers_even_when_the_source_bound_matches() {
        for source in [None, Some(2000), Some(6000)] {
            let root = files::Stage::new(None).unwrap();
            let path = fixture(root.path(), 0, "6.000");
            let before = std::fs::read(&path).unwrap();
            let mut command = command(source, Some(2000));
            command.hls_target_policy = super::super::HlsTargetPolicy::Legacy;
            command.finish_hls_segment_target(root.path(), 1).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
    #[test]
    fn current_inheritance_uses_the_source_bound_independently_of_the_saved_request() {
        let root = files::Stage::new(None).unwrap();
        let path = fixture(root.path(), 0, "6.013977778");
        let mut command = command(Some(6000), Some(2000));
        command.hls_target_policy = super::super::HlsTargetPolicy::SavedCurrent;
        command.finish_hls_segment_target(root.path(), 1).unwrap();
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("#EXT-X-TARGETDURATION:6\r\n")
        );
    }
    #[test]
    fn an_invalid_later_duration_or_cancelled_operation_cannot_edit_an_earlier_playlist() {
        for duration in ["6.5", "6.50000001", "7", "NaN", "+6"] {
            let root = files::Stage::new(None).unwrap();
            let path = fixture(root.path(), 0, "6.000");
            fixture(root.path(), 1, duration);
            let before = std::fs::read(&path).unwrap();
            assert_eq!(
                command(Some(6000), Some(6000))
                    .finish_hls_segment_target(root.path(), 2)
                    .unwrap_err()
                    .0,
                INVALID.0
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let root = files::Stage::new(None).unwrap();
        let path = fixture(root.path(), 0, "6.000");
        let before = std::fs::read(&path).unwrap();
        let command = command(Some(6000), Some(6000));
        command.backend.stop.set();
        assert_eq!(
            command
                .finish_hls_segment_target(root.path(), 1)
                .unwrap_err()
                .0,
            "cancelled"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    fn peak_fixture(root: &Path, ordinal: usize, sizes: [usize; 2]) -> std::path::PathBuf {
        let directory = root.join(format!("track-{ordinal:04}"));
        crate::native::create_media_dir_all(&directory).unwrap();
        for (name, size) in ["first.ts", "tail.ts"].into_iter().zip(sizes) {
            crate::native::create_media_new(&directory.join(name))
                .unwrap()
                .write_all(&vec![0; size])
                .unwrap();
        }
        let path = directory.join("media.m3u8");
        crate::native::create_media_new(&path).unwrap().write_all(b"#EXTM3U\r\n#EXT-X-TARGETDURATION:7\r\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://synthetic.invalid/key\"\r\n#EXTINF:6.013977778,first title\r\nfirst.ts\r\n#EXTINF:3.1,last title\r\ntail.ts\r\n#EXT-X-ENDLIST\r\n").unwrap();
        path
    }

    #[test]
    fn lowering_the_target_remeasures_the_tail_peak_and_audio_group_without_metadata_edits() {
        let root = files::Stage::new(None).unwrap();
        let video = peak_fixture(root.path(), 0, [1000, 100_000]);
        let audio = peak_fixture(root.path(), 1, [100, 1000]);
        let before = [
            std::fs::read(&video).unwrap(),
            std::fs::read(&audio).unwrap(),
        ];
        let old_video = (101_000_u128 * 8 * 1_000_000_000).div_ceil(9_113_977_778);
        let old_audio = (1100_u128 * 8 * 1_000_000_000).div_ceil(9_113_977_778);
        let average = old_video + old_audio;
        let peak = (100_000_u128 * 8 * 10).div_ceil(31) + (1000_u128 * 8 * 10).div_ceil(31);
        assert!(peak > average);
        let master = format!(
            "#EXTM3U\r\n#EXT-X-VERSION:6\r\n#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"https://synthetic.invalid/key\"\r\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"stereo\",NAME=\"English, stereo\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"track-0001/media.m3u8\",CHANNELS=\"2\"\r\n#EXT-X-STREAM-INF:BANDWIDTH={average},AVERAGE-BANDWIDTH={average},CODECS=\"avc1.64001f,mp4a.40.2\",RESOLUTION=640x360,AUDIO=\"stereo\"\r\ntrack-0000/media.m3u8\r\n"
        );
        let master_path = root.path().join("master.m3u8");
        crate::native::create_media_new(&master_path)
            .unwrap()
            .write_all(master.as_bytes())
            .unwrap();
        command(Some(6000), Some(6000))
            .finish_hls_segment_target(root.path(), 2)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&master_path).unwrap(),
            master.replace(
                &format!(":BANDWIDTH={average},"),
                &format!(":BANDWIDTH={peak},")
            )
        );
        for (path, bytes) in [(&video, &before[0]), (&audio, &before[1])] {
            let text = String::from_utf8(bytes.clone()).unwrap();
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                text.replace(&format!("{TAG}7"), &format!("{TAG}6"))
            );
        }
    }

    #[test]
    fn an_audio_variant_alias_uses_the_largest_group_rate_without_double_counting() {
        let root = files::Stage::new(None).unwrap();
        peak_fixture(root.path(), 0, [100, 1000]);
        peak_fixture(root.path(), 1, [200, 2000]);
        let master = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"stereo\",NAME=\"First\",URI=\"track-0000/media.m3u8\"\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"stereo\",NAME=\"Second\",URI=\"track-0001/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"mp4a.40.2\",AUDIO=\"stereo\"\ntrack-0000/media.m3u8\n";
        let master_path = root.path().join("master.m3u8");
        crate::native::create_media_new(&master_path)
            .unwrap()
            .write_all(master.as_bytes())
            .unwrap();
        command(Some(6000), Some(6000))
            .finish_hls_segment_target(root.path(), 2)
            .unwrap();
        let peak = (2000_u128 * 8 * 10).div_ceil(31);
        let average = (2200_u128 * 8 * 1_000_000_000).div_ceil(9_113_977_778);
        let expected = master
            .replace("BANDWIDTH=1,", &format!("BANDWIDTH={peak},"))
            .replace(
                "AUDIO=\"stereo\"\ntrack-0000",
                &format!("AUDIO=\"stereo\",AVERAGE-BANDWIDTH={average}\ntrack-0000"),
            );
        assert_eq!(std::fs::read_to_string(master_path).unwrap(), expected);
    }

    #[test]
    fn an_invalid_later_master_variant_cannot_edit_any_media_header() {
        let root = files::Stage::new(None).unwrap();
        let video = fixture(root.path(), 0, "6.000");
        let audio = fixture(root.path(), 1, "6.013977778");
        let before = [
            std::fs::read(&video).unwrap(),
            std::fs::read(&audio).unwrap(),
        ];
        let master_path = root.path().join("master.m3u8");
        let master = std::fs::read_to_string(&master_path)
            .unwrap()
            .replace("track-0001/media.m3u8", "missing.m3u8");
        crate::native::create_media_or_truncate(&master_path)
            .unwrap()
            .write_all(master.as_bytes())
            .unwrap();
        assert!(
            command(Some(6000), Some(6000))
                .finish_hls_segment_target(root.path(), 2)
                .is_err()
        );
        assert_eq!(std::fs::read(&video).unwrap(), before[0]);
        assert_eq!(std::fs::read(&audio).unwrap(), before[1]);
        assert_eq!(std::fs::read_to_string(master_path).unwrap(), master);
    }
}
