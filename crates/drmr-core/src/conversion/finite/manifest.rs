//! Final declarations are derived from the candidate's own edited sample presentation.
use super::{BAD, ConversionError, Path, Stop, Write, window};
use crate::inspection::OutputFormat;
use std::{fmt::Write as _, path::PathBuf};
pub(in crate::conversion) fn seconds(ticks: u64, scale: u64) -> Result<String, ConversionError> {
    if scale == 0 {
        return Err(BAD);
    }
    let n = u128::from(ticks).checked_mul(1_000_000_000).ok_or(BAD)?;
    let ns = n.div_ceil(u128::from(scale));
    let fraction = ns % 1_000_000_000;
    let mut out = format!("{}.{fraction:09}", ns / 1_000_000_000);
    while out.ends_with('0') {
        out.pop();
    }
    if out.ends_with('.') {
        out.pop();
    }
    Ok(out)
}
fn write(path: &Path, text: &str) -> Result<(), ConversionError> {
    if u64::try_from(text.len()).map_err(|_| BAD)? > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(BAD);
    }
    super::super::files::seal(path.parent().ok_or(BAD)?)?;
    crate::native::create_media_or_truncate(path)
        .map_err(|_| BAD)?
        .write_all(text.as_bytes())
        .map_err(|_| BAD)
}
fn hls(root: &Path, count: usize, stop: &Stop) -> Result<(), ConversionError> {
    for index in 0..count {
        let dir = root.join(format!("track-{index:04}"));
        let init = dir.join("init.mp4");
        if window(&init, 0)?.is_none() {
            continue;
        }
        let path = dir.join("media.m3u8");
        let bytes = super::super::files::read_manifest(&path)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| BAD)?;
        let mut clock = super::super::timing::FragmentClock::read(&init)?;
        let mut lines: Vec<_> = text.lines().map(str::to_string).collect();
        let mut pending = None;
        let mut max_seconds = 0u64;
        for index in 0..lines.len() {
            if stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            if lines[index].starts_with("#EXTINF:") {
                if pending.replace(index).is_some() {
                    return Err(BAD);
                }
            } else if !lines[index].is_empty() && !lines[index].starts_with('#') {
                let at = pending.take().ok_or(BAD)?;
                let uri = &lines[index];
                let number = uri
                    .strip_prefix("seg_")
                    .and_then(|v| v.strip_suffix(".m4s"))
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or(BAD)?;
                let (first, end, scale) =
                    clock.segment_presentation(&dir.join(format!("seg_{number}.m4s")))?;
                let duration = end.checked_sub(first).filter(|v| *v > 0).ok_or(BAD)?;
                max_seconds = max_seconds.max(duration.div_ceil(scale));
                lines[at] = format!("#EXTINF:{},", seconds(duration, scale)?);
            }
        }
        if pending.is_some() {
            return Err(BAD);
        }
        clock.presentation_window()?;
        for line in &mut lines {
            if line.starts_with("#EXT-X-TARGETDURATION:") {
                *line = format!("#EXT-X-TARGETDURATION:{max_seconds}");
            }
        }
        write(&path, &format!("{}\n", lines.join("\n")))?;
    }
    Ok(())
}
fn dash(root: &Path, count: usize, stop: &Stop) -> Result<(), ConversionError> {
    let path = root.join("manifest.mpd");
    let bytes = super::super::files::read_manifest(&path)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| BAD)?;
    let doc = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| BAD)?;
    let representations: Vec<_> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "Representation")
        .collect();
    if representations.len() != count {
        return Err(BAD);
    }
    let mut changes = Vec::new();
    let mut maximum = (0u64, 1u64);
    for (index, node) in representations.iter().enumerate() {
        let init = root.join(format!("track-{index:04}/init.mp4"));
        // A mixed presentation can contain finite edits and complete open-ended tracks.
        // Each clock reads its own initialization; every track contributes its actual end.
        let template = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "SegmentTemplate")
            .ok_or(BAD)?;
        let timeline = template
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "SegmentTimeline")
            .ok_or(BAD)?;
        let values = super::super::dash::sequence(&[template], 100_000)?;
        let scale = template
            .attribute("timescale")
            .ok_or(BAD)?
            .parse::<u64>()
            .map_err(|_| BAD)?;
        let mut clock = super::super::timing::FragmentClock::read(&init)?;
        let mut rewritten = String::from("<SegmentTimeline>\n");
        for (number, _, _) in values {
            if stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            let (first, end, actual_scale) = clock
                .segment_presentation(&root.join(format!("track-{index:04}/seg_{number}.m4s")))?;
            let convert = |value: u64| -> Result<u64, ConversionError> {
                let n = u128::from(value) * u128::from(scale);
                if actual_scale == 0 || !n.is_multiple_of(u128::from(actual_scale)) {
                    return Err(BAD);
                }
                u64::try_from(n / u128::from(actual_scale)).map_err(|_| BAD)
            };
            let time = convert(first)?;
            let duration = convert(end.checked_sub(first).filter(|v| *v > 0).ok_or(BAD)?)?;
            writeln!(rewritten, "<S t=\"{time}\" d=\"{duration}\"/>").map_err(|_| BAD)?;
            if u64::try_from(rewritten.len()).map_err(|_| BAD)?
                > crate::inspection::MAX_MANIFEST_BYTES
            {
                return Err(BAD);
            }
        }
        rewritten.push_str("</SegmentTimeline>");
        changes.push((timeline.range(), rewritten));
        let (first, end, scale) = clock.presentation_window()?;
        if first != 0 {
            return Err(BAD);
        }
        if u128::from(end) * u128::from(maximum.1) > u128::from(maximum.0) * u128::from(scale) {
            maximum = (end, scale);
        }
    }
    let duration = format!("PT{}S", seconds(maximum.0, maximum.1)?);
    let rootnode = doc.root_element();
    let attribute = rootnode
        .attribute_node("mediaPresentationDuration")
        .ok_or(BAD)?;
    changes.push((attribute.range_value(), duration.clone()));
    let period = rootnode
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "Period")
        .ok_or(BAD)?;
    if let Some(attribute) = period.attribute_node("duration") {
        changes.push((attribute.range_value(), duration));
    } else {
        let at = period.range().start + text[period.range()].find('>').ok_or(BAD)?;
        changes.push((at..at, format!(" duration=\"{duration}\"")));
    }
    changes.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut output = text.to_string();
    let mut previous = text.len();
    for (range, value) in changes {
        if range.end > previous {
            return Err(BAD);
        }
        previous = range.start;
        output.replace_range(range, &value);
    }
    write(&path, &output)
}
pub(in crate::conversion) fn finish(
    root: &Path,
    clear: &[PathBuf],
    format: OutputFormat,
    stop: &Stop,
) -> Result<(), ConversionError> {
    if !clear
        .iter()
        .map(|p| window(p, 0))
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(Option::is_some)
    {
        return Ok(());
    }
    match format {
        OutputFormat::Dash => dash(root, clear.len(), stop),
        OutputFormat::Hls => hls(root, clear.len(), stop),
        _ => Err(BAD),
    }
}
