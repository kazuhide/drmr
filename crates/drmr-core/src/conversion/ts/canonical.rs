//! Canonical access units retain every byte except validated container framing.
use super::super::ConversionError;
use crate::sha256::Sha256;

const BAD: ConversionError = ConversionError("invalid-ts-access-unit-evidence");
const CONFIG: ConversionError = ConversionError("ts-codec-configuration-not-supported");
const MAX_PACKET: usize = 8 * 1024 * 1024;
pub(super) const MEMORY: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) enum Config {
    Video { sps: Vec<u8>, pps: Vec<u8> },
    Audio { channels: u8, sample_rate: u32 },
}
impl Config {
    pub(super) fn video_access_unit(data: &[u8]) -> Result<Self, ConversionError> {
        let units = annex(data)?;
        let parameters: Vec<_> = units
            .iter()
            .filter(|n| matches!(n[0] & 31, 7 | 8))
            .collect();
        if parameters.len() != 2 {
            return Err(CONFIG);
        }
        let mut bytes = Vec::new();
        for parameter in parameters {
            bytes.extend_from_slice(&[0, 0, 0, 1]);
            bytes.extend_from_slice(parameter);
        }
        Self::video(&bytes, true)
    }
    pub fn video(data: &[u8], transport: bool) -> Result<Self, ConversionError> {
        if !transport {
            let (configuration, _) = super::avc::Configuration::from_avcc(data)?;
            let (sps, pps) = configuration.parameter_sets();
            return Ok(Self::Video {
                sps: sps.to_vec(),
                pps: pps.to_vec(),
            });
        }
        let parameters = annex(data)?;
        if parameters.len() != 2
            || parameters[0]
                .first()
                .is_none_or(|n| n & 0x9f != 7 || n & 0x60 == 0)
            || parameters[1]
                .first()
                .is_none_or(|n| n & 0x9f != 8 || n & 0x60 == 0)
            || parameters.iter().any(|p| p.len() > 65535)
        {
            return Err(CONFIG);
        }
        Ok(Self::Video {
            sps: parameters[0].to_vec(),
            pps: parameters[1].to_vec(),
        })
    }
    pub fn audio(data: &[u8]) -> Result<Self, ConversionError> {
        if data.len() != 2 || data[0] >> 3 != 2 || data[1] & 7 != 0 {
            return Err(CONFIG);
        }
        let index = ((data[0] & 7) << 1) | (data[1] >> 7);
        let sample_rate = Self::audio_rate(index)?;
        let channels = (data[1] >> 3) & 15;
        if !(1..=7).contains(&channels) {
            return Err(CONFIG);
        }
        Ok(Self::Audio {
            channels,
            sample_rate,
        })
    }
    fn audio_rate(index: u8) -> Result<u32, ConversionError> {
        match index {
            3 => Ok(48_000),
            4 => Ok(44_100),
            _ => Err(ConversionError("ts-aac-sample-rate-not-supported")),
        }
    }
    pub(super) fn audio_adts(data: &[u8]) -> Result<Self, ConversionError> {
        let head = data.get(..7).ok_or(CONFIG)?;
        if head[0] != 0xff || head[1] != 0xf1 || head[2] >> 6 != 1 || head[6] & 3 != 0 {
            return Err(CONFIG);
        }
        let sample_rate = Self::audio_rate((head[2] >> 2) & 15)?;
        let channels = ((head[2] & 1) << 2) | (head[3] >> 6);
        if !(1..=7).contains(&channels) {
            return Err(CONFIG);
        }
        Ok(Self::Audio {
            channels,
            sample_rate,
        })
    }
    pub(in crate::conversion) fn units(
        &self,
        data: &[u8],
        transport: bool,
        hash: &mut Sha256,
    ) -> Result<(), ConversionError> {
        self.units_with_width(data, transport, 4, hash)
    }
    pub(in crate::conversion) fn units_with_width(
        &self,
        data: &[u8],
        transport: bool,
        nal_length_size: u8,
        hash: &mut Sha256,
    ) -> Result<(), ConversionError> {
        match self {
            Self::Video { .. } => {
                let units = if transport {
                    annex(data)?
                } else {
                    if !matches!(nal_length_size, 1 | 2 | 4) {
                        return Err(CONFIG);
                    }
                    let mut units = Vec::new();
                    let mut at = 0;
                    while at < data.len() {
                        units.push(length_unit(data, &mut at, usize::from(nal_length_size))?);
                        if units.len() > 4096 {
                            return Err(BAD);
                        }
                    }
                    units
                };
                self.video_units(&units, hash)?;
            }
            Self::Audio { .. } => {
                let raw = if transport {
                    if Self::audio_adts(data)? != *self {
                        return Err(CONFIG);
                    }
                    let length = (usize::from(data[3] & 3) << 11)
                        | (usize::from(data[4]) << 3)
                        | usize::from(data[5] >> 5);
                    if length != data.len() {
                        return Err(BAD);
                    }
                    &data[7..]
                } else {
                    data
                };
                if raw.is_empty() {
                    return Err(BAD);
                }
                hash.update(u64::try_from(raw.len()).map_err(|_| BAD)?.to_be_bytes());
                hash.update(raw);
            }
        }
        Ok(())
    }
    fn video_units(&self, units: &[&[u8]], hash: &mut Sha256) -> Result<(), ConversionError> {
        let Self::Video { sps, pps } = self else {
            return Err(CONFIG);
        };
        let mut count = 0_u64;
        let mut vcl = false;
        for (at, unit) in units.iter().enumerate() {
            let first = *unit.first().ok_or(BAD)?;
            let kind = first & 31;
            if first & 0x80 != 0
                || (matches!(kind, 5 | 7 | 8) && first & 0x60 == 0)
                || (matches!(kind, 6 | 9 | 10 | 11 | 12) && first & 0x60 != 0)
            {
                return Err(BAD);
            }
            match kind {
                7 if *unit == sps.as_slice() => {}
                8 if *unit == pps.as_slice() => {}
                7 | 8 => return Err(CONFIG),
                9 if at == 0 && unit.len() == 2 && unit[1] & 31 == 16 => {}
                9 => return Err(BAD),
                1 | 5 | 6 | 10 | 11 | 12 => {
                    vcl |= matches!(kind, 1 | 5);
                    count += 1;
                    hash.update(u64::try_from(unit.len()).map_err(|_| BAD)?.to_be_bytes());
                    hash.update(unit);
                }
                _ => return Err(ConversionError("ts-nal-type-not-supported")),
            }
        }
        if !vcl {
            return Err(BAD);
        }
        hash.update(count.to_be_bytes());
        Ok(())
    }
}
fn length_unit<'a>(
    data: &'a [u8],
    at: &mut usize,
    width: usize,
) -> Result<&'a [u8], ConversionError> {
    let mut length = 0usize;
    let prefix_end = at.checked_add(width).ok_or(BAD)?;
    for &b in data.get(*at..prefix_end).ok_or(BAD)? {
        length = length
            .checked_mul(256)
            .and_then(|n| n.checked_add(usize::from(b)))
            .ok_or(BAD)?;
    }
    *at = prefix_end;
    if length == 0 {
        return Err(BAD);
    }
    let end = at.checked_add(length).ok_or(BAD)?;
    let unit = data.get(*at..end).ok_or(BAD)?;
    *at = end;
    Ok(unit)
}
pub(in crate::conversion) fn annex(data: &[u8]) -> Result<Vec<&[u8]>, ConversionError> {
    let mut marks = Vec::new();
    let mut at = 0;
    while at + 3 <= data.len() {
        let width = if data.get(at..at + 4) == Some(&[0, 0, 0, 1]) {
            4
        } else if data.get(at..at + 3) == Some(&[0, 0, 1]) {
            3
        } else {
            at += 1;
            continue;
        };
        marks.push((at, at + width));
        at += width;
        if marks.len() > 4096 {
            return Err(BAD);
        }
    }
    if marks.first().is_none_or(|m| m.0 != 0) {
        return Err(BAD);
    }
    let mut units = Vec::new();
    for (index, &(_, begin)) in marks.iter().enumerate() {
        let mut end = marks.get(index + 1).map_or(data.len(), |m| m.0);
        // Annex B trailing_zero_8bits are byte-stream framing, outside the NAL unit.
        while end > begin && data[end - 1] == 0 {
            end -= 1;
        }
        if begin == end {
            return Err(BAD);
        }
        units.push(&data[begin..end]);
    }
    Ok(units)
}

