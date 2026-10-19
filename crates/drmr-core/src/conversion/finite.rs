//! Exact finite MP4 presentation windows, independent of fragment-demux padding support.
use super::{ConversionError, audio};
use crate::{
    concurrency::Stop,
    mp4::{self, Mp4Box},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
mod fragment;
pub(super) mod manifest;
const BAD: ConversionError = ConversionError("presentation-window-not-supported");
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Window {
    pub start: u64,
    pub duration: u64,
    pub scale: u32,
    pub audio: bool,
}
impl Window {
    pub fn equivalent(self, other: Self) -> bool {
        self.audio == other.audio
            && u128::from(self.start) * u128::from(other.scale)
                == u128::from(other.start) * u128::from(self.scale)
            && u128::from(self.duration) * u128::from(other.scale)
                == u128::from(other.duration) * u128::from(self.scale)
    }
    pub fn samples(self, rate: u32) -> Result<(u64, u64), ConversionError> {
        let start = rescale(self.start, self.scale, rate)?;
        Ok((
            start,
            start
                .checked_add(rescale(self.duration, self.scale, rate)?)
                .ok_or(BAD)?,
        ))
    }
}
fn rescale(value: u64, from: u32, to: u32) -> Result<u64, ConversionError> {
    let n = u128::from(value) * u128::from(to);
    if from == 0 || to == 0 || !n.is_multiple_of(u128::from(from)) {
        return Err(BAD);
    }
    u64::try_from(n / u128::from(from)).map_err(|_| BAD)
}
fn one(node: &Mp4Box, kind: [u8; 4]) -> Result<&Mp4Box, ConversionError> {
    let mut it = node.children.iter().filter(|n| n.kind() == kind);
    let n = it.next().ok_or(BAD)?;
    if it.next().is_some() {
        return Err(BAD);
    }
    Ok(n)
}
fn integer(bytes: &[u8], at: usize) -> Result<u32, ConversionError> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
fn scale(node: &Mp4Box, data: &[u8]) -> Result<u32, ConversionError> {
    let p = node.header.payload(data);
    let at = match p.first() {
        Some(0) => 12,
        Some(1) => 20,
        _ => return Err(BAD),
    };
    let n = integer(p, at)?;
    if n == 0 {
        return Err(BAD);
    }
    Ok(n)
}
fn edit(track: &Mp4Box, data: &[u8]) -> Result<Option<(u64, u64)>, ConversionError> {
    let mut it = track.children.iter().filter(|n| n.kind() == *b"edts");
    let Some(edts) = it.next() else {
        return Ok(None);
    };
    if it.next().is_some() || edts.children.len() != 1 {
        return Err(BAD);
    }
    let p = one(edts, *b"elst")?.header.payload(data);
    let w = match p.first() {
        Some(0) => 4,
        Some(1) => 8,
        _ => return Err(BAD),
    };
    if p.get(1..4) != Some(&[0, 0, 0])
        || integer(p, 4)? != 1
        || p.get(8 + w * 2..) != Some(&[0, 1, 0, 0])
    {
        return Err(BAD);
    }
    let (duration, start) = if w == 4 {
        (
            u64::from(integer(p, 8)?),
            i64::from(i32::from_be_bytes(p[12..16].try_into().unwrap())),
        )
    } else {
        (
            u64::from_be_bytes(p[8..16].try_into().unwrap()),
            i64::from_be_bytes(p[16..24].try_into().unwrap()),
        )
    };
    Ok(Some((duration, u64::try_from(start).map_err(|_| BAD)?)))
}
pub(super) fn window(path: &Path, index: usize) -> Result<Option<Window>, ConversionError> {
    let movie = audio::movie(path)?;
    let nodes = mp4::parse_boxes(&movie.bytes).map_err(|_| BAD)?;
    let moov = nodes.first().filter(|n| n.kind() == *b"moov").ok_or(BAD)?;
    let track = moov
        .children
        .iter()
        .filter(|n| n.kind() == *b"trak")
        .nth(index)
        .ok_or(BAD)?;
    let (duration, start) = match edit(track, &movie.bytes)? {
        Some(value) => value,
        None if !movie.fragmented => {
            let p = one(track, *b"tkhd")?.header.payload(&movie.bytes);
            let d = match p.first() {
                Some(0) => u64::from(integer(p, 20)?),
                Some(1) => {
                    u64::from_be_bytes(p.get(28..36).ok_or(BAD)?.try_into().map_err(|_| BAD)?)
                }
                _ => return Err(BAD),
            };
            (d, 0)
        }
        None => return Ok(None),
    };
    if duration == 0 {
        return Ok(None);
    }
    let mdia = one(track, *b"mdia")?;
    let track_scale = scale(one(mdia, *b"mdhd")?, &movie.bytes)?;
    let handler = one(mdia, *b"hdlr")?
        .header
        .payload(&movie.bytes)
        .get(8..12)
        .ok_or(BAD)?;
    if !matches!(handler, b"soun" | b"vide") {
        return Err(BAD);
    }
    Ok(Some(Window {
        start,
        duration: rescale(
            duration,
            scale(one(moov, *b"mvhd")?, &movie.bytes)?,
            track_scale,
        )?,
        scale: track_scale,
        audio: handler == b"soun",
    }))
}
pub(super) fn has_audio(path: &Path) -> Result<bool, ConversionError> {
    let movie = audio::movie(path)?;
    let nodes = mp4::parse_boxes(&movie.bytes).map_err(|_| BAD)?;
    let moov = nodes.first().ok_or(BAD)?;
    for track in moov.children.iter().filter(|n| n.kind() == *b"trak") {
        let mdia = one(track, *b"mdia")?;
        if one(mdia, *b"hdlr")?.header.payload(&movie.bytes).get(8..12) == Some(b"soun")
            && edit(track, &movie.bytes)?.is_some_and(|(duration, _)| duration > 0)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn boxed(kind: [u8; 4], payload: &[u8]) -> Result<Vec<u8>, ConversionError> {
    let size = u32::try_from(payload.len().checked_add(8).ok_or(BAD)?).map_err(|_| BAD)?;
    let mut out = Vec::with_capacity(size as usize);
    out.extend(size.to_be_bytes());
    out.extend(kind);
    out.extend(payload);
    Ok(out)
}
fn rewritten(data: &[u8], node: &Mp4Box, elst: &[u8]) -> Result<Vec<u8>, ConversionError> {
    if !matches!(&node.kind(), b"moov" | b"trak") {
        return Ok(node.header.bytes(data).to_vec());
    }
    let mut payload = Vec::new();
    for child in &node.children {
        if child.kind() != *b"edts" {
            payload.extend(rewritten(data, child, elst)?);
        }
    }
    if node.kind() == *b"trak" {
        payload.extend(elst);
    }
    boxed(node.kind(), &payload)
}
/// Restore only the actual source's single window. Movie/track header durations remain zero in fMP4.
pub(super) fn restore(
    source: &Path,
    index: usize,
    result: &Path,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let Some(window) = window(source, index)? else {
        return Ok(());
    };
    let movie = audio::movie(result)?;
    let nodes = mp4::parse_boxes(&movie.bytes).map_err(|_| BAD)?;
    let moov = nodes.first().ok_or(BAD)?;
    let tracks: Vec<_> = moov
        .children
        .iter()
        .filter(|n| n.kind() == *b"trak")
        .collect();
    let [track] = tracks.as_slice() else {
        return Err(BAD);
    };
    one(moov, *b"mvex")?;
    for node in mp4::walk(&nodes) {
        if matches!(&node.kind(), b"stco" | b"co64")
            && integer(node.header.payload(&movie.bytes), 4)? != 0
        {
            return Err(BAD);
        }
    }
    let target_scale = scale(one(one(track, *b"mdia")?, *b"mdhd")?, &movie.bytes)?;
    let start = rescale(window.start, window.scale, target_scale)?;
    if edit(track, &movie.bytes)?.map_or(start != 0, |(_, old)| old != start) {
        return Err(BAD);
    }
    let duration = rescale(
        window.duration,
        window.scale,
        scale(one(moov, *b"mvhd")?, &movie.bytes)?,
    )?;
    let mut payload = vec![1, 0, 0, 0];
    payload.extend(1u32.to_be_bytes());
    payload.extend(duration.to_be_bytes());
    payload.extend(start.to_be_bytes());
    payload.extend([0, 1, 0, 0]);
    let elst = boxed(*b"edts", &boxed(*b"elst", &payload)?)?;
    let replacement = rewritten(&movie.bytes, moov, &elst)?;
    rewrite_file(result, &replacement, stop)?;
    if !window.equivalent(self::window(result, 0)?.ok_or(BAD)?) {
        return Err(BAD);
    }
    Ok(())
}
fn copy_n(
    input: &mut File,
    output: &mut File,
    mut count: u64,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let mut buf = [0; 8192];
    while count > 0 {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let n = usize::try_from(count.min(buf.len() as u64)).map_err(|_| BAD)?;
        input.read_exact(&mut buf[..n]).map_err(|_| BAD)?;
        output.write_all(&buf[..n]).map_err(|_| BAD)?;
        count -= n as u64;
    }
    Ok(())
}
fn generic_ftyp(data: &[u8]) -> Result<Vec<u8>, ConversionError> {
    if data.len() < 16
        || data.len() > 1024 * 1024
        || !(data.len() - 16).is_multiple_of(4)
        || data.get(4..8) != Some(b"ftyp")
    {
        return Err(BAD);
    }
    let mut payload = data[8..16].to_vec();
    if payload[..4].starts_with(b"cmf") {
        payload[..4].copy_from_slice(b"isom");
    }
    for brand in data[16..].as_chunks::<4>().0 {
        if !brand.starts_with(b"cmf") {
            payload.extend(brand);
        }
    }
    boxed(*b"ftyp", &payload)
}
fn rewrite_file(path: &Path, moov: &[u8], stop: &Stop) -> Result<(), ConversionError> {
    let stage = super::files::Stage::new(path.parent())?;
    let temporary = stage.path().join("window.mp4");
    let mut out = super::files::create_new(&temporary)?;
    let mut input = super::files::regular_input(path)?;
    let length = input.metadata().map_err(|_| BAD)?.len();
    let mut at = 0;
    for _ in 0..100_000 {
        if at == length {
            break;
        }
        let mut head = [0; 8];
        input.read_exact(&mut head).map_err(|_| BAD)?;
        let mut size = u64::from(u32::from_be_bytes(head[..4].try_into().unwrap()));
        let kind = &head[4..];
        let mut header = head.to_vec();
        if size == 1 {
            let mut wide = [0; 8];
            input.read_exact(&mut wide).map_err(|_| BAD)?;
            size = u64::from_be_bytes(wide);
            header.extend(wide);
        }
        if size == 0 {
            size = length - at;
        }
        if size < header.len() as u64 || size > length - at {
            return Err(BAD);
        }
        if kind == b"moov" {
            out.write_all(moov).map_err(|_| BAD)?;
            input.seek(SeekFrom::Start(at + size)).map_err(|_| BAD)?;
        } else if kind == b"ftyp" || kind == b"moof" {
            if size > 16 * 1024 * 1024 {
                return Err(BAD);
            }
            let mut bytes = header.clone();
            bytes.resize(usize::try_from(size).map_err(|_| BAD)?, 0);
            input
                .read_exact(&mut bytes[header.len()..])
                .map_err(|_| BAD)?;
            if kind == b"ftyp" {
                out.write_all(&generic_ftyp(&bytes)?).map_err(|_| BAD)?;
            } else {
                fragment::validate(&bytes)?;
                out.write_all(&bytes).map_err(|_| BAD)?;
            }
        } else if matches!(kind, b"sidx" | b"mfra" | b"ssix") {
            input.seek(SeekFrom::Start(at + size)).map_err(|_| BAD)?;
        } else {
            out.write_all(&header).map_err(|_| BAD)?;
            copy_n(&mut input, &mut out, size - header.len() as u64, stop)?;
        }
        at += size;
    }
    if at != length {
        return Err(BAD);
    }
    out.sync_all().map_err(|_| BAD)?;
    drop(out);
    drop(input);
    std::fs::rename(&temporary, path).map_err(|_| BAD)?;
    Ok(())
}

impl super::Converter {
    pub(super) fn finite_mp4(
        &self,
        source: &Path,
        destination: &Path,
        input: &crate::inspection::Inspection,
        keys: &super::InputKeys,
        output: Option<&super::OutputKey>,
        target: crate::inspection::TargetProtection,
    ) -> Result<Option<super::ConversionReport>, ConversionError> {
        use crate::inspection::{Protection, TargetProtection};
        let finite = has_audio(source)? && !audio::movie(source)?.fragmented;
        if finite {
            if !super::progressive::validate_input(source, input, keys)? {
                return Err(ConversionError("progressive-mp4-profile-not-supported"));
            }
            self.progressive_codecs(
                source,
                input,
                (input.protection != Protection::Clear)
                    .then_some(super::keys::ProbeKeys::All(keys)),
            )?;
        }
        if target == TargetProtection::Clear {
            return self.progressive_mp4(source, destination, input, keys);
        }
        if finite && matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs) {
            return self
                .finite_encrypted_mp4(
                    source,
                    destination,
                    input,
                    keys,
                    output.ok_or(ConversionError("output-key-required"))?,
                    target,
                )
                .map(Some);
        }
        Ok(None)
    }

    pub(super) fn finite_encrypted_mp4(
        &self,
        source: &Path,
        output: &Path,
        input: &crate::inspection::Inspection,
        keys: &super::InputKeys,
        key: &super::OutputKey,
        target: crate::inspection::TargetProtection,
    ) -> Result<super::ConversionReport, ConversionError> {
        self.progressive_codecs(
            source,
            input,
            (input.protection != crate::inspection::Protection::Clear)
                .then_some(super::keys::ProbeKeys::All(keys)),
        )?;
        let stage = super::workspace()?;
        let mut parts = Vec::new();
        let clear_original = stage.path().join("whole-clear.mp4");
        let packaging_source = if input.protection == crate::inspection::Protection::Clear {
            source
        } else {
            self.clear_finite(source, &clear_original, input, keys)?;
            &clear_original
        };

        for index in 0..input.tracks.len() {
            let dir = stage.path().join(format!("track-{index}"));
            super::files::create_dir(&dir)?;
            let part = dir.join("fragment.mp4");
            let clear = dir.join("clear.mp4");
            self.backend
                .package_stream(packaging_source, index, &clear, None, None)?;
            self.backend
                .package_stream(&clear, 0, &part, None, Some((key, target)))?;
            parts.push(part);
        }
        let candidate = stage.path().join("result.mp4");
        fragment::combine(&parts, source, &candidate, &self.backend.stop)?;
        let result_keys = super::InputKeys::new(vec![super::ContentKey::from_hex(
            &key.0.id_hex(),
            &key.0.key_hex(),
        )?])?;
        let empty = super::HlsInputKeys::default();
        let verification = self.verify(super::VerificationRequest {
            source: super::VerificationSource {
                input: source,
                input_keys: keys,
                hls_keys: &empty,
            },
            output: super::VerificationSource {
                input: &candidate,
                input_keys: &result_keys,
                hls_keys: &empty,
            },
        })?;
        if !verification.passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        let actual = crate::inspection::inspect(&candidate).map_err(|_| BAD)?;
        if actual.tracks.len() != input.tracks.len()
            || actual
                .tracks
                .iter()
                .any(|t| t.key_ids != [key.0.id_hex()] || t.protection.as_str() != target.as_str())
        {
            return Err(ConversionError("output-protection-mismatch"));
        }
        self.publish(&candidate, output)?;
        Ok(super::ConversionReport {
            schema_version: 1,
            input: input.clone(),
            output: actual,
            verification,
            processing: Some(crate::inspection::ActualProcessing::new(
                crate::inspection::ProcessingRouteId::FragmentedMp4,
                crate::inspection::InputPreparation::CompleteFile,
            )),
        })
    }
}

/// Streaming evidence for every decoded sample and the independently declared presentation.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Decoded {
    pub raw: String,
    pub presented: String,
    pub raw_samples: u64,
    pub presented_samples: u64,
}
pub(super) struct Pcm {
    raw: sha2::Sha256,
    presented: sha2::Sha256,
    bytes: u64,
    start: u64,
    end: u64,
    width: u64,
}
impl Pcm {
    pub fn new(window: Window, rate: u32, channels: u32) -> Result<Self, ConversionError> {
        use sha2::Digest as _;
        let (start, end) = window.samples(rate)?;
        let width = u64::from(channels)
            .checked_mul(4)
            .filter(|v| *v > 0 && *v <= 512)
            .ok_or(BAD)?;
        Ok(Self {
            raw: sha2::Sha256::new(),
            presented: sha2::Sha256::new(),
            bytes: 0,
            start: start.checked_mul(width).ok_or(BAD)?,
            end: end.checked_mul(width).ok_or(BAD)?,
            width,
        })
    }
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), ()> {
        use sha2::Digest as _;
        let end = self
            .bytes
            .checked_add(u64::try_from(chunk.len()).map_err(|_| ())?)
            .ok_or(())?;
        self.raw.update(chunk);
        let a = self.start.max(self.bytes);
        let b = self.end.min(end);
        if a < b {
            self.presented.update(
                &chunk[usize::try_from(a - self.bytes).map_err(|_| ())?
                    ..usize::try_from(b - self.bytes).map_err(|_| ())?],
            );
        }
        self.bytes = end;
        Ok(())
    }
    pub fn finish(self) -> Result<Decoded, ConversionError> {
        use sha2::Digest as _;
        if !self.bytes.is_multiple_of(self.width) || self.bytes < self.end {
            return Err(ConversionError("decoded-presentation-range-unavailable"));
        }
        Ok(Decoded {
            raw: hex::encode(self.raw.finalize()),
            presented: hex::encode(self.presented.finalize()),
            raw_samples: self.bytes / self.width,
            presented_samples: (self.end - self.start) / self.width,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest as _;
    #[test]
    fn modified_fragments_do_not_advertise_an_unqualified_cmaf_track() {
        let bytes = boxed(*b"ftyp", b"cmfc\0\0\0\0isomcmfccmfscmf2dash").unwrap();
        let actual = generic_ftyp(&bytes).unwrap();
        assert_eq!(&actual[8..12], b"isom");
        assert_eq!(&actual[16..], b"isomdash");
        assert!(generic_ftyp(&bytes[..bytes.len() - 1]).is_err());
    }
    #[test]
    fn streamed_own_window_counts_every_raw_sample_and_refuses_short_or_partial_frames() {
        let window = Window {
            start: 2,
            duration: 3,
            scale: 8,
            audio: true,
        };
        let mut p = Pcm::new(window, 8, 2).unwrap();
        for chunk in (0u8..56).collect::<Vec<_>>().chunks(3) {
            p.push(chunk).unwrap();
        }
        let actual = p.finish().unwrap();
        assert_eq!(
            actual.raw,
            hex::encode(sha2::Sha256::digest((0u8..56).collect::<Vec<_>>()))
        );
        assert_eq!(
            actual.presented,
            hex::encode(sha2::Sha256::digest((16u8..40).collect::<Vec<_>>()))
        );
        assert_eq!((actual.raw_samples, actual.presented_samples), (7, 3));
        for count in [39, 41] {
            let mut p = Pcm::new(window, 8, 2).unwrap();
            p.push(&vec![0; count]).unwrap();
            assert!(p.finish().is_err());
        }
        assert!(window.samples(3).is_err());
    }
}
