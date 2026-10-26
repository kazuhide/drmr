//! HLS declarations are derived from the written resources and explicit selection metadata.
use super::super::{ConversionError, files, presentation::metadata};
use crate::concurrency::Stop;
use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
};

const BAD: ConversionError = ConversionError("hls-output-metadata-invalid");
pub(in crate::conversion) mod rates;
use rates::{SCALE, rate};
const MAX_RESOURCES: usize = 100_000;

pub(in crate::conversion) struct HlsResource {
    pub path: PathBuf,
    pub duration: String,
    pub key_tag: Option<String>,
}

pub(in crate::conversion) struct HlsPart {
    pub initialization: Option<PathBuf>,
    pub resources: Vec<HlsResource>,
    pub codecs: Vec<String>,
    pub resolution: Option<(u64, u64)>,
    pub metadata: metadata::Metadata,
    pub initial_key_tag: Option<String>,
}

struct Measured {
    peak: u64,
    average: u64,
    target: u64,
    version: u8,
}

fn check(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}

pub(super) fn duration(text: &str) -> Result<u128, ConversionError> {
    rates::duration(text)
}

fn quoted(value: &str) -> Result<String, ConversionError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(|c| c.is_control() || c == '"') {
        return Err(BAD);
    }
    Ok(format!("\"{value}\""))
}

fn key_version(tag: Option<&str>) -> Result<u8, ConversionError> {
    let Some(tag) = tag else { return Ok(3) };
    if tag.len() > 16 * 1024 || tag.chars().any(char::is_control) {
        return Err(BAD);
    }
    let attrs = super::super::hls::attributes(tag.strip_prefix("#EXT-X-KEY:").ok_or(BAD)?)?;
    match attrs.get("METHOD") {
        Some("NONE" | "AES-128") => Ok(if attrs.get("KEYFORMAT").is_some() {
            5
        } else {
            3
        }),
        Some("SAMPLE-AES" | "SAMPLE-AES-CTR") => Ok(5),
        _ => Err(BAD),
    }
}

fn filename(path: &Path, directory: &Path) -> Result<String, ConversionError> {
    let relative = path.strip_prefix(directory).map_err(|_| BAD)?;
    let mut components = relative.components();
    let Some(std::path::Component::Normal(name)) = components.next() else {
        return Err(BAD);
    };
    if components.next().is_some() {
        return Err(BAD);
    }
    let name = name.to_str().ok_or(BAD)?;
    if name.is_empty()
        || name.len() > 256
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
    {
        return Err(BAD);
    }
    Ok(name.into())
}

fn size(path: &Path, total: &mut u64) -> Result<u64, ConversionError> {
    let size = files::regular_input(path)?
        .metadata()
        .map_err(|_| BAD)?
        .len();
    *total = total
        .checked_add(size)
        .filter(|total| *total <= super::super::MAX_MEDIA_BYTES)
        .ok_or(BAD)?;
    Ok(size)
}

fn measure(
    part: &HlsPart,
    directory: &Path,
    total: &mut u64,
    target: u64,
    stop: &Stop,
) -> Result<Measured, ConversionError> {
    let mut prefix = Vec::with_capacity(part.resources.len() + 1);
    prefix.push((0_u128, 0_u128));
    let mut longest = 0;
    let mut maximum = 0;
    let mut version = key_version(part.initial_key_tag.as_deref())?;
    if let Some(path) = &part.initialization {
        filename(path, directory)?;
        size(path, total)?;
        version = version.max(6);
    }
    for resource in &part.resources {
        check(stop)?;
        filename(&resource.path, directory)?;
        let time = duration(&resource.duration)?;
        let bits = u128::from(size(&resource.path, total)?)
            .checked_mul(8)
            .ok_or(BAD)?;
        maximum = maximum.max(rate(bits, time)?);
        longest = longest.max(time);
        version = version.max(key_version(resource.key_tag.as_deref())?);
        let &(prior_time, prior_bits) = prefix.last().ok_or(BAD)?;
        prefix.push((
            prior_time.checked_add(time).ok_or(BAD)?,
            prior_bits.checked_add(bits).ok_or(BAD)?,
        ));
    }
    if target == 0 || longest > u128::from(target).checked_mul(SCALE).ok_or(BAD)? {
        return Err(BAD);
    }
    let rates = rates::measure(&prefix, maximum, target, stop)?;
    Ok(Measured {
        peak: rates.peak,
        average: rates.average,
        target,
        version,
    })
}

