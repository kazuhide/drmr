//! AAC transport demuxing that preserves the original 90 kHz packet clocks.
use super::super::{ConversionError, files};
use super::{NativeScanner, canonical};
use crate::{concurrency::Stop, inspection::TrackKind};
use std::path::Path;
use zeroize::{Zeroize as _, Zeroizing};

const BAD: ConversionError = ConversionError("ts-aac-mp4-invalid");
const CLOCK: ConversionError = ConversionError("ts-clock-wrap-or-discontinuity-not-supported");
const WRITE: ConversionError = ConversionError("ts-aac-mp4-write-failed");
const MAX_SAMPLES: usize = 128;
// An ADTS frame has a 13-bit length, including its validated seven-byte header.
const MAX_SAMPLE_BYTES: usize = 8191 - 7;
const MAX_PAYLOAD: usize = MAX_SAMPLES * MAX_SAMPLE_BYTES;

/// Strip only validated ADTS framing, retaining every AAC access unit and clock.
pub(super) fn write(
    input: &Path,
    output: &Path,
    expected: &canonical::Native,
    stop: &Stop,
) -> Result<(), ConversionError> {
    check_stop(stop)?;
    let canonical::Config::Audio {
        channels,
        sample_rate,
    } = expected.config
    else {
        return Err(BAD);
    };
    if expected.count == 0 {
        return Err(BAD);
    }
    let mut writer = Writer::new(files::create_new(output)?, channels, sample_rate, stop)?;
    let mut scanner = NativeScanner::new();
    let mut selected = None;
    scanner.segment_with(input, stop, &mut |id, kind, bytes, pts, dts| {
        if kind != TrackKind::Audio {
            return Ok(());
        }
        if selected.is_some_and(|selected| selected != id) {
            return Err(BAD);
        }
        selected = Some(id);
        if pts != dts {
            return Err(CLOCK);
        }
        writer.push(bytes.get(7..).ok_or(BAD)?, pts, stop)
    })?;
    let (_, native) = scanner.finish()?;
    let source = native.get(&selected.ok_or(BAD)?).ok_or(BAD)?;
    if !source.same_as(expected) || writer.count != expected.count {
        return Err(ConversionError("ts-original-evidence-mismatch"));
    }
    writer.finish(stop)
}

struct Sample {
    pts: i64,
    duration: u32,
    size: u32,
}
struct Writer<W: std::io::Write> {
    output: W,
    samples: Vec<Sample>,
    payload: Zeroizing<Vec<u8>>,
    sequence: u32,
    count: u64,
    final_duration: u32,
}
impl<W: std::io::Write> Writer<W> {
    fn new(
        mut output: W,
        channels: u8,
        sample_rate: u32,
        stop: &Stop,
    ) -> Result<Self, ConversionError> {
        check_stop(stop)?;
        output
            .write_all(&initialization(channels, sample_rate)?)
            .map_err(|_| WRITE)?;
        let mut payload = Zeroizing::new(Vec::new());
        // Reserve once so plaintext is never copied out of a reallocated Vec.
        payload.try_reserve_exact(MAX_PAYLOAD).map_err(|_| BAD)?;
        Ok(Self {
            output,
            samples: Vec::with_capacity(MAX_SAMPLES),
            payload,
            sequence: 1,
            count: 0,
            final_duration: 1024 * 90_000 / sample_rate,
        })
    }
    fn push(&mut self, bytes: &[u8], pts: i64, stop: &Stop) -> Result<(), ConversionError> {
        check_stop(stop)?;
        if pts < 0 || bytes.is_empty() || bytes.len() > MAX_SAMPLE_BYTES {
            return Err(BAD);
        }
        if let Some(previous) = self.samples.last_mut() {
            previous.duration = pts
                .checked_sub(previous.pts)
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n != 0)
                .ok_or(CLOCK)?;
        }
        // The next frame resolves the preceding duration before a fragment is
        // flushed. No accumulated rounding or sample-rate clock conversion occurs.
        if self.samples.len() == MAX_SAMPLES {
            self.flush(stop)?;
        }
        if self
            .payload
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > MAX_PAYLOAD)
        {
            return Err(BAD);
        }
        self.payload.extend_from_slice(bytes);
        self.samples.push(Sample {
            pts,
            duration: 0,
            size: u32::try_from(bytes.len()).map_err(|_| BAD)?,
        });
        self.count = self.count.checked_add(1).ok_or(BAD)?;
        Ok(())
    }
    fn finish(mut self, stop: &Stop) -> Result<(), ConversionError> {
        check_stop(stop)?;
        // There is no following source timestamp for the final frame. Match its
        // integer transport duration without extending the asset by a new frame.
        self.samples.last_mut().ok_or(BAD)?.duration = self.final_duration;
        self.flush(stop)?;
        self.output.flush().map_err(|_| WRITE)
    }
    fn flush(&mut self, stop: &Stop) -> Result<(), ConversionError> {
        check_stop(stop)?;
        let moof = fragment(self.sequence, &self.samples)?;
        self.output.write_all(&moof).map_err(|_| WRITE)?;
        let mdat_size = u32::try_from(self.payload.len() + 8).map_err(|_| BAD)?;
        self.output
            .write_all(&mdat_size.to_be_bytes())
            .map_err(|_| WRITE)?;
        self.output.write_all(b"mdat").map_err(|_| WRITE)?;
        for bytes in self.payload.chunks(64 * 1024) {
            check_stop(stop)?;
            self.output.write_all(bytes).map_err(|_| WRITE)?;
        }
        self.payload.zeroize();
        self.samples.clear();
        self.sequence = self.sequence.checked_add(1).ok_or(BAD)?;
        Ok(())
    }
}

