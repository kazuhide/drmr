//! Generated audio and video playlists share a bound after their final sample timing is known.
use super::{BAD, ConversionError, Path, Stop};
use std::ops::Range;

const TAG: &str = "#EXT-X-TARGETDURATION:";

fn declaration(text: &str) -> Result<(Range<usize>, u64), ConversionError> {
    let mut found = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        if let Some(value) = content.strip_prefix(TAG) {
            let duration = value
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0 && value.bytes().all(|byte| byte.is_ascii_digit()))
                .ok_or(BAD)?;
            let start = offset + TAG.len();
            if found
                .replace((start..start + value.len(), duration))
                .is_some()
            {
                return Err(BAD);
            }
        }
        offset += line.len();
    }
    found.ok_or(BAD)
}

pub(super) fn finish(root: &Path, count: usize, stop: &Stop) -> Result<(), ConversionError> {
    if count == 0 || count > 64 {
        return Err(BAD);
    }
    let read = |index| -> Result<String, ConversionError> {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let bytes = crate::conversion::files::read_manifest(
            &root.join(format!("track-{index:04}/media.m3u8")),
        )?;
        String::from_utf8(bytes).map_err(|_| BAD)
    };
    let mut maximum = 0;
    for index in 0..count {
        maximum = maximum.max(declaration(&read(index)?)?.1);
    }
    // RFC 8216 section 6.2.4 requires the same target for these generated AV renditions.
    // Raising a declared bound changes neither EXTINF durations nor media resources.
    for index in 0..count {
        let mut text = read(index)?;
        let (range, duration) = declaration(&text)?;
        if duration > maximum {
            return Err(BAD);
        }
        if duration != maximum {
            text.replace_range(range, &maximum.to_string());
            super::write(&root.join(format!("track-{index:04}/media.m3u8")), &text)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    fn fixture(root: &Path, index: usize, target: &str) -> std::path::PathBuf {
        let directory = root.join(format!("track-{index:04}"));
        crate::native::create_media_dir_all(&directory).unwrap();
        let path = directory.join("media.m3u8");
        crate::native::create_media_new(&path).unwrap().write_all(format!("#EXTM3U\r\n{TAG}{target}\r\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd:synthetic\"\r\n#EXTINF:2.005333333,\r\nsegment.m4s\r\n#EXT-X-ENDLIST").as_bytes()).unwrap();
        path
    }
    #[test]
    fn only_the_generated_target_value_changes_and_repeated_finalization_is_stable() {
        let root = crate::native::private_tempdir().unwrap();
        let video = fixture(root.path(), 0, "2");
        let audio = fixture(root.path(), 1, "3");
        let before_video = std::fs::read_to_string(&video).unwrap();
        let before_audio = std::fs::read(&audio).unwrap();
        finish(root.path(), 2, &Stop::new()).unwrap();
        let expected = before_video.replace(&format!("{TAG}2"), &format!("{TAG}3"));
        assert_eq!(std::fs::read_to_string(&video).unwrap(), expected);
        assert_eq!(std::fs::read(&audio).unwrap(), before_audio);
        finish(root.path(), 2, &Stop::new()).unwrap();
        assert_eq!(std::fs::read_to_string(video).unwrap(), expected);
    }
    #[test]
    fn invalid_later_declarations_and_cancellation_cannot_modify_an_earlier_playlist() {
        for target in [
            "0",
            "+3",
            "-1",
            "3.0",
            "18446744073709551616",
            "3\r\n#EXT-X-TARGETDURATION:3",
        ] {
            let root = crate::native::private_tempdir().unwrap();
            let video = fixture(root.path(), 0, "2");
            fixture(root.path(), 1, target);
            let original = std::fs::read(&video).unwrap();
            assert!(finish(root.path(), 2, &Stop::new()).is_err());
            assert_eq!(std::fs::read(&video).unwrap(), original);
        }
        let root = crate::native::private_tempdir().unwrap();
        let video = fixture(root.path(), 0, "2");
        fixture(root.path(), 1, "3");
        let original = std::fs::read(&video).unwrap();
        let stop = Stop::new();
        stop.set();
        assert_eq!(finish(root.path(), 2, &stop).unwrap_err().0, "cancelled");
        assert_eq!(std::fs::read(video).unwrap(), original);
    }
}