#[cfg(test)]
#[path = "canonical/annex_tests.rs"]
mod annex_tests;

/// Decode `FFprobe`'s hex display, never its non-authoritative ASCII rendering.
pub(super) fn dump(data: &[u8], escaped: bool) -> Result<Vec<u8>, ConversionError> {
    let mut result = Vec::new();
    let mut line = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let mut byte = data[at];
        at += 1;
        if escaped && byte == b'\\' {
            byte = *data.get(at).ok_or(BAD)?;
            at += 1;
            byte = match byte {
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'f' => 12,
                b'b' => 8,
                b'\\' | b'|' | b'\'' => byte,
                _ => return Err(BAD),
            };
        }
        if byte == b'\n' {
            if !line.is_empty() {
                hex_line(&line, &mut result)?;
                line.clear();
            }
        } else {
            line.push(byte);
            if line.len() > 128 {
                return Err(BAD);
            }
        }
    }
    if !line.is_empty() {
        return Err(BAD);
    }
    Ok(result)
}
fn hex_line(line: &[u8], output: &mut Vec<u8>) -> Result<(), ConversionError> {
    let text = std::str::from_utf8(line).map_err(|_| BAD)?;
    let (offset, data) = text.split_once(": ").ok_or(BAD)?;
    if offset.len() != 8 || usize::from_str_radix(offset, 16).ok() != Some(output.len()) {
        return Err(BAD);
    }
    let (hex, _ascii) = data.split_once("  ").ok_or(BAD)?;
    let digits: Vec<_> = hex.bytes().filter(|b| *b != b' ').collect();
    if digits.is_empty() || digits.len() > 32 || digits.len() % 2 != 0 {
        return Err(BAD);
    }
    for pair in digits.as_chunks::<2>().0 {
        let high = (pair[0] as char).to_digit(16).ok_or(BAD)?;
        let low = (pair[1] as char).to_digit(16).ok_or(BAD)?;
        output.push(u8::try_from((high << 4) | low).map_err(|_| BAD)?);
    }
    if output.len() > MAX_PACKET {
        return Err(ConversionError("ts-access-unit-size-limit"));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Evidence {
    pub(in crate::conversion) count: u64,
    digest: [u8; 32],
    clocks: [u8; 32],
    pub first_pts: i64,
    pub end_pts: i64,
}
impl Evidence {
    pub(in crate::conversion) fn payload_digest(&self) -> [u8; 32] {
        self.digest
    }
    pub(in crate::conversion) fn clock_digest(&self) -> [u8; 32] {
        self.clocks
    }
    pub fn payload_equal(&self, other: &Self) -> bool {
        self.count == other.count && self.digest == other.digest
    }
    pub fn timing_equal(&self, other: &Self) -> bool {
        self.count == other.count
            && self.clocks == other.clocks
            && self.first_pts == other.first_pts
            && self.end_pts == other.end_pts
    }
}
/// Derived directly from decrypted original PES bytes, before transport repacking.
pub(in crate::conversion) struct Native {
    pub(in crate::conversion) config: Config,
    pub(in crate::conversion) count: u64,
    hash: Sha256,
    clocks: Sha256,
    validation: Result<Validation, ConversionError>,
    whole: Span,
    current_segment: Option<Span>,
    segments: Vec<Span>,
    failed: Option<ConversionError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) enum TrackMetadata {
    Video(super::avc::Metadata),
    Audio {
        sample_rate: u32,
        channel_config: u8,
        channels: u8,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Timing {
    pub count: u64,
    /// PTS of the first sample in decode order, independent of presentation order.
    pub first_sample_pts: i64,
    /// Minimum PTS across the span.
    pub first_pts: i64,
    pub max_pts: i64,
    pub end_pts: i64,
    pub first_dts: i64,
    pub last_dts: i64,
    pub terminal_duration: i64,
    pub nominal_duration_numerator: u64,
    pub nominal_duration_denominator: u32,
    pub requires_manifest_duration: bool,
    pub coded_samples: Option<u64>,
}
#[derive(Clone, Copy, Default)]
struct Span {
    count: u64,
    first_sample_pts: i64,
    min_pts: i64,
    max_pts: i64,
    first_dts: i64,
    last_dts: i64,
}
impl Span {
    fn record(&mut self, pts: i64, dts: i64) -> Result<(), ConversionError> {
        if self.count == 0 {
            self.first_sample_pts = pts;
            self.min_pts = pts;
            self.max_pts = pts;
            self.first_dts = dts;
        } else {
            self.min_pts = self.min_pts.min(pts);
            self.max_pts = self.max_pts.max(pts);
        }
        self.last_dts = dts;
        self.count = self.count.checked_add(1).ok_or(BAD)?;
        Ok(())
    }
}
enum Validation {
    Video {
        config: Box<super::avc::Configuration>,
        clock: Option<super::clock::Video>,
    },
    Audio(super::clock::Audio),
}
impl Native {
    pub(in crate::conversion) fn new(config: Config) -> Self {
        let validation = match &config {
            Config::Video { sps, pps } => {
                super::avc::Configuration::parse(sps, pps).and_then(|config| {
                    let clock = config
                        .metadata()
                        .clock
                        .map(super::clock::Video::new)
                        .transpose()?;
                    Ok(Validation::Video {
                        config: Box::new(config),
                        clock,
                    })
                })
            }
            Config::Audio { sample_rate, .. } => {
                super::clock::Audio::new(*sample_rate).map(Validation::Audio)
            }
        };
        Self {
            config,
            count: 0,
            hash: Sha256::new(),
            clocks: Sha256::new(),
            validation,
            whole: Span::default(),
            current_segment: None,
            segments: Vec::new(),
            failed: None,
        }
    }
    pub(in crate::conversion) fn record(
        &mut self,
        bytes: &[u8],
        pts: i64,
        dts: i64,
    ) -> Result<(), ConversionError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.record_checked(bytes, pts, dts);
        if let Err(error) = result {
            self.failed = Some(error);
        }
        result
    }
    fn record_checked(&mut self, bytes: &[u8], pts: i64, dts: i64) -> Result<(), ConversionError> {
        if bytes.len() > MAX_PACKET {
            return Err(ConversionError("ts-access-unit-size-limit"));
        }
        if dts < 0 || pts < dts {
            return Err(BAD);
        }
        let units = matches!(&self.config, Config::Video { .. })
            .then(|| annex(bytes))
            .transpose()?;
        self.hash.update(b"access-unit");
        if let Some(units) = &units {
            self.config.video_units(units, &mut self.hash)?;
        } else {
            self.config.units(bytes, true, &mut self.hash)?;
        }
        self.clocks.update(pts.to_be_bytes());
        self.clocks.update(dts.to_be_bytes());
        self.count = self.count.checked_add(1).ok_or(BAD)?;
        self.whole.record(pts, dts)?;
        self.current_segment
            .get_or_insert_with(Span::default)
            .record(pts, dts)?;
        // Original-byte capture and codec qualification are distinct stages.
        // Preserve the first syntax error and require it at every native
        // metadata/timing accessor; failed qualification can never yield proof.
        let validation = match &mut self.validation {
            Ok(Validation::Video { config, clock }) => config
                .access_unit(units.as_deref().ok_or(BAD)?)
                .and_then(|_| {
                    if let Some(clock) = clock {
                        clock.frame(pts, dts)?;
                    }
                    Ok(())
                }),
            Ok(Validation::Audio(clock)) => clock
                .begin(pts, dts)
                .and_then(|()| clock.frame(pts, dts))
                .map(|_| ()),
            Err(error) => Err(*error),
        };
        if let Err(error) = validation {
            self.validation = Err(error);
        }
        Ok(())
    }
    pub(in crate::conversion) fn begin_segment(&mut self) -> Result<(), ConversionError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if self.current_segment.is_some() {
            let error = ConversionError("ts-segment-evidence-unfinished");
            self.failed = Some(error);
            return Err(error);
        }
        self.current_segment = Some(Span::default());
        Ok(())
    }
    pub(in crate::conversion) fn end_segment(&mut self) -> Result<(), ConversionError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let Some(span) = self.current_segment.take().filter(|s| s.count != 0) else {
            let error = ConversionError("ts-segment-evidence-missing");
            self.failed = Some(error);
            return Err(error);
        };
        if self.segments.len() >= 4096 {
            let error = ConversionError("ts-segment-evidence-limit");
            self.failed = Some(error);
            return Err(error);
        }
        self.segments.push(span);
        Ok(())
    }
    pub(in crate::conversion) fn validate(&self) -> Result<(), ConversionError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if self.count == 0 || self.whole.count != self.count {
            return Err(BAD);
        }
        match self.validation.as_ref().map_err(|e| *e)? {
            Validation::Video {
                clock: Some(clock), ..
            } => clock.finish(),
            Validation::Video { clock: None, .. } => {
                Err(ConversionError("ts-video-duration-clock-required"))
            }
            Validation::Audio(_) => Ok(()),
        }
    }
    pub(in crate::conversion) fn metadata(&self) -> Result<TrackMetadata, ConversionError> {
        self.validate()?;
        match (&self.config, self.validation.as_ref().map_err(|e| *e)?) {
            (Config::Video { .. }, Validation::Video { config, .. }) => {
                Ok(TrackMetadata::Video(config.metadata().clone()))
            }
            (
                Config::Audio {
                    channels,
                    sample_rate,
                },
                Validation::Audio(_),
            ) => Ok(TrackMetadata::Audio {
                sample_rate: *sample_rate,
                channel_config: *channels,
                channels: if *channels == 7 { 8 } else { *channels },
            }),
            _ => Err(BAD),
        }
    }
    fn span_timing(&self, span: Span) -> Result<Timing, ConversionError> {
        if span.count == 0 {
            return Err(BAD);
        }
        let (numerator, denominator, requires_manifest_duration, coded_samples) =
            match self.metadata()? {
                TrackMetadata::Video(metadata) => {
                    let clock = metadata
                        .clock
                        .ok_or(ConversionError("ts-video-duration-clock-required"))?;
                    (
                        u64::from(clock.num_units_in_tick)
                            .checked_mul(180_000)
                            .ok_or(BAD)?,
                        clock.time_scale,
                        !clock.fixed_frame_rate,
                        None,
                    )
                }
                TrackMetadata::Audio { sample_rate, .. } => (
                    1024 * 90_000,
                    sample_rate,
                    false,
                    Some(span.count.checked_mul(1024).ok_or(BAD)?),
                ),
            };
        let terminal_duration = match self.validation.as_ref().map_err(|e| *e)? {
            Validation::Video {
                clock: Some(clock), ..
            } => clock.nominal_duration()?,
            _ => i64::try_from(numerator / u64::from(denominator)).map_err(|_| BAD)?,
        };
        let end_pts = span.max_pts.checked_add(terminal_duration).ok_or(BAD)?;
        Ok(Timing {
            count: span.count,
            first_sample_pts: span.first_sample_pts,
            first_pts: span.min_pts,
            max_pts: span.max_pts,
            end_pts,
            first_dts: span.first_dts,
            last_dts: span.last_dts,
            terminal_duration,
            nominal_duration_numerator: numerator,
            nominal_duration_denominator: denominator,
            requires_manifest_duration,
            coded_samples,
        })
    }
    pub(in crate::conversion) fn timing(&self) -> Result<Timing, ConversionError> {
        self.span_timing(self.whole)
    }
    pub(in crate::conversion) fn segment_timings(&self) -> Result<Vec<Timing>, ConversionError> {
        self.validate()?;
        if self.current_segment.is_some() {
            return Err(ConversionError("ts-segment-evidence-unfinished"));
        }
        self.segments
            .iter()
            .map(|span| self.span_timing(*span))
            .collect()
    }
    pub(in crate::conversion) fn evidence(&self) -> Result<Evidence, ConversionError> {
        let timing = self.timing()?;
        Ok(Evidence {
            count: self.count,
            digest: self.hash.clone().finalize(),
            clocks: self.clocks.clone().finalize(),
            first_pts: timing.first_pts,
            end_pts: timing.end_pts,
        })
    }
    pub(super) fn same_as(&self, other: &Self) -> bool {
        self.failed.is_none()
            && other.failed.is_none()
            && self.count != 0
            && self.config == other.config
            && self.count == other.count
            && self.hash.clone().finalize() == other.hash.clone().finalize()
            && self.clocks.clone().finalize() == other.clocks.clone().finalize()
    }
    pub(super) fn matches(&self, config: &Config, evidence: &Evidence) -> bool {
        self.failed.is_none()
            && self.count != 0
            && self.config == *config
            && self.count == evidence.count
            && self.hash.clone().finalize().as_slice() == evidence.digest
            && self.clocks.clone().finalize().as_slice() == evidence.clocks
    }
}
pub(super) struct Parser {
    index: usize,
    expected: u64,
    transport: bool,
    nal_length_size: u8,
    config: Config,
    buffer: Vec<u8>,
    hash: Sha256,
    clocks: Sha256,
    count: u64,
    next_dts: Option<i64>,
    audio_clock: Option<super::clock::Audio>,
    first_pts: Option<i64>,
    end_pts: i64,
    error: Option<ConversionError>,
}