fn text_file(path: &Path, text: &str, stop: &Stop) -> Result<(), ConversionError> {
    check(stop)?;
    if u64::try_from(text.len()).map_err(|_| BAD)? > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(BAD);
    }
    let mut output = files::create_new(path)?;
    for chunk in text.as_bytes().chunks(64 * 1024) {
        check(stop)?;
        output.write_all(chunk).map_err(|_| BAD)?;
    }
    output.sync_all().map_err(|_| BAD)
}

fn audio_only(part: &HlsPart) -> bool {
    part.codecs.iter().all(|codec| {
        codec.starts_with("mp4a.")
            || matches!(
                codec.as_str(),
                "ac-3" | "ec-3" | "opus" | "Opus" | "fLaC" | "flac"
            )
    })
}

fn write_media(
    directory: &Path,
    part: &HlsPart,
    measure: &Measured,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let mut media = format!(
        "#EXTM3U\n#EXT-X-VERSION:{}\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n",
        measure.version, measure.target
    );
    let mut active_key = part.initial_key_tag.as_deref();
    if let Some(tag) = active_key {
        writeln!(media, "{tag}").map_err(|_| BAD)?;
    }
    if let Some(initialization) = &part.initialization {
        writeln!(
            media,
            "#EXT-X-MAP:URI=\"{}\"",
            filename(initialization, directory)?
        )
        .map_err(|_| BAD)?;
    }
    for resource in &part.resources {
        check(stop)?;
        if let Some(tag) = resource.key_tag.as_deref()
            && active_key != Some(tag)
        {
            writeln!(media, "{tag}").map_err(|_| BAD)?;
            active_key = Some(tag);
        }
        writeln!(
            media,
            "#EXTINF:{},\n{}",
            resource.duration,
            filename(&resource.path, directory)?
        )
        .map_err(|_| BAD)?;
        if u64::try_from(media.len()).map_err(|_| BAD)? > crate::inspection::MAX_MANIFEST_BYTES {
            return Err(BAD);
        }
    }
    media.push_str("#EXT-X-ENDLIST\n");
    text_file(&directory.join("media.m3u8"), &media, stop)
}

fn write_variant(
    master: &mut String,
    index: usize,
    parts: &[HlsPart],
    measured: &[Measured],
    audio_index: Option<usize>,
    rendition: Option<&metadata::Rendition>,
) -> Result<(), ConversionError> {
    let part = &parts[index];
    let measure = &measured[index];
    let mut peak = measure.peak;
    let mut average = measure.average;
    let mut codecs = part.codecs.clone();
    if let Some(audio) = audio_index
        && audio != index
    {
        peak = peak.checked_add(measured[audio].peak).ok_or(BAD)?;
        average = average.checked_add(measured[audio].average).ok_or(BAD)?;
        for codec in &parts[audio].codecs {
            if !codecs.contains(codec) {
                codecs.push(codec.clone());
            }
        }
    }
    write!(
        master,
        "#EXT-X-STREAM-INF:BANDWIDTH={peak},AVERAGE-BANDWIDTH={average},CODECS=\"{}\"",
        codecs.join(",")
    )
    .map_err(|_| BAD)?;
    if let Some((width, height)) = part.resolution {
        if width == 0 || height == 0 {
            return Err(BAD);
        }
        write!(master, ",RESOLUTION={width}x{height}").map_err(|_| BAD)?;
    }
    if let Some(rendition) = rendition {
        if part
            .metadata
            .audio_group
            .as_ref()
            .is_some_and(|group| *group != rendition.group)
        {
            return Err(BAD);
        }
        write!(master, ",AUDIO={}", quoted(&rendition.group)?).map_err(|_| BAD)?;
    } else if part.metadata.audio_group.is_some() {
        return Err(BAD);
    }
    writeln!(master, "\ntrack-{index:04}/media.m3u8").map_err(|_| BAD)
}