fn check_stop(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}

fn boxed(kind: [u8; 4], payload: &[u8]) -> Result<Vec<u8>, ConversionError> {
    let size = u32::try_from(payload.len().checked_add(8).ok_or(BAD)?).map_err(|_| BAD)?;
    let mut bytes = Vec::with_capacity(payload.len() + 8);
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(&kind);
    bytes.extend_from_slice(payload);
    Ok(bytes)
}
fn container(kind: [u8; 4], children: &[Vec<u8>]) -> Result<Vec<u8>, ConversionError> {
    boxed(kind, &children.concat())
}
fn full_box(kind: [u8; 4], flags: u32, payload: &[u8]) -> Result<Vec<u8>, ConversionError> {
    let mut bytes = flags.to_be_bytes().to_vec();
    bytes.extend_from_slice(payload);
    boxed(kind, &bytes)
}
fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|word| word.to_be_bytes()).collect()
}
fn matrix() -> Vec<u8> {
    words(&[0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000])
}

fn initialization(channels: u8, sample_rate: u32) -> Result<Vec<u8>, ConversionError> {
    if !(1..=7).contains(&channels) || !matches!(sample_rate, 44_100 | 48_000) {
        return Err(BAD);
    }
    let mut movie_header = words(&[0, 0, 90_000, 0, 0x0001_0000, 0x0100_0000, 0, 0]);
    movie_header.extend(matrix());
    movie_header.extend(words(&[0, 0, 0, 0, 0, 0, 2]));
    let mut tkhd = words(&[0, 0, 1, 0, 0, 0, 0, 0, 0x0100_0000]);
    tkhd.extend(matrix());
    tkhd.extend(words(&[0, 0]));
    let mut media_header = words(&[0, 0, 90_000, 0]);
    media_header.extend_from_slice(&[0x55, 0xc4, 0, 0]); // Language: undetermined.
    let mut hdlr = words(&[0]);
    hdlr.extend_from_slice(b"soun");
    hdlr.extend(words(&[0, 0, 0]));
    hdlr.extend_from_slice(b"SoundHandler\0");
    let dref = full_box(
        *b"dref",
        0,
        &[words(&[1]), full_box(*b"url ", 1, &[])?].concat(),
    )?;
    let stsd = full_box(
        *b"stsd",
        0,
        &[words(&[1]), sample_entry(channels, sample_rate)?].concat(),
    )?;
    let stbl = container(
        *b"stbl",
        &[
            stsd,
            full_box(*b"stts", 0, &words(&[0]))?,
            full_box(*b"stsc", 0, &words(&[0]))?,
            full_box(*b"stsz", 0, &words(&[0, 0]))?,
            full_box(*b"stco", 0, &words(&[0]))?,
        ],
    )?;
    let minf = container(
        *b"minf",
        &[
            full_box(*b"smhd", 0, &[0; 4])?,
            container(*b"dinf", &[dref])?,
            stbl,
        ],
    )?;
    let mdia = container(
        *b"mdia",
        &[
            full_box(*b"mdhd", 0, &media_header)?,
            full_box(*b"hdlr", 0, &hdlr)?,
            minf,
        ],
    )?;
    let trak = container(*b"trak", &[full_box(*b"tkhd", 7, &tkhd)?, mdia])?;
    let mvex = container(
        *b"mvex",
        &[full_box(*b"trex", 0, &words(&[1, 1, 0, 0, 0]))?],
    )?;
    let moov = container(
        *b"moov",
        &[full_box(*b"mvhd", 0, &movie_header)?, trak, mvex],
    )?;
    Ok([boxed(*b"ftyp", b"iso6\0\0\0\x01iso6mp41dash")?, moov].concat())
}