impl Parser {
    pub fn new(
        index: usize,
        expected: u64,
        transport: bool,
        config: Config,
        nal_length_size: u8,
    ) -> Self {
        let audio_clock = if transport
            && matches!(
                config,
                Config::Audio {
                    sample_rate: 44_100,
                    ..
                }
            ) {
            Some(super::clock::Audio::new(44_100).expect("supported AAC sample rate"))
        } else {
            None
        };
        Self {
            index,
            expected,
            transport,
            nal_length_size,
            config,
            buffer: Vec::new(),
            hash: Sha256::new(),
            clocks: Sha256::new(),
            count: 0,
            next_dts: None,
            audio_clock,
            first_pts: None,
            end_pts: i64::MIN,
            error: None,
        }
    }
    pub fn error(&self) -> Option<ConversionError> {
        self.error
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ()> {
        let result = self.push_checked(bytes);
        if let Err(error) = result {
            self.error = Some(error);
            return Err(());
        }
        Ok(())
    }
    fn push_checked(&mut self, mut bytes: &[u8]) -> Result<(), ConversionError> {
        if self.buffer.capacity() == 0 {
            self.buffer.reserve_exact(40 * 1024 * 1024);
        }
        while !bytes.is_empty() {
            let end = memchr::memchr(b'\n', bytes).map_or(bytes.len(), |n| n + 1);
            if self.buffer.len() + end > 40 * 1024 * 1024 {
                return Err(ConversionError("ts-access-unit-size-limit"));
            }
            self.buffer.extend_from_slice(&bytes[..end]);
            bytes = &bytes[end..];
            if self.buffer.last() == Some(&b'\n') {
                let record = std::mem::take(&mut self.buffer);
                let line = &record[..record.len() - 1];
                self.record(line.strip_suffix(b"\r").unwrap_or(line))?;
                self.buffer = record;
                self.buffer.clear();
            }
        }
        Ok(())
    }
    fn record(&mut self, line: &[u8]) -> Result<(), ConversionError> {
        let text = std::str::from_utf8(line).map_err(|_| BAD)?;
        let (header, data) = text.split_once("|data=").ok_or(BAD)?;
        let fields: Vec<_> = header.split('|').take(6).collect();
        if fields.len() != 5 || fields[0] != "packet" {
            return Err(BAD);
        }
        let read = |at: usize, name: &str| -> Result<i64, ConversionError> {
            fields[at]
                .strip_prefix(name)
                .ok_or(BAD)?
                .parse()
                .map_err(|_| BAD)
        };
        if read(1, "stream_index=")? != i64::try_from(self.index).map_err(|_| BAD)? {
            return Err(BAD);
        }
        let pts = read(2, "pts=")?;
        let dts = read(3, "dts=")?;
        let duration = read(4, "duration=")?;
        if duration <= 0
            || dts < 0
            || pts < dts
            || (self.audio_clock.is_none() && self.next_dts.is_some_and(|next| next != dts))
        {
            return Err(ConversionError(
                "ts-clock-wrap-or-discontinuity-not-supported",
            ));
        }
        if let Config::Audio { sample_rate, .. } = self.config
            && self.transport
            && duration != 1024 * 90_000 / i64::from(sample_rate)
        {
            return Err(CONFIG);
        }
        if let Some(clock) = &mut self.audio_clock {
            clock.begin(pts, dts)?;
            clock.frame(pts, dts)?;
        }
        self.next_dts = Some(dts.checked_add(duration).ok_or(BAD)?);
        self.first_pts = Some(self.first_pts.map_or(pts, |old| old.min(pts)));
        self.end_pts = self.end_pts.max(pts.checked_add(duration).ok_or(BAD)?);
        let bytes = dump(data.strip_suffix('|').unwrap_or(data).as_bytes(), true)?;
        self.hash.update(b"access-unit");
        self.config.units_with_width(
            &bytes,
            self.transport,
            self.nal_length_size,
            &mut self.hash,
        )?;
        self.clocks.update(pts.to_be_bytes());
        self.clocks.update(dts.to_be_bytes());
        self.count = self.count.checked_add(1).ok_or(BAD)?;
        if self.count > self.expected {
            return Err(ConversionError("ts-access-unit-count-mismatch"));
        }
        Ok(())
    }
    pub fn finish(self) -> Result<Evidence, ConversionError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if !self.buffer.is_empty() || self.count != self.expected || self.count == 0 {
            return Err(ConversionError("ts-access-unit-count-mismatch"));
        }
        Ok(Evidence {
            count: self.count,
            digest: self.hash.finalize(),
            clocks: self.clocks.finalize(),
            first_pts: self.first_pts.ok_or(BAD)?,
            end_pts: self.end_pts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn video() -> Config {
        Config::Video {
            sps: vec![0x67, 0x80],
            pps: vec![0x68, 0x80],
        }
    }
    fn digest(config: &Config, data: &[u8], ts: bool) -> Result<Vec<u8>, ConversionError> {
        let mut hash = Sha256::new();
        config.units(data, ts, &mut hash)?;
        Ok(hash.finalize().to_vec())
    }
    fn avcc_configurations() -> Vec<Vec<u8>> {
        [
            "01640015ffe1001a67640015ac4c220a0cfcf808800000030080000018078b1708c001000768e843044b22c0fdf8f800",
            "016e000cffe1001a676e000ca6cd941419f9f011000003000100000300300f14299601000668ebe3cb22c0fdfafa00",
            "017a000cffe1001a677a000cb6cd941419f8dc0440000003004000000c03c50a658001000668ebe3cb22c0fefafa00",
            "01f4000cffe1001b67f4000c90d9b282833f1b80880000030008000003018078a14cb001000668ebe3c44844fffafa00",
        ]
        .map(|data| hex::decode(data).unwrap())
        .into()
    }
    #[test]
    fn avcc_canonical_configuration_preserves_high_profile_syntax_and_ignores_length_width() {
        for avcc in avcc_configurations() {
            let (strict, _) = super::super::avc::Configuration::from_avcc(&avcc).unwrap();
            let (sps, pps) = strict.parameter_sets();
            let transport = [&[0, 0, 0, 1][..], sps, &[0, 0, 1], pps].concat();
            let expected = Config::video(&transport, true).unwrap();
            for width in [1, 2, 4] {
                let mut avcc = avcc.clone();
                avcc[4] = 0xfc | (width - 1);
                assert_eq!(Config::video(&avcc, false).unwrap(), expected);
            }
        }
    }
    #[test]
    fn avcc_canonical_configuration_cannot_disagree_with_its_parameter_sets() {
        for avcc in avcc_configurations() {
            for at in [1, 2, 3, avcc.len() - 4, avcc.len() - 3, avcc.len() - 2] {
                let mut changed = avcc.clone();
                changed[at] ^= 1;
                assert!(Config::video(&changed, false).is_err(), "byte {at}");
            }
            for width in [0xf8, 0xfe] {
                let mut changed = avcc.clone();
                changed[4] = width;
                assert!(Config::video(&changed, false).is_err());
            }
            let mut changed = avcc.clone();
            changed[8] |= 0x80;
            assert!(Config::video(&changed, false).is_err());
            let mut changed = avcc.clone();
            changed.truncate(changed.len() - 1);
            assert!(Config::video(&changed, false).is_err());
        }
        let fake = [
            1, 100, 0, 31, 0xff, 0xe1, 0, 2, 0x67, 0x80, 1, 0, 2, 0x68, 0x80,
        ];
        assert!(Config::video(&fake, false).is_err());
    }
    fn canonical_video_row(data: &[u8]) -> String {
        format!(
            "packet|stream_index=0|pts=90000|dts=90000|duration=3750|data=\\n00000000: {}  ignored\\n|\n",
            hex::encode(data)
        )
    }
    #[test]
    fn streamed_video_uses_explicit_container_lengths_without_changing_identity() {
        let transport = [0, 0, 1, 6, 0x80, 0, 0, 1, 0x65, 0x80];
        for avcc in avcc_configurations() {
            let config = Config::video(&avcc, false).unwrap();
            let mut source = Parser::new(0, 1, true, config.clone(), 0);
            source
                .push(canonical_video_row(&transport).as_bytes())
                .unwrap();
            let source = source.finish().unwrap();
            for width in [1, 2, 4] {
                let mut bytes = Vec::new();
                for nal in [[6, 0x80], [0x65, 0x80]] {
                    bytes.extend_from_slice(&2_u32.to_be_bytes()[4 - usize::from(width)..]);
                    bytes.extend_from_slice(&nal);
                }
                for declared in [0, 1, 2, 3, 4, 5] {
                    let mut parser = Parser::new(0, 1, false, config.clone(), declared);
                    let result = parser.push(canonical_video_row(&bytes).as_bytes());
                    if declared == width {
                        result.unwrap();
                        let observed = parser.finish().unwrap();
                        assert!(source.payload_equal(&observed));
                        assert!(source.timing_equal(&observed));
                    } else {
                        assert!(result.is_err(), "actual {width}, declared {declared}");
                        assert!(parser.finish().is_err());
                    }
                }
                let mut parser = Parser::new(0, 1, false, config.clone(), width);
                bytes.pop();
                assert!(parser.push(canonical_video_row(&bytes).as_bytes()).is_err());
                assert!(parser.finish().is_err());
            }
        }
    }
    #[test]
    fn spans_preserve_the_first_decode_order_pts_when_bframes_present_earlier() {
        let mut whole = Span::default();
        let mut segments = [Span::default(); 2];
        for (index, pts) in [97_500, 90_000, 93_750, 101_250].into_iter().enumerate() {
            let dts = 82_500 + i64::try_from(index).unwrap() * 3750;
            whole.record(pts, dts).unwrap();
            segments[index / 2].record(pts, dts).unwrap();
        }
        assert_eq!(whole.first_sample_pts, 97_500);
        assert_eq!((whole.min_pts, whole.max_pts), (90_000, 101_250));
        assert_eq!((whole.first_dts, whole.last_dts), (82_500, 93_750));
        assert_eq!(whole.first_sample_pts - whole.first_dts, 15_000);
        assert_eq!(segments[0].first_sample_pts, 97_500);
        assert_eq!(
            (segments[0].min_pts, segments[0].first_dts),
            (90_000, 82_500)
        );
        assert_eq!(segments[1].first_sample_pts, 93_750);
        assert_eq!(
            (segments[1].min_pts, segments[1].first_dts),
            (93_750, 90_000)
        );
        assert_eq!(whole.count, 4);
        assert!(segments.iter().all(|span| span.count == 2));
    }
    #[test]
    fn capture_failure_overflow_and_segment_limits_cannot_yield_native_proof() {
        let config = Config::Audio {
            channels: 2,
            sample_rate: 48_000,
        };
        let frame = [0xff, 0xf1, 0x4c, 0x80, 1, 0x5f, 0xfc, 1, 0x34, 0x80];
        for overflow in [false, true] {
            let mut native = Native::new(config.clone());
            if overflow {
                native.count = u64::MAX;
            }
            assert!(
                native
                    .record(&frame, if overflow { 90_000 } else { -1 }, 90_000)
                    .is_err()
            );
            assert!(native.record(&frame, 90_000, 90_000).is_err());
            assert!(native.validate().is_err());
            assert!(native.metadata().is_err());
            assert!(native.timing().is_err());
            assert!(native.evidence().is_err());
        }
        let mut native = Native::new(config);
        for index in 0..4096 {
            native.begin_segment().unwrap();
            native
                .record(&frame, 90_000 + index * 1920, 90_000 + index * 1920)
                .unwrap();
            native.end_segment().unwrap();
        }
        assert_eq!(native.timing().unwrap().first_sample_pts, 90_000);
        let timings = native.segment_timings().unwrap();
        assert_eq!(timings.len(), 4096);
        for (index, timing) in timings.iter().enumerate() {
            assert_eq!(
                timing.first_sample_pts,
                90_000 + i64::try_from(index).unwrap() * 1920
            );
            assert_eq!(timing.first_sample_pts, timing.first_pts);
            assert_eq!(timing.first_sample_pts, timing.first_dts);
        }
        native.begin_segment().unwrap();
        native
            .record(&frame, 90_000 + 4096 * 1920, 90_000 + 4096 * 1920)
            .unwrap();
        assert_eq!(
            native.end_segment().unwrap_err().0,
            "ts-segment-evidence-limit"
        );
        assert!(native.segment_timings().is_err());
        assert!(native.evidence().is_err());
    }
    #[test]
    fn access_unit_identity_retains_sei_slices_order_and_every_nonframing_byte() {
        let ts = [
            0, 0, 0, 1, 9, 0xf0, 0, 0, 1, 0x67, 0x80, 0, 0, 1, 0x68, 0x80, 0, 0, 1, 6, 0x80, 0, 0,
            1, 0x65, 0x80,
        ];
        let mp4 = [0, 0, 0, 2, 6, 0x80, 0, 0, 0, 2, 0x65, 0x80];
        assert_eq!(
            digest(&video(), &ts, true).unwrap(),
            digest(&video(), &mp4, false).unwrap()
        );
        let with_aud = [&[0, 0, 0, 2, 9, 0xf0][..], &mp4].concat();
        assert_eq!(
            digest(&video(), &with_aud, false).unwrap(),
            digest(&video(), &mp4, false).unwrap()
        );
        for aud in [[0, 0, 0, 2, 9, 0], [0, 0, 0, 2, 0x69, 0xf0]] {
            assert!(digest(&video(), &[&aud[..], &mp4].concat(), false).is_err());
        }
        assert!(digest(&video(), &[&mp4[..], &with_aud].concat(), false).is_err());
        for index in [5, 11] {
            let mut changed = mp4;
            changed[index] ^= 1;
            assert_ne!(
                digest(&video(), &ts, true).unwrap(),
                digest(&video(), &changed, false).unwrap()
            );
        }
        let mut changed = ts;
        changed[11] = 0x81;
        assert_eq!(digest(&video(), &changed, true).unwrap_err(), CONFIG);
        let mut changed = ts;
        changed[5] = 0;
        assert!(digest(&video(), &changed, true).is_err());
        let mut extra = mp4.to_vec();
        extra.extend([0, 0, 0, 2, 6, 0x80]);
        assert_ne!(
            digest(&video(), &mp4, false).unwrap(),
            digest(&video(), &extra, false).unwrap()
        );
    }
    #[test]
    fn aac_framing_requires_one_lc_block_with_the_same_configuration() {
        let config = Config::audio(&[0x11, 0x90]).unwrap();
        let adts = [0xff, 0xf1, 0x4c, 0x80, 1, 0x5f, 0xfc, 0x12, 0x34, 0x80];
        let raw = &adts[7..];
        assert_eq!(
            digest(&config, &adts, true).unwrap(),
            digest(&config, raw, false).unwrap()
        );
        for (index, value) in [(1, 0xf0), (2, 0x50), (3, 0xc0), (4, 0), (6, 0xfd)] {
            let mut changed = adts;
            changed[index] = value;
            assert!(digest(&config, &changed, true).is_err());
        }
        let mut changed = adts;
        changed[8] ^= 1;
        assert_ne!(
            digest(&config, &adts, true).unwrap(),
            digest(&config, &changed, true).unwrap()
        );
        assert_eq!(
            Config::audio(&[0x12, 0x10]).unwrap(),
            Config::Audio {
                channels: 2,
                sample_rate: 44_100
            }
        );
        assert!(Config::audio(&[0x11, 0x94]).is_err());
    }
    #[test]
    fn aac_rate_is_part_of_both_decoder_configuration_and_each_adts_frame() {
        let audio = Config::audio(&[0x12, 0x10]).unwrap();
        let adts = [0xff, 0xf1, 0x50, 0x80, 1, 0x5f, 0xfc, 0x12, 0x34, 0x80];
        assert_eq!(Config::audio_adts(&adts).unwrap(), audio);
        assert_eq!(
            digest(&audio, &adts, true).unwrap(),
            digest(&audio, &adts[7..], false).unwrap()
        );
        let mut changed = adts;
        changed[2] = 0x4c;
        assert_eq!(digest(&audio, &changed, true).unwrap_err(), CONFIG);
        let mut changed = adts;
        changed[3] = 0xc0;
        assert_eq!(digest(&audio, &changed, true).unwrap_err(), CONFIG);
        let mut changed = adts;
        changed[6] |= 1;
        assert_eq!(digest(&audio, &changed, true).unwrap_err(), CONFIG);
        for asc in [[0x13, 0x10], [0x12, 0x14], [0x12, 0x00], [0x0a, 0x10]] {
            assert!(Config::audio(&asc).is_err());
        }
    }

    fn audio_row(pts: i64, duration: i64, transport: bool) -> String {
        let data = if transport {
            "fff1 5080 015f fc12 3480"
        } else {
            "1234 80"
        };
        format!(
            "packet|stream_index=0|pts={pts}|dts={pts}|duration={duration}|data=\\n00000000: {data}  ignored\\n|\n"
        )
    }
    fn audio_evidence(rows: &[(i64, i64)], transport: bool) -> Evidence {
        let mut parser = Parser::new(
            0,
            rows.len() as u64,
            transport,
            Config::audio(&[0x12, 0x10]).unwrap(),
            0,
        );
        for &(pts, duration) in rows {
            parser
                .push(audio_row(pts, duration, transport).as_bytes())
                .unwrap();
        }
        parser.finish().unwrap()
    }

    #[test]
    fn exact_frame_clocks_match_even_when_transport_packet_duration_is_truncated() {
        let source = audio_evidence(
            &[
                (900_000, 2089),
                (902_090, 2089),
                (904_180, 2089),
                (906_269, 2089),
            ],
            true,
        );
        let result = audio_evidence(
            &[
                (900_000, 2090),
                (902_090, 2090),
                (904_180, 2089),
                (906_269, 2089),
            ],
            false,
        );
        assert!(source.payload_equal(&result));
        assert!(source.timing_equal(&result));
        // Altering a single output timestamp still fails exact equality even
        // when the adjacent sample duration is changed to remain contiguous.
        let shifted = audio_evidence(
            &[
                (900_000, 2090),
                (902_090, 2090),
                (904_180, 2090),
                (906_270, 2089),
            ],
            false,
        );
        assert!(source.payload_equal(&shifted));
        assert!(!source.timing_equal(&shifted));
        let extended = audio_evidence(
            &[
                (900_000, 2090),
                (902_090, 2090),
                (904_180, 2089),
                (906_269, 2090),
            ],
            false,
        );
        assert!(!source.timing_equal(&extended));
        let mut parser = Parser::new(0, 1, true, Config::audio(&[0x12, 0x10]).unwrap(), 0);
        assert!(
            parser
                .push(audio_row(900_000, 2090, true).as_bytes())
                .is_err()
        );
    }

    #[test]
    fn hex_evidence_cannot_skip_or_repeat_offsets_or_invent_bytes() {
        assert_eq!(
            dump(
                b"\n00000000: 0102 03                                  ...\n",
                false
            )
            .unwrap(),
            [1, 2, 3]
        );
        for bad in [
            b"\n00000001: 0102 03                                  ...\n".as_slice(),
            b"\n00000000: 01gg                                     ..\n",
            b"\n00000000: 0102 03                                  ...",
            b"garbage\n",
        ] {
            assert!(dump(bad, false).is_err());
        }
    }
    fn record(index: usize, pts: &str, dts: i64, duration: i64) -> String {
        format!(
            "packet|stream_index={index}|pts={pts}|dts={dts}|duration={duration}|data=\\n00000000: 0000 0002 6580                           ....e.\\n|\n"
        )
    }
    #[test]
    fn original_access_unit_anchor_checks_every_byte_group_and_raw_clock() {
        let bytes = [0, 0, 1, 0x65, 0x80];
        let mut native = Native::new(video());
        native.record(&bytes, 90000, 90000).unwrap();
        let observed = |line: String| {
            let mut parser = Parser::new(0, 1, false, video(), 4);
            parser.push(line.as_bytes()).unwrap();
            parser.finish().unwrap()
        };
        assert!(native.matches(&video(), &observed(record(0, "90000", 90000, 3750))));
        assert!(!native.matches(&video(), &observed(record(0, "90001", 90000, 3750))));
        assert!(!native.matches(&video(), &observed(record(0, "90001", 90001, 3750))));
        assert!(!native.matches(
            &video(),
            &observed(record(0, "90000", 90000, 3750).replace("6580", "6581"))
        ));
        native.record(&bytes, 93750, 93750).unwrap();
        assert!(!native.matches(&video(), &observed(record(0, "90000", 90000, 3750))));
    }
    #[test]
    fn streamed_access_units_require_complete_counts_and_continuous_absolute_clocks() {
        let first = record(0, "90000", 90000, 3750);
        let second = record(0, "93750", 93750, 3750);
        for width in [1, 7, 4096] {
            let mut parser = Parser::new(0, 2, false, video(), 4);
            for part in format!("{first}{second}").as_bytes().chunks(width) {
                parser.push(part).unwrap();
            }
            parser.finish().unwrap();
        }
        let mut parser = Parser::new(0, 2, false, video(), 4);
        parser
            .push(format!("{first}{second}").replace('\n', "\r\n").as_bytes())
            .unwrap();
        parser.finish().unwrap();
        for bad in [
            record(1, "90000", 90000, 3750),
            record(0, "N/A", 90000, 3750),
            record(0, "90000", 90000, 0),
            record(0, "89999", 90000, 3750),
            first.trim_end().to_string(),
            format!("{first}{}", record(0, "97500", 97500, 3750)),
        ] {
            let mut parser = Parser::new(0, 1, false, video(), 4);
            let _ = parser.push(bad.as_bytes());
            assert!(parser.finish().is_err());
        }
        let mut parser = Parser::new(0, 2, false, video(), 4);
        let gap = format!("{first}{}", record(0, "97500", 97500, 3750));
        assert!(parser.push(gap.as_bytes()).is_err());
        let mut parser = Parser::new(0, 2, false, video(), 4);
        parser.push(first.as_bytes()).unwrap();
        assert!(parser.finish().is_err());
    }
}
