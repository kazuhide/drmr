use super::{BAD, ConversionError, UNSUPPORTED};
use super::{
    crypto::{SampleEncryption, Subsample, TrackEncryption},
    read::{count, full, integer, optional, unsigned64},
};
use crate::mp4::Mp4Box;
use zeroize::Zeroizing;

pub(super) struct Auxiliary<'a> {
    track: &'a TrackEncryption,
    bytes: &'a [u8],
    sizes: &'a [u8],
    at: usize,
    count: usize,
    index: usize,
    subsamples: bool,
    constant: bool,
}
impl<'a> Auxiliary<'a> {
    pub fn new(
        nodes: &[Mp4Box],
        data: &'a [u8],
        track: &'a TrackEncryption,
        total: usize,
        source_base: u64,
        offset_base: u64,
    ) -> Result<Self, ConversionError> {
        let Some(senc) = optional(nodes, *b"senc")? else {
            if track.constant_iv.is_none()
                || optional(nodes, *b"saiz")?.is_some()
                || optional(nodes, *b"saio")?.is_some()
            {
                return Err(BAD);
            }
            return Ok(Self {
                track,
                bytes: &[],
                sizes: &[],
                at: 0,
                count: total,
                index: 0,
                subsamples: false,
                constant: true,
            });
        };
        let bytes = senc.header.payload(data);
        let flags = full(bytes, 0, 2)?;
        if usize::try_from(integer(bytes, 4)?).map_err(|_| BAD)? != total {
            return Err(BAD);
        }
        let sizes_node = optional(nodes, *b"saiz")?;
        let offsets_node = optional(nodes, *b"saio")?;
        let sizes = match (sizes_node, offsets_node) {
            (None, None) => &[][..],
            (Some(sizes), Some(offsets)) => {
                let sizes = sizes.header.payload(data);
                full(sizes, 0, 0)?;
                if usize::try_from(integer(sizes, 5)?).map_err(|_| BAD)? != total {
                    return Err(BAD);
                }
                let default = *sizes.get(4).ok_or(BAD)?;
                if sizes.len()
                    != if default == 0 {
                        9usize.checked_add(total).ok_or(BAD)?
                    } else {
                        9
                    }
                {
                    return Err(BAD);
                }
                let offsets = offsets.header.payload(data);
                let version = *offsets.first().ok_or(BAD)?;
                if version > 1 {
                    return Err(UNSUPPORTED);
                }
                full(offsets, version, 0)?;
                if count(offsets, 4, 8, if version == 0 { 4 } else { 8 })? != 1 {
                    return Err(BAD);
                }
                let offset = if version == 0 {
                    u64::from(integer(offsets, 8)?)
                } else {
                    unsigned64(offsets, 8)?
                };
                let expected = source_base
                    .checked_add(senc.header.offset as u64)
                    .and_then(|n| n.checked_add(senc.header.header_size as u64 + 8))
                    .ok_or(BAD)?;
                if offset_base.checked_add(offset) != Some(expected) {
                    return Err(BAD);
                }
                sizes
            }
            _ => return Err(BAD),
        };
        let constant = track.iv_size == 0 && flags == 0 && bytes.len() == 8;
        if constant
            && !sizes.is_empty()
            && (sizes[4] != 0 || sizes[9..].iter().any(|size| *size != 0))
        {
            return Err(BAD);
        }
        Ok(Self {
            track,
            bytes,
            sizes,
            at: 8,
            count: total,
            index: 0,
            subsamples: flags & 2 != 0,
            constant,
        })
    }
    pub fn next(&mut self, size: u32) -> Result<SampleEncryption, ConversionError> {
        if self.index >= self.count {
            return Err(BAD);
        }
        let start = self.at;
        let iv = if self.track.iv_size == 0 {
            self.track.constant_iv.as_ref().ok_or(BAD)?.clone()
        } else {
            let end = self
                .at
                .checked_add(usize::from(self.track.iv_size))
                .ok_or(BAD)?;
            let iv = self.bytes.get(self.at..end).ok_or(BAD)?;
            self.at = end;
            Zeroizing::new(iv.to_vec())
        };
        let subsamples = if self.subsamples {
            let head = self
                .bytes
                .get(self.at..self.at.checked_add(2).ok_or(BAD)?)
                .ok_or(BAD)?;
            let total = usize::from(u16::from_be_bytes(head.try_into().map_err(|_| BAD)?));
            self.at += 2;
            if total == 0 || total > self.bytes.len().saturating_sub(self.at) / 6 {
                return Err(BAD);
            }
            let mut subsamples = Vec::with_capacity(total);
            let mut covered = 0_u64;
            for _ in 0..total {
                let clear = u32::from(u16::from_be_bytes(
                    self.bytes
                        .get(self.at..self.at + 2)
                        .ok_or(BAD)?
                        .try_into()
                        .map_err(|_| BAD)?,
                ));
                let encrypted = integer(self.bytes, self.at + 2)?;
                self.at += 6;
                covered = covered
                    .checked_add(u64::from(clear) + u64::from(encrypted))
                    .ok_or(BAD)?;
                subsamples.push(Subsample { clear, encrypted });
            }
            if covered != u64::from(size) {
                return Err(BAD);
            }
            subsamples
        } else {
            vec![Subsample {
                clear: 0,
                encrypted: size,
            }]
        };
        if !self.constant && !self.sizes.is_empty() {
            let expected = if self.sizes[4] == 0 {
                self.sizes[9 + self.index]
            } else {
                self.sizes[4]
            };
            if self.at - start != usize::from(expected) {
                return Err(BAD);
            }
        }
        self.index += 1;
        Ok(SampleEncryption { iv, subsamples })
    }
    pub fn finish(self) -> Result<(), ConversionError> {
        if self.index != self.count || (!self.constant && self.at != self.bytes.len()) {
            return Err(BAD);
        }
        Ok(())
    }
    pub fn constant(&self) -> bool {
        self.constant
    }
    pub fn finish_constant(mut self) -> Result<(), ConversionError> {
        if !self.constant {
            return Err(BAD);
        }
        self.index = self.count;
        self.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4;
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bytes.extend(kind);
        bytes.extend(payload);
        bytes
    }
    fn track() -> TrackEncryption {
        TrackEncryption {
            scheme: super::super::Scheme::Cenc,
            kid: [1; 16],
            iv_size: 8,
            constant_iv: None,
            crypt: 0,
            skip: 0,
        }
    }
    fn senc() -> Vec<u8> {
        let mut p = vec![0, 0, 0, 2];
        p.extend(1_u32.to_be_bytes());
        p.extend([7; 8]);
        p.extend(1_u16.to_be_bytes());
        p.extend(4_u16.to_be_bytes());
        p.extend(8_u32.to_be_bytes());
        p
    }
    fn data(payload: &[u8], size: u8, offset: u32, pair: bool) -> Vec<u8> {
        let mut bytes = boxed(*b"senc", payload);
        if pair {
            let mut sizes = vec![0, 0, 0, 0, size];
            sizes.extend(1_u32.to_be_bytes());
            bytes.extend(boxed(*b"saiz", &sizes));
            let mut offsets = vec![0, 0, 0, 0];
            offsets.extend(1_u32.to_be_bytes());
            offsets.extend(offset.to_be_bytes());
            bytes.extend(boxed(*b"saio", &offsets));
        }
        bytes
    }
    fn validate(bytes: &[u8], sample: u32) -> Result<(), ConversionError> {
        let nodes = mp4::parse_boxes(bytes).map_err(|_| BAD)?;
        let protection = track();
        let mut aux = Auxiliary::new(&nodes, bytes, &protection, 1, 0, 0)?;
        let entry = aux.next(sample)?;
        if entry.subsamples.len() != 1 {
            return Err(BAD);
        }
        aux.finish()
    }
    #[test]
    fn auxiliary_counts_sizes_offsets_and_subsample_totals_are_bound_to_the_same_sample() {
        validate(&data(&senc(), 16, 16, true), 12).unwrap();
        validate(&data(&senc(), 16, 16, false), 12).unwrap();
        assert!(validate(&data(&senc(), 15, 16, true), 12).is_err());
        assert!(validate(&data(&senc(), 16, 17, true), 12).is_err());
        assert!(validate(&data(&senc(), 16, 16, true), 13).is_err());
        let mut wrong = senc();
        wrong[4..8].copy_from_slice(&2_u32.to_be_bytes());
        assert!(validate(&data(&wrong, 16, 16, true), 12).is_err());
        let mut wrong = senc();
        wrong[3] = 4;
        assert!(validate(&data(&wrong, 16, 16, true), 12).is_err());
    }
    #[test]
    fn auxiliary_reads_cannot_cross_senc_bounds_or_invent_zero_iv_bytes() {
        let p = senc();
        for length in 0..p.len() {
            assert!(validate(&data(&p[..length], 16, 16, true), 12).is_err());
        }
        let mut one = boxed(*b"senc", &p);
        let mut sizes = vec![0, 0, 0, 0, 16];
        sizes.extend(1_u32.to_be_bytes());
        one.extend(boxed(*b"saiz", &sizes));
        assert!(validate(&one, 12).is_err());
    }
}
