use super::{BAD, CHUNK_BYTES, ConversionError, MAX_METADATA_BYTES, UNSUPPORTED};
use crate::sha256::Sha256;
use crate::{concurrency::Stop, mp4::Mp4Box, native::FileIdentity};
use std::{
    io::{Read as _, Seek as _, SeekFrom},
    ops::Range,
    path::{Path, PathBuf},
};
use zeroize::{Zeroize as _, Zeroizing};

mod positioned;
use positioned::PositionedInput;
mod sample;
pub(super) use sample::SampleInput;

#[derive(Default)]
pub(super) struct PayloadBuffer {
    bytes: Zeroizing<Vec<u8>>,
}
pub(super) struct PayloadChunk<'a> {
    pub bytes: &'a mut [u8],
}
impl Drop for PayloadChunk<'_> {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}
impl PayloadBuffer {
    pub fn chunk(&mut self, length: usize) -> Result<PayloadChunk<'_>, ConversionError> {
        if length > CHUNK_BYTES {
            return Err(BAD);
        }
        if length != 0 && self.bytes.is_empty() {
            // Reserve the entire lifetime bound before retaining any plaintext.
            self.bytes
                .try_reserve_exact(CHUNK_BYTES)
                .map_err(|_| ConversionError("resource-budget"))?;
            self.bytes.resize(CHUNK_BYTES, 0);
        }
        Ok(PayloadChunk {
            bytes: &mut self.bytes[..length],
        })
    }
}