fn sample_entry(channels: u8, sample_rate: u32) -> Result<Vec<u8>, ConversionError> {
    let frequency_index = if sample_rate == 44_100 { 4_u16 } else { 3 };
    let asc = ((2_u16 << 11) | (frequency_index << 7) | (u16::from(channels) << 3)).to_be_bytes();
    let mut decoder = vec![0x40, 0x15, 0, 0, 0];
    decoder.extend_from_slice(&[0; 8]); // Unspecified peak and average bitrates.
    decoder.extend_from_slice(&[5, 2, asc[0], asc[1]]);
    let mut es = vec![0, 1, 0, 4, u8::try_from(decoder.len()).map_err(|_| BAD)?];
    es.extend(decoder);
    es.extend_from_slice(&[6, 1, 2]);
    let mut descriptors = vec![3, u8::try_from(es.len()).map_err(|_| BAD)?];
    descriptors.extend(es);
    let mut entry = vec![0; 6];
    entry.extend_from_slice(&1_u16.to_be_bytes());
    entry.extend_from_slice(&[0; 8]);
    entry.extend_from_slice(&u16::from(if channels == 7 { 8 } else { channels }).to_be_bytes());
    entry.extend_from_slice(&[0, 16, 0, 0, 0, 0]);
    entry.extend_from_slice(&(sample_rate << 16).to_be_bytes());
    entry.extend(full_box(*b"esds", 0, &descriptors)?);
    boxed(*b"mp4a", &entry)
}