pub(in crate::conversion) fn write(
    root: &Path,
    parts: &[HlsPart],
    stop: &Stop,
) -> Result<(), ConversionError> {
    check(stop)?;
    if parts.is_empty()
        || parts.len() > 64
        || parts
            .iter()
            .map(|part| part.resources.len())
            .try_fold(0_usize, usize::checked_add)
            .is_none_or(|n| n > MAX_RESOURCES)
    {
        return Err(BAD);
    }
    let longest = parts
        .iter()
        .flat_map(|part| &part.resources)
        .try_fold(0_u128, |longest, resource| {
            duration(&resource.duration).map(|duration| longest.max(duration))
        })?;
    let target = u64::try_from(longest.div_ceil(SCALE)).map_err(|_| BAD)?;
    let mut total = 0;
    let mut measured = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        if part.resources.is_empty()
            || part.codecs.is_empty()
            || part.codecs.len() > 16
            || part.codecs.iter().any(|codec| {
                codec.len() > 128
                    || codec.is_empty()
                    || !codec
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
            })
        {
            return Err(BAD);
        }
        measured.push(measure(
            part,
            &root.join(format!("track-{index:04}")),
            &mut total,
            target,
            stop,
        )?);
    }
    let separate_audio = parts
        .iter()
        .enumerate()
        .filter(|(_, part)| audio_only(part))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if separate_audio.len() > 1 {
        return Err(ConversionError("presentation-selection-not-representable"));
    }
    let audio_index = separate_audio
        .first()
        .copied()
        .filter(|&index| parts.len() > 1 || parts[index].metadata.audio.is_some());
    let rendition = audio_index.map(|index| {
        parts[index]
            .metadata
            .audio
            .clone()
            .unwrap_or(metadata::Rendition {
                group: "audio".into(),
                name: Some("Audio".into()),
                language: "und".into(),
                is_default: Some(true),
                autoselect: Some(true),
                channels: None,
            })
    });
    let version = measured.iter().map(|part| part.version).max().ok_or(BAD)?;
    let mut master = format!("#EXTM3U\n#EXT-X-VERSION:{version}\n");
    if let (Some(index), Some(rendition)) = (audio_index, rendition.as_ref()) {
        writeln!(
            master,
            "{}",
            metadata::hls_declaration(rendition, &format!("track-{index:04}/media.m3u8"))?
        )
        .map_err(|_| BAD)?;
    }
    for (index, part) in parts.iter().enumerate() {
        check(stop)?;
        let directory = root.join(format!("track-{index:04}"));
        let measure = &measured[index];
        write_media(&directory, part, measure, stop)?;
        if audio_index == Some(index) && parts.len() > 1 {
            continue;
        }
        write_variant(
            &mut master,
            index,
            parts,
            &measured,
            audio_index,
            rendition.as_ref(),
        )?;
    }
    text_file(&root.join("master.m3u8"), &master, stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(
        root: &Path,
        index: usize,
        init: bool,
        sizes: &[usize],
        durations: &[&str],
    ) -> HlsPart {
        let directory = root.join(format!("track-{index:04}"));
        files::create_dir(&directory).unwrap();
        let initialization = init.then(|| {
            let path = directory.join("init.mp4");
            fs::write(&path, [0_u8; 100]).unwrap();
            path
        });
        HlsPart {
            initialization,
            resources: sizes
                .iter()
                .zip(durations)
                .enumerate()
                .map(|(ordinal, (&size, duration))| {
                    let path = directory.join(format!(
                        "segment-{ordinal:05}.{}",
                        if init { "m4s" } else { "ts" }
                    ));
                    fs::write(&path, vec![0_u8; size]).unwrap();
                    HlsResource {
                        path,
                        duration: (*duration).into(),
                        key_tag: None,
                    }
                })
                .collect(),
            codecs: vec!["avc1.64001f".into()],
            resolution: Some((640, 360)),
            metadata: metadata::Metadata::default(),
            initial_key_tag: None,
        }
    }

    #[test]
    fn durations_versions_and_measured_rates_follow_the_written_resources() {
        for (init, method, version) in [
            (false, None, 3),
            (false, Some("AES-128"), 3),
            (false, Some("SAMPLE-AES"), 5),
            (true, Some("SAMPLE-AES"), 6),
        ] {
            let root = files::Stage::new(None).unwrap();
            let mut part = fixture(
                root.path(),
                0,
                init,
                &[1000, 2000, 1200],
                &["1.0", "3.000", "2"],
            );
            if let Some(method) = method {
                part.initial_key_tag = Some(format!(
                    "#EXT-X-KEY:METHOD={method},URI=\"skd://synthetic\",IV=0x00000000000000000000000000000001"
                ));
            }
            write(root.path(), &[part], &Stop::default()).unwrap();
            let media = fs::read_to_string(root.path().join("track-0000/media.m3u8")).unwrap();
            assert!(media.contains(&format!("#EXT-X-VERSION:{version}\n")));
            assert!(media.contains("#EXT-X-TARGETDURATION:3\n"));
            assert!(media.contains("#EXTINF:1.0,\nsegment-00000."));
            assert!(media.contains("#EXTINF:3.000,\nsegment-00001."));
            assert!(media.ends_with("#EXT-X-ENDLIST\n"));
            assert!(!media.contains("INDEPENDENT-SEGMENTS"));
            assert_eq!(media.contains("#EXT-X-MAP:"), init);
            let master = fs::read_to_string(root.path().join("master.m3u8")).unwrap();
            // The one-second resource is below half of the target duration.
            // Its two-resource window is the actual peak: 3000 * 8 / 4.
            assert!(
                master.contains("BANDWIDTH=6000,AVERAGE-BANDWIDTH=5600,"),
                "{master}"
            );
        }
    }

    #[test]
    fn separate_audio_keeps_selection_metadata_codecs_and_aggregate_rates() {
        let root = files::Stage::new(None).unwrap();
        let mut video = fixture(root.path(), 0, true, &[1000, 2000], &["2", "2"]);
        video.metadata.audio_group = Some("stereo".into());
        let mut audio = fixture(root.path(), 1, true, &[100, 200], &["2", "2"]);
        audio.resolution = None;
        audio.codecs = vec!["mp4a.40.2".into()];
        audio.metadata.audio = Some(metadata::Rendition {
            group: "stereo".into(),
            name: Some("English".into()),
            language: "en".into(),
            is_default: Some(true),
            autoselect: Some(true),
            channels: Some("2".into()),
        });
        write(root.path(), &[video, audio], &Stop::default()).unwrap();
        let master = fs::read_to_string(root.path().join("master.m3u8")).unwrap();
        assert!(
            master.contains("BANDWIDTH=8800,AVERAGE-BANDWIDTH=6600,"),
            "{master}"
        );
        assert!(
            master.contains("CODECS=\"avc1.64001f,mp4a.40.2\",RESOLUTION=640x360,AUDIO=\"stereo\"")
        );
        assert!(master.contains("#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"stereo\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"track-0001/media.m3u8\",CHANNELS=\"2\""));
        assert_eq!(master.matches("#EXT-X-STREAM-INF:").count(), 1);
    }

    #[test]
    fn unequal_renditions_share_one_target_and_use_its_peak_duration_windows() {
        let root = files::Stage::new(None).unwrap();
        let video = fixture(
            root.path(),
            0,
            true,
            &[1000, 2000, 1200],
            &["6.5", "2", "3"],
        );
        let mut audio = fixture(root.path(), 1, true, &[100, 200, 120], &["5.5", "3", "3"]);
        audio.codecs = vec!["mp4a.40.2".into()];
        audio.resolution = None;
        write(root.path(), &[video, audio], &Stop::new()).unwrap();
        for index in 0..2 {
            let text = fs::read_to_string(root.path().join(format!("track-{index:04}/media.m3u8")))
                .unwrap();
            assert!(text.contains("#EXT-X-TARGETDURATION:7\n"), "{text}");
        }
        let master = fs::read_to_string(root.path().join("master.m3u8")).unwrap();
        // Video's 2+3 second window: 3200*8/5 = 5120.
        // Audio's 3+3 second window: 320*8/6 rounds up to 427.
        assert!(master.contains("BANDWIDTH=5547,"), "{master}");
    }

    #[test]
    fn key_rotation_is_written_in_order_without_redundant_tags() {
        let root = files::Stage::new(None).unwrap();
        let mut part = fixture(root.path(), 0, false, &[1, 1, 1, 1], &["2", "2", "2", "2"]);
        let first = "#EXT-X-KEY:METHOD=AES-128,URI=\"first\",IV=0x01";
        let second = "#EXT-X-KEY:METHOD=AES-128,URI=\"second\",IV=0x02";
        part.initial_key_tag = Some(first.into());
        part.resources[0].key_tag = Some(first.into());
        part.resources[1].key_tag = Some(second.into());
        part.resources[2].key_tag = Some(second.into());
        part.resources[3].key_tag = Some("#EXT-X-KEY:METHOD=NONE".into());
        write(root.path(), &[part], &Stop::default()).unwrap();
        let media = fs::read_to_string(root.path().join("track-0000/media.m3u8")).unwrap();
        assert_eq!(media.matches("#EXT-X-KEY:").count(), 3);
        assert!(media.find(second).unwrap() < media.find("#EXTINF:2,\nsegment-00001.ts").unwrap());
    }

    #[test]
    fn unsafe_paths_tags_and_cancellation_never_overwrite_foreign_files() {
        let root = files::Stage::new(None).unwrap();
        let mut part = fixture(root.path(), 0, false, &[1], &["2"]);
        part.resources[0].key_tag = Some("#EXT-X-KEY:METHOD=NONE\n#EXT-X-ENDLIST".into());
        assert!(write(root.path(), &[part], &Stop::default()).is_err());
        assert!(!root.path().join("master.m3u8").exists());
        let root = files::Stage::new(None).unwrap();
        let mut part = fixture(root.path(), 0, false, &[1], &["2"]);
        part.resources[0].path = PathBuf::from("/outside/segment.ts");
        assert!(write(root.path(), &[part], &Stop::default()).is_err());
        let root = files::Stage::new(None).unwrap();
        let part = fixture(root.path(), 0, false, &[1], &["2"]);
        fs::write(root.path().join("master.m3u8"), b"foreign").unwrap();
        assert!(write(root.path(), &[part], &Stop::default()).is_err());
        assert_eq!(
            fs::read(root.path().join("master.m3u8")).unwrap(),
            b"foreign"
        );
        let stop = Stop::default();
        stop.set();
        assert_eq!(
            write(root.path(), &[], &stop).unwrap_err(),
            ConversionError("cancelled")
        );
    }

    #[test]
    fn bounded_peak_search_matches_an_independent_exhaustive_window_oracle() {
        let mut random = 7_u64;
        for count in 1..=24 {
            let mut sizes = Vec::new();
            let mut milliseconds = Vec::new();
            for _ in 0..count {
                random = random
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                sizes.push(1 + usize::try_from((random >> 32) % 20_000).unwrap());
                random = random
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                milliseconds.push(1 + (random >> 32) % 5000);
            }
            let target_ms = milliseconds.iter().max().unwrap().div_ceil(1000) * 1000;
            let mut expected = 0_u64;
            for first in 0..count {
                let mut time = 0_u64;
                let mut bytes = 0_u64;
                for last in first..count {
                    time += milliseconds[last];
                    bytes += u64::try_from(sizes[last]).unwrap();
                    if 2 * time >= target_ms && 2 * time <= 3 * target_ms {
                        expected = expected.max((bytes * 8 * 1000).div_ceil(time));
                    }
                }
            }
            if milliseconds.iter().sum::<u64>() * 2 < target_ms {
                expected = sizes
                    .iter()
                    .zip(&milliseconds)
                    .map(|(&bytes, &time)| {
                        (u64::try_from(bytes).unwrap() * 8 * 1000).div_ceil(time)
                    })
                    .max()
                    .unwrap();
            }
            let texts = milliseconds
                .iter()
                .map(|value| format!("{}.{:03}", value / 1000, value % 1000))
                .collect::<Vec<_>>();
            let texts = texts.iter().map(String::as_str).collect::<Vec<_>>();
            let root = files::Stage::new(None).unwrap();
            let part = fixture(root.path(), 0, false, &sizes, &texts);
            let actual = measure(
                &part,
                &root.path().join("track-0000"),
                &mut 0,
                target_ms / 1000,
                &Stop::new(),
            )
            .unwrap();
            assert_eq!(
                actual.peak, expected,
                "count={count}, milliseconds={milliseconds:?}"
            );
        }
    }
}