pub(super) struct Reader<'a> {
    pub file: PositionedInput,
    pub length: u64,
    pub stop: &'a Stop,
    pub scratch: PayloadBuffer,
    pub sample_window: PayloadBuffer,
    path: PathBuf,
    identity: FileIdentity,
    digest: Option<[u8; 32]>,
}
pub(super) struct Layout {
    pub movie: Vec<u8>,
    pub movie_offset: u64,
    pub mdats: Vec<Range<u64>>,
    pub moofs: Vec<Range<u64>>,
}
impl<'a> Reader<'a> {
    pub fn open(path: &Path, stop: &'a Stop, fingerprint: bool) -> Result<Self, ConversionError> {
        check_stop(stop)?;
        let file = super::super::files::regular_input(path)?;
        let identity = FileIdentity::read(&file).map_err(|_| BAD)?;
        let length = file.metadata().map_err(|_| BAD)?.len();
        let mut reader = Self {
            file: PositionedInput::new(file),
            length,
            stop,
            scratch: PayloadBuffer::default(),
            sample_window: PayloadBuffer::default(),
            path: path.into(),
            identity,
            digest: None,
        };
        if fingerprint {
            reader.digest = Some(reader.fingerprint()?);
        }
        Ok(reader)
    }
    pub fn read(&mut self, range: Range<u64>, maximum: usize) -> Result<Vec<u8>, ConversionError> {
        let length = range.end.checked_sub(range.start).ok_or(BAD)?;
        let length = usize::try_from(length).map_err(|_| BAD)?;
        if range.end > self.length || length > maximum {
            return Err(BAD);
        }
        check_stop(self.stop)?;
        self.file
            .seek(SeekFrom::Start(range.start))
            .map_err(|_| BAD)?;
        let mut bytes = vec![0; length];
        self.file.read_exact(&mut bytes).map_err(|_| BAD)?;
        Ok(bytes)
    }
    pub fn layout(&mut self) -> Result<Layout, ConversionError> {
        self.layout_with_initialization(None)
    }
    pub fn resource_layout(&mut self, initialization: &Layout) -> Result<Layout, ConversionError> {
        self.layout_with_initialization(Some(initialization))
    }
    pub fn expected_fingerprint(&self) -> Option<[u8; 32]> {
        self.digest
    }
    fn layout_with_initialization(
        &mut self,
        initialization: Option<&Layout>,
    ) -> Result<Layout, ConversionError> {
        let mut at = 0_u64;
        let mut movie = initialization.map(|value| value.movie.clone());
        let mut movie_offset = initialization.map_or(0, |value| value.movie_offset);
        let mut mdats = Vec::new();
        let mut moofs = Vec::new();
        while at < self.length {
            let header = self.read(at..at.checked_add(8).ok_or(BAD)?, 8)?;
            let small = integer(&header, 0)?;
            let (size, head) = match small {
                0 => (self.length.checked_sub(at).ok_or(BAD)?, 8_u64),
                1 => (
                    unsigned64(&self.read(at + 8..at.checked_add(16).ok_or(BAD)?, 8)?, 0)?,
                    16,
                ),
                value => (u64::from(value), 8),
            };
            let end = at
                .checked_add(size)
                .filter(|end| *end <= self.length)
                .ok_or(BAD)?;
            if size < head {
                return Err(BAD);
            }
            match &header[4..8] {
                b"moov" => {
                    if movie.is_some() {
                        return Err(BAD);
                    }
                    let mut bytes = self.read(at..end, MAX_METADATA_BYTES)?;
                    if small == 0 {
                        bytes[..4]
                            .copy_from_slice(&u32::try_from(size).map_err(|_| BAD)?.to_be_bytes());
                    }
                    movie = Some(bytes);
                    movie_offset = at;
                }
                b"mdat" => {
                    range_budget(mdats.len(), moofs.len())?;
                    mdats.push(at + head..end);
                }
                b"moof" => {
                    if size > MAX_METADATA_BYTES as u64 {
                        return Err(BAD);
                    }
                    range_budget(mdats.len(), moofs.len())?;
                    moofs.push(at..end);
                }
                b"ftyp" | b"styp" | b"sidx" | b"ssix" | b"mfra" | b"free" | b"skip" | b"pssh" => {}
                // A conventional empty wide box is padding. Nonempty legacy
                // variants can contain media and require a separate reader.
                b"wide" if size == 8 && head == 8 => {}
                b"emsg" => return Err(ConversionError("inband-events-not-supported")),
                _ => return Err(UNSUPPORTED),
            }
            at = end;
        }
        Ok(Layout {
            movie: movie.ok_or(BAD)?,
            movie_offset,
            mdats,
            moofs,
        })
    }
    fn fingerprint(&mut self) -> Result<[u8; 32], ConversionError> {
        self.file.seek(SeekFrom::Start(0)).map_err(|_| BAD)?;
        let buffer = self.scratch.chunk(CHUNK_BYTES)?;
        let mut digest = Sha256::new();
        let mut total = 0_u64;
        loop {
            check_stop(self.stop)?;
            let read = self.file.read(buffer.bytes).map_err(|_| BAD)?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(read as u64)
                .filter(|n| *n <= self.length)
                .ok_or(BAD)?;
            digest.update(&buffer.bytes[..read]);
        }
        if total != self.length {
            return Err(BAD);
        }
        Ok(digest.finalize())
    }
    pub fn finish(mut self) -> Result<(), ConversionError> {
        check_stop(self.stop)?;
        if let Some(expected) = self.digest
            && self.fingerprint()? != expected
        {
            return Err(ConversionError("native-source-changed"));
        }
        let current = super::super::files::regular_input(&self.path)?;
        if FileIdentity::read(&self.file).map_err(|_| BAD)? != self.identity
            || FileIdentity::read(&current).map_err(|_| BAD)? != self.identity
            || self.file.metadata().map_err(|_| BAD)?.len() != self.length
            || current.metadata().map_err(|_| BAD)?.len() != self.length
        {
            return Err(ConversionError("native-source-changed"));
        }
        Ok(())
    }
}
fn range_budget(mdats: usize, moofs: usize) -> Result<(), ConversionError> {
    let total = mdats
        .checked_add(moofs)
        .and_then(|n| n.checked_add(1))
        .ok_or(BAD)?;
    if total > MAX_METADATA_BYTES / std::mem::size_of::<Range<u64>>() {
        return Err(ConversionError("resource-budget"));
    }
    Ok(())
}
pub(super) fn check_stop(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}
pub(super) fn integer(bytes: &[u8], at: usize) -> Result<u32, ConversionError> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at.checked_add(4).ok_or(BAD)?)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
pub(super) fn unsigned64(bytes: &[u8], at: usize) -> Result<u64, ConversionError> {
    Ok(u64::from_be_bytes(
        bytes
            .get(at..at.checked_add(8).ok_or(BAD)?)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
pub(super) fn signed16(bytes: &[u8], at: usize) -> Result<i16, ConversionError> {
    Ok(i16::from_be_bytes(
        bytes
            .get(at..at.checked_add(2).ok_or(BAD)?)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
pub(super) fn full(bytes: &[u8], version: u8, allowed_flags: u32) -> Result<u32, ConversionError> {
    let value = integer(bytes, 0)?;
    if value >> 24 != u32::from(version) || value & 0x00ff_ffff & !allowed_flags != 0 {
        return Err(BAD);
    }
    Ok(value & 0x00ff_ffff)
}
pub(super) fn sample_flags(flags: u32) -> Result<(), ConversionError> {
    if flags & 0xf000_0000 != 0 || [20, 22, 24, 26].into_iter().any(|at| flags >> at & 3 == 3) {
        return Err(BAD);
    }
    Ok(())
}
pub(super) fn one(nodes: &[Mp4Box], kind: [u8; 4]) -> Result<&Mp4Box, ConversionError> {
    optional(nodes, kind)?.ok_or(BAD)
}
pub(super) fn optional(
    nodes: &[Mp4Box],
    kind: [u8; 4],
) -> Result<Option<&Mp4Box>, ConversionError> {
    let mut matching = nodes.iter().filter(|node| node.kind() == kind);
    let first = matching.next();
    if matching.next().is_some() {
        return Err(BAD);
    }
    Ok(first)
}
pub(super) fn count(
    bytes: &[u8],
    at: usize,
    start: usize,
    width: usize,
) -> Result<usize, ConversionError> {
    let count = usize::try_from(integer(bytes, at)?).map_err(|_| BAD)?;
    if count.checked_mul(width).and_then(|n| n.checked_add(start)) != Some(bytes.len()) {
        return Err(BAD);
    }
    Ok(count)
}
pub(super) fn in_mdat(range: &Range<u64>, mdats: &[Range<u64>]) -> Result<(), ConversionError> {
    // The top-level walker emits sorted, disjoint mdat payloads. Search only the
    // last payload starting at or before the sample; a sample cannot cross boxes.
    let at = mdats.partition_point(|mdat| mdat.start <= range.start);
    let candidate = at.checked_sub(1).and_then(|at| mdats.get(at));
    if range.start >= range.end || candidate.is_none_or(|mdat| range.end > mdat.end) {
        return Err(BAD);
    }
    Ok(())
}
pub(super) fn disjoint(ranges: &mut [Range<u64>]) -> Result<(), ConversionError> {
    ranges.sort_unstable_by_key(|range| range.start);
    if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return Err(BAD);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn adjacent_sample_reads_keep_the_cursor_without_repeating_a_seek() {
        let root = crate::native::private_tempdir().unwrap();
        let path = root.path().join("samples");
        std::fs::write(&path, (0_u8..=255).cycle().take(4096).collect::<Vec<_>>()).unwrap();
        let mut input = PositionedInput::new(File::open(path).unwrap());
        let mut bytes = [0; 32];
        for ordinal in 0..32_u64 {
            assert_eq!(
                input.seek(SeekFrom::Start(ordinal * 32)).unwrap(),
                ordinal * 32
            );
            input.read_exact(&mut bytes).unwrap();
            for (index, byte) in bytes.iter().enumerate() {
                assert_eq!(
                    *byte,
                    u8::try_from((ordinal * 32 + index as u64) % 256).unwrap()
                );
            }
        }
        assert_eq!(input.seeks, 0);
        assert_eq!(input.reads, 32);
        input.seek(SeekFrom::Start(2048)).unwrap();
        input.read_exact(&mut bytes).unwrap();
        assert_eq!(input.seeks, 1);
        assert_eq!(input.position, Some(2080));
        assert_eq!(input.seek(SeekFrom::Current(-32)).unwrap(), 2048);
        assert_eq!(input.seeks, 2);
        input.seek(SeekFrom::Start(2048)).unwrap();
        assert_eq!(input.seeks, 2);
        input.seek(SeekFrom::End(-16)).unwrap();
        assert_eq!(input.position, Some(4080));
        input.read_exact(&mut bytes[..16]).unwrap();
        assert_eq!(input.position, Some(4096));
        assert!(input.read_exact(&mut bytes[..1]).is_err());
        assert_eq!(input.position, Some(4096));
        input.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(input.position, Some(0));
    }

    #[test]
    fn failed_reads_and_seeks_require_a_fresh_absolute_position() {
        let root = crate::native::private_tempdir().unwrap();
        let path = root.path().join("write-only");
        let mut input = PositionedInput::new(File::create(path).unwrap());
        assert!(input.read(&mut [0; 1]).is_err());
        assert_eq!(input.position, None);
        assert_eq!(input.seek(SeekFrom::Start(0)).unwrap(), 0);
        assert_eq!(input.seeks, 1);
        assert!(input.seek(SeekFrom::Current(-1)).is_err());
        assert_eq!(input.position, None);
        assert_eq!(input.seek(SeekFrom::Start(0)).unwrap(), 0);
        assert_eq!(input.seeks, 3);
    }

    #[test]
    fn payload_scratch_is_bounded_reused_and_wiped_on_success_and_error() {
        let mut scratch = PayloadBuffer::default();
        assert!(scratch.chunk(0).unwrap().bytes.is_empty());
        assert!(scratch.bytes.is_empty());
        let address;
        {
            let chunk = scratch.chunk(17).unwrap();
            address = chunk.bytes.as_ptr();
            chunk.bytes.fill(0x55);
        }
        assert_eq!(scratch.bytes.len(), CHUNK_BYTES);
        assert!(scratch.bytes.iter().all(|byte| *byte == 0));
        let failure = (|| -> Result<(), ConversionError> {
            let chunk = scratch.chunk(CHUNK_BYTES)?;
            assert_eq!(chunk.bytes.as_ptr(), address);
            chunk.bytes.fill(0x77);
            Err(BAD)
        })();
        assert_eq!(failure, Err(BAD));
        assert!(scratch.bytes.iter().all(|byte| *byte == 0));
        assert!(matches!(scratch.chunk(CHUNK_BYTES + 1), Err(BAD)));
        assert_eq!(scratch.chunk(1).unwrap().bytes.as_ptr(), address);
    }

    #[test]
    fn only_the_empty_conventional_wide_box_is_neutral_top_level_padding() {
        let root = crate::native::private_tempdir().unwrap();
        let input = root.path().join("layout.mp4");
        let stop = Stop::new();
        for (kind, payload) in [
            (*b"wide", Vec::new()),
            (*b"wide", vec![0; 8]),
            (*b"uuid", vec![0; 16]),
            (*b"zzzz", Vec::new()),
        ] {
            let mut bytes = u32::try_from(payload.len() + 8)
                .unwrap()
                .to_be_bytes()
                .to_vec();
            bytes.extend(kind);
            bytes.extend(payload);
            bytes.extend([0, 0, 0, 8, b'm', b'o', b'o', b'v']);
            std::fs::write(&input, &bytes).unwrap();
            let layout = Reader::open(&input, &stop, true).unwrap().layout();
            if kind == *b"wide" && bytes.len() == 16 {
                assert_eq!(layout.unwrap().movie_offset, 8);
            } else {
                assert!(matches!(layout, Err(UNSUPPORTED)));
            }
            assert_eq!(std::fs::read(&input).unwrap(), bytes);
        }
    }

    #[test]
    fn logarithmic_mdat_lookup_keeps_exact_payload_boundaries_and_refusals() {
        let mdats = [10..20, 30..30, 40..50, 60..70];
        for range in [10..20, 11..12, 40..41, 49..50, 60..70] {
            in_mdat(&range, &mdats).unwrap();
        }
        for range in [
            0..1,
            9..11,
            10..21,
            19..41,
            20..21,
            30..31,
            40..40,
            70..71,
            Range { start: 71, end: 70 },
        ] {
            assert!(in_mdat(&range, &mdats).is_err(), "{range:?}");
        }
        assert!(in_mdat(&(1..2), &[]).is_err());
        assert!(disjoint(&mut [10..20, 19..21]).is_err());
        let many = (0..100_000_u64)
            .map(|n| n * 20..n * 20 + 10)
            .collect::<Vec<_>>();
        in_mdat(&(1_999_980..1_999_990), &many).unwrap();
        assert!(in_mdat(&(1_999_989..1_999_991), &many).is_err());
    }
}
