//! Final byte sizes and the final HLS bound determine the advertised rates.
use super::{ConversionError, INVALID};
use crate::{
    concurrency::Stop,
    conversion::{files, hls, segments::hls_rates},
    m3u8::Attributes,
};
use hls_rates::Rates;
use std::{
    collections::BTreeMap,
    ops::Range,
    path::{Path, PathBuf},
};

fn check(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Budget {
    bytes: u64,
    resources: usize,
}

pub(super) fn measure(
    playlist: &Path,
    text: &str,
    target: u64,
    budget: &mut Budget,
    stop: &Stop,
) -> Result<Rates, ConversionError> {
    let root = playlist
        .parent()
        .ok_or(INVALID)?
        .canonicalize()
        .map_err(|_| INVALID)?;
    let mut prefix = vec![(0_u128, 0_u128)];
    let mut maximum = 0;
    let mut pending = None;
    for line in text.lines() {
        check(stop)?;
        if line.starts_with("#EXT-X-BYTERANGE:") {
            return Err(INVALID);
        }
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            let (duration, _) = value.split_once(',').ok_or(INVALID)?;
            if pending.replace(hls_rates::duration(duration)?).is_some() {
                return Err(INVALID);
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            if prefix.len() > 100_000 {
                return Err(INVALID);
            }
            budget.resources = budget
                .resources
                .checked_add(1)
                .filter(|count| *count <= 100_000)
                .ok_or(INVALID)?;
            let time = pending.take().ok_or(INVALID)?;
            let path = hls::local_resource(&root, line)?;
            let bytes = files::regular_input(&path)?
                .metadata()
                .map_err(|_| INVALID)?
                .len();
            budget.bytes = budget
                .bytes
                .checked_add(bytes)
                .filter(|total| *total <= crate::conversion::MAX_MEDIA_BYTES)
                .ok_or(INVALID)?;
            let bits = u128::from(bytes).checked_mul(8).ok_or(INVALID)?;
            maximum = maximum.max(hls_rates::rate(bits, time)?);
            let &(prior_time, prior_bits) = prefix.last().ok_or(INVALID)?;
            prefix.push((
                prior_time.checked_add(time).ok_or(INVALID)?,
                prior_bits.checked_add(bits).ok_or(INVALID)?,
            ));
        }
    }
    if pending.is_some() {
        return Err(INVALID);
    }
    hls_rates::measure(&prefix, maximum, target, stop)
}

struct Variant {
    range: Range<usize>,
    attributes: Attributes,
    path: PathBuf,
}

type Groups = BTreeMap<String, Vec<PathBuf>>;

fn parse_master(
    root: &Path,
    text: &str,
    stop: &Stop,
) -> Result<(Vec<Variant>, Groups), ConversionError> {
    let mut variants = Vec::new();
    let mut groups = Groups::new();
    let mut pending = None;
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        check(stop)?;
        let line = raw.trim_end_matches(['\r', '\n']);
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attributes = hls::attributes(value)?;
            if attributes.get("TYPE") == Some("AUDIO") {
                let id = attributes.get("GROUP-ID").ok_or(INVALID)?;
                let paths = groups.entry(id.to_owned()).or_default();
                if let Some(uri) = attributes.get("URI") {
                    paths.push(hls::local_resource(root, uri)?);
                }
                if groups.len() > 64 {
                    return Err(INVALID);
                }
            }
        } else if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let range = offset + "#EXT-X-STREAM-INF:".len()..offset + line.len();
            if pending.replace((range, hls::attributes(value)?)).is_some() {
                return Err(INVALID);
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            let (range, attributes) = pending.take().ok_or(INVALID)?;
            variants.push(Variant {
                range,
                attributes,
                path: hls::local_resource(root, line)?,
            });
            if variants.len() > 128 {
                return Err(INVALID);
            }
        }
        offset += raw.len();
    }
    if pending.is_some() || variants.is_empty() {
        return Err(INVALID);
    }
    Ok((variants, groups))
}

fn combined(
    variant: &Variant,
    groups: &Groups,
    measured: &BTreeMap<PathBuf, Rates>,
) -> Result<Rates, ConversionError> {
    let mut rates = *measured.get(&variant.path).ok_or(INVALID)?;
    if let Some(id) = variant.attributes.get("AUDIO") {
        let group = groups.get(id).ok_or(INVALID)?;
        let mut audio = Rates {
            peak: 0,
            average: 0,
        };
        for path in group {
            let measured = measured.get(path).ok_or(INVALID)?;
            audio.peak = audio.peak.max(measured.peak);
            audio.average = audio.average.max(measured.average);
        }
        if group.contains(&variant.path) {
            // An audio-only variant can name the same audio rendition.
            rates.peak = rates.peak.max(audio.peak);
            rates.average = rates.average.max(audio.average);
        } else {
            rates.peak = rates.peak.checked_add(audio.peak).ok_or(INVALID)?;
            rates.average = rates.average.checked_add(audio.average).ok_or(INVALID)?;
        }
    }
    Ok(rates)
}

fn numeric_range(value: &str, name: &str) -> Result<Option<Range<usize>>, ConversionError> {
    let mut quoted = false;
    let mut start = 0;
    for (index, byte) in value.bytes().chain(std::iter::once(b',')).enumerate() {
        if byte == b'"' {
            quoted = !quoted;
        }
        if byte == b',' && !quoted {
            let field = &value[start..index];
            let (key, number) = field.split_once('=').ok_or(INVALID)?;
            if key == name {
                number
                    .parse::<u64>()
                    .ok()
                    .filter(|number| *number > 0)
                    .ok_or(INVALID)?;
                return Ok(Some(start + key.len() + 1..index));
            }
            start = index + 1;
        }
    }
    Ok(None)
}

pub(super) fn rewrite(
    root: &Path,
    text: &str,
    measured: &BTreeMap<PathBuf, Rates>,
    stop: &Stop,
) -> Result<String, ConversionError> {
    let root = root.canonicalize().map_err(|_| INVALID)?;
    let (variants, groups) = parse_master(&root, text, stop)?;
    let mut changes = Vec::new();
    for variant in variants {
        check(stop)?;
        let rates = combined(&variant, &groups, measured)?;
        let attributes = text.get(variant.range.clone()).ok_or(INVALID)?;
        for (name, rate) in [
            ("BANDWIDTH", rates.peak),
            ("AVERAGE-BANDWIDTH", rates.average),
        ] {
            if let Some(range) = numeric_range(attributes, name)? {
                changes.push((
                    variant.range.start + range.start..variant.range.start + range.end,
                    rate.to_string(),
                ));
            } else if name == "AVERAGE-BANDWIDTH" {
                changes.push((
                    variant.range.end..variant.range.end,
                    format!(",{name}={rate}"),
                ));
            } else {
                return Err(INVALID);
            }
        }
    }
    changes.sort_by_key(|change| std::cmp::Reverse(change.0.start));
    let mut result = text.to_owned();
    for (range, value) in changes {
        result.replace_range(range, &value);
    }
    if result.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(INVALID);
    }
    Ok(result)
}
