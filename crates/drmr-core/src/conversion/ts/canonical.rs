//! Canonical access units retain every byte except validated container framing.
use super::super::ConversionError;
use sha2::{Digest as _, Sha256};

const BAD: ConversionError = ConversionError("invalid-ts-access-unit-evidence");
const CONFIG: ConversionError = ConversionError("ts-codec-configuration-not-supported");
const MAX_PACKET: usize = 8 * 1024 * 1024;
pub(super) const MEMORY: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Config {
    Video { sps: Vec<u8>, pps: Vec<u8> },
    Audio { channels: u8 },
}
impl Config {
    pub fn video(data: &[u8], transport: bool) -> Result<Self, ConversionError> {
        let parameters = if transport {
            annex(data)?
        } else {
            if data.len() < 7 || data[0] != 1 || data[4] != 0xff || data[5] != 0xe1 {
                return Err(CONFIG);
            }
            let mut at = 6;
            let sps = length_unit(data, &mut at, 2)?;
            if data.get(at) != Some(&1) {
                return Err(CONFIG);
            }
            at += 1;
            let pps = length_unit(data, &mut at, 2)?;
            // AVC High-profile 4:2:0 eight-bit configuration, with no SPS extension.
            if at != data.len() && data.get(at..) != Some(&[0xfd, 0xf8, 0xf8, 0]) {
                return Err(CONFIG);
            }
            vec![sps, pps]
        };
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
        if data.len() != 2
            || data[0] >> 3 != 2
            || ((data[0] & 7) << 1) | (data[1] >> 7) != 3
            || data[1] & 7 != 0
        {
            return Err(CONFIG);
        }
        let channels = (data[1] >> 3) & 15;
        if !(1..=7).contains(&channels) {
            return Err(CONFIG);
        }
        Ok(Self::Audio { channels })
    }
    fn units(
        &self,
        data: &[u8],
        transport: bool,
        hash: &mut Sha256,
    ) -> Result<(), ConversionError> {
        match self {
            Self::Video { sps, pps } => {
                let units = if transport {
                    annex(data)?
                } else {
                    let mut units = Vec::new();
                    let mut at = 0;
                    while at < data.len() {
                        units.push(length_unit(data, &mut at, 4)?);
                        if units.len() > 4096 {
                            return Err(BAD);
                        }
                    }
                    units
                };
                let mut count = 0_u64;
                let mut vcl = false;
                for (at, unit) in units.iter().enumerate() {
                    let kind = unit[0] & 31;
                    if unit[0] & 0x80 != 0
                        || (matches!(kind, 5 | 7 | 8) && unit[0] & 0x60 == 0)
                        || (matches!(kind, 6 | 9 | 10 | 11 | 12) && unit[0] & 0x60 != 0)
                    {
                        return Err(BAD);
                    }
                    match kind {
                        7 if *unit == sps.as_slice() => {}
                        8 if *unit == pps.as_slice() => {}
                        7 | 8 => return Err(CONFIG),
                        9 if transport && at == 0 && unit.len() == 2 && unit[1] & 31 == 16 => {}
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
            }
            Self::Audio { channels } => {
                let raw = if transport {
                    if data.len() < 7
                        || data[0] != 0xff
                        || data[1] != 0xf1
                        || data[2] >> 6 != 1
                        || (data[2] >> 2) & 15 != 3
                        || ((data[2] & 1) << 2) | (data[3] >> 6) != *channels
                        || data[6] & 3 != 0
                    {
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
}
fn length_unit<'a>(
    data: &'a [u8],
    at: &mut usize,
    width: usize,
) -> Result<&'a [u8], ConversionError> {
    let mut length = 0usize;
    for &b in data.get(*at..*at + width).ok_or(BAD)? {
        length = (length << 8) | usize::from(b);
    }
    *at += width;
    if length == 0 {
        return Err(BAD);
    }
    let unit = data.get(*at..*at + length).ok_or(BAD)?;
    *at += length;
    Ok(unit)
}
fn annex(data: &[u8]) -> Result<Vec<&[u8]>, ConversionError> {
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
pub(super) struct Evidence {
    count: u64,
    digest: [u8; 32],
    pub first_pts: i64,
    pub end_pts: i64,
}
impl Evidence {
    pub fn payload_equal(&self, other: &Self) -> bool {
        self.count == other.count && self.digest == other.digest
    }
}
pub(super) struct Parser {
    index: usize,
    expected: u64,
    transport: bool,
    config: Config,
    buffer: Vec<u8>,
    hash: Sha256,
    count: u64,
    next_dts: Option<i64>,
    first_pts: Option<i64>,
    end_pts: i64,
    error: Option<ConversionError>,
}

impl Parser {
    pub fn new(index: usize, expected: u64, transport: bool, config: Config) -> Self {
        Self {
            index,
            expected,
            transport,
            config,
            buffer: Vec::new(),
            hash: Sha256::new(),
            count: 0,
            next_dts: None,
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
                self.record(&record[..record.len() - 1])?;
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
        if duration <= 0 || dts < 0 || pts < dts || self.next_dts.is_some_and(|next| next != dts) {
            return Err(ConversionError(
                "ts-clock-wrap-or-discontinuity-not-supported",
            ));
        }
        if matches!(self.config, Config::Audio { .. }) && duration != 1920 {
            return Err(CONFIG);
        }
        self.next_dts = Some(dts.checked_add(duration).ok_or(BAD)?);
        self.first_pts = Some(self.first_pts.map_or(pts, |old| old.min(pts)));
        self.end_pts = self.end_pts.max(pts.checked_add(duration).ok_or(BAD)?);
        let bytes = dump(data.strip_suffix('|').unwrap_or(data).as_bytes(), true)?;
        self.hash.update(b"access-unit");
        self.config.units(&bytes, self.transport, &mut self.hash)?;
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
            digest: self.hash.finalize().into(),
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
    fn aac_framing_requires_one_lc_48khz_block_with_the_same_configuration() {
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
        assert!(Config::audio(&[0x12, 0x10]).is_err());
        assert!(Config::audio(&[0x11, 0x94]).is_err());
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
    fn streamed_access_units_require_complete_counts_and_continuous_absolute_clocks() {
        let first = record(0, "90000", 90000, 3750);
        let second = record(0, "93750", 93750, 3750);
        for width in [1, 7, 4096] {
            let mut parser = Parser::new(0, 2, false, video());
            for part in format!("{first}{second}").as_bytes().chunks(width) {
                parser.push(part).unwrap();
            }
            parser.finish().unwrap();
        }
        for bad in [
            record(1, "90000", 90000, 3750),
            record(0, "N/A", 90000, 3750),
            record(0, "90000", 90000, 0),
            record(0, "89999", 90000, 3750),
            first.trim_end().to_string(),
            format!("{first}{}", record(0, "97500", 97500, 3750)),
        ] {
            let mut parser = Parser::new(0, 1, false, video());
            let _ = parser.push(bad.as_bytes());
            assert!(parser.finish().is_err());
        }
        let mut parser = Parser::new(0, 2, false, video());
        let gap = format!("{first}{}", record(0, "97500", 97500, 3750));
        assert!(parser.push(gap.as_bytes()).is_err());
        let mut parser = Parser::new(0, 2, false, video());
        parser.push(first.as_bytes()).unwrap();
        assert!(parser.finish().is_err());
    }
}