fn fragment(sequence: u32, samples: &[Sample]) -> Result<Vec<u8>, ConversionError> {
    let first = samples.first().ok_or(BAD)?;
    if samples.len() > MAX_SAMPLES || samples.iter().any(|sample| sample.duration == 0) {
        return Err(BAD);
    }
    let count = u32::try_from(samples.len()).map_err(|_| BAD)?;
    let moof_length = 88_u32
        .checked_add(count.checked_mul(8).ok_or(BAD)?)
        .ok_or(BAD)?;
    let data_offset = moof_length.checked_add(8).ok_or(BAD)?;
    let mut entries = words(&[count, data_offset]);
    for sample in samples {
        entries.extend(words(&[sample.duration, sample.size]));
    }
    let traf = container(
        *b"traf",
        &[
            full_box(*b"tfhd", 0x0002_0000, &words(&[1]))?,
            full_box(
                *b"tfdt",
                0x0100_0000,
                &u64::try_from(first.pts).map_err(|_| BAD)?.to_be_bytes(),
            )?,
            full_box(*b"trun", 0x0000_0301, &entries)?,
        ],
    )?;
    let moof = container(
        *b"moof",
        &[full_box(*b"mfhd", 0, &words(&[sequence]))?, traf],
    )?;
    if moof.len() != usize::try_from(moof_length).map_err(|_| BAD)? {
        return Err(BAD);
    }
    Ok(moof)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_keeps_aac_rate_separate_from_media_clock() {
        for (rate, asc) in [(44_100, [0x12, 0x10]), (48_000, [0x11, 0x90])] {
            let bytes = initialization(2, rate).unwrap();
            let boxes = crate::mp4::parse_boxes(&bytes).unwrap();
            let found = crate::mp4::walk(&boxes);
            let mdhd = found
                .iter()
                .find(|box_| box_.header.kind == *b"mdhd")
                .unwrap();
            assert_eq!(
                &mdhd.header.payload(&bytes)[12..16],
                &90_000_u32.to_be_bytes()
            );
            let entry = sample_entry(2, rate).unwrap();
            assert_eq!(&entry[32..36], &(rate << 16).to_be_bytes());
            assert!(
                entry
                    .windows(4)
                    .any(|bytes| bytes == [5, 2, asc[0], asc[1]])
            );
        }
        assert!(initialization(0, 44_100).is_err());
        assert!(initialization(2, 32_000).is_err());
    }

    #[test]
    fn fragments_keep_fractional_clock_rounding_and_raw_samples() {
        let stop = Stop::new();
        let mut output = Vec::new();
        let mut writer = Writer::new(&mut output, 2, 44_100, &stop).unwrap();
        let mut clocks = Vec::new();
        for index in 0..(MAX_SAMPLES + 2) {
            let pts = 126_000 + i64::try_from(index).unwrap() * 1024 * 90_000 / 44_100;
            clocks.push(pts);
            writer
                .push(&[0x21, u8::try_from(index).unwrap(), 0x34], pts, &stop)
                .unwrap();
        }
        writer.finish(&stop).unwrap();
        let mut at = 0;
        let mut fragment_index = 0;
        let mut observed = Vec::new();
        while at < output.len() {
            let length =
                usize::try_from(u32::from_be_bytes(output[at..at + 4].try_into().unwrap()))
                    .unwrap();
            if &output[at + 4..at + 8] == b"moof" {
                let fragment = &output[at..at + length];
                let count = if fragment_index == 0 { MAX_SAMPLES } else { 2 };
                assert_eq!(length, 88 + count * 8);
                let tfdt = fragment
                    .windows(4)
                    .position(|bytes| bytes == b"tfdt")
                    .unwrap();
                let base = u64::from_be_bytes(fragment[tfdt + 8..tfdt + 16].try_into().unwrap());
                let first = fragment_index * MAX_SAMPLES;
                assert_eq!(base, u64::try_from(clocks[first]).unwrap());
                let trun = fragment
                    .windows(4)
                    .position(|bytes| bytes == b"trun")
                    .unwrap();
                assert_eq!(
                    u32::from_be_bytes(fragment[trun + 12..trun + 16].try_into().unwrap()),
                    u32::try_from(length + 8).unwrap()
                );
                let mut pts = i64::try_from(base).unwrap();
                for index in 0..count {
                    let entry = trun + 16 + index * 8;
                    let duration =
                        u32::from_be_bytes(fragment[entry..entry + 4].try_into().unwrap());
                    observed.push(pts);
                    pts += i64::from(duration);
                }
                let mdat = at + length;
                assert_eq!(&output[mdat + 4..mdat + 8], b"mdat");
                for index in 0..count {
                    assert_eq!(
                        &output[mdat + 8 + index * 3..mdat + 11 + index * 3],
                        &[0x21, u8::try_from(first + index).unwrap(), 0x34]
                    );
                }
                fragment_index += 1;
            }
            at += length;
        }
        assert_eq!(fragment_index, 2);
        assert_eq!(observed, clocks);
    }

    #[test]
    fn refuses_duplicate_clocks_oversized_samples_and_cancellation() {
        let stop = Stop::new();
        let mut writer = Writer::new(Vec::new(), 2, 44_100, &stop).unwrap();
        writer.push(&[1], 126_000, &stop).unwrap();
        assert_eq!(writer.push(&[2], 126_000, &stop).unwrap_err(), CLOCK);
        let mut writer = Writer::new(Vec::new(), 2, 44_100, &stop).unwrap();
        assert_eq!(
            writer
                .push(&[0; MAX_SAMPLE_BYTES + 1], 126_000, &stop)
                .unwrap_err(),
            BAD
        );
        stop.set();
        assert_eq!(
            writer.push(&[1], 126_000, &stop).unwrap_err(),
            ConversionError("cancelled")
        );
    }
}
