//! HEVC uses a two-byte NAL header; offsets below refer to the escaped original.
use super::{BAD, ConversionError, MAX_BYTES};
use zeroize::Zeroizing;

pub(super) struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }
    pub fn u(&mut self, width: u32) -> Result<u32, ConversionError> {
        if width > 32
            || self
                .at
                .checked_add(width as usize)
                .is_none_or(|end| end > self.data.len() * 8)
        {
            return Err(BAD);
        }
        let mut value = 0;
        for _ in 0..width {
            value = (value << 1) | u32::from((self.data[self.at / 8] >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Ok(value)
    }
    pub fn u8(&mut self, width: u32) -> Result<u8, ConversionError> {
        u8::try_from(self.u(width)?).map_err(|_| BAD)
    }
    pub fn u16(&mut self, width: u32) -> Result<u16, ConversionError> {
        u16::try_from(self.u(width)?).map_err(|_| BAD)
    }
    pub fn ue8(&mut self, maximum: u32) -> Result<u8, ConversionError> {
        u8::try_from(self.ue(maximum)?).map_err(|_| BAD)
    }
    pub fn ue16(&mut self, maximum: u32) -> Result<u16, ConversionError> {
        u16::try_from(self.ue(maximum)?).map_err(|_| BAD)
    }
    pub fn flag(&mut self) -> Result<bool, ConversionError> {
        Ok(self.u(1)? != 0)
    }
    pub fn ue(&mut self, max: u32) -> Result<u32, ConversionError> {
        let mut width = 0;
        while !self.flag()? {
            width += 1;
            if width > 31 {
                return Err(BAD);
            }
        }
        let value = (1_u64 << width) - 1 + u64::from(self.u(width)?);
        if value > u64::from(max) {
            return Err(BAD);
        }
        u32::try_from(value).map_err(|_| BAD)
    }
    pub fn se(&mut self, min: i32, max: i32) -> Result<i32, ConversionError> {
        let code = self.ue(u32::MAX)?;
        let magnitude = i64::from(code).div_euclid(2) + i64::from(code & 1);
        let value = if code & 1 == 0 { -magnitude } else { magnitude };
        if value < i64::from(min) || value > i64::from(max) {
            return Err(BAD);
        }
        i32::try_from(value).map_err(|_| BAD)
    }
    pub fn position(&self) -> usize {
        self.at
    }
    pub fn remaining(&self) -> usize {
        self.data.len() * 8 - self.at
    }
    pub fn trailing(&mut self) -> Result<(), ConversionError> {
        if !self.flag()? || self.remaining() >= 8 {
            return Err(BAD);
        }
        while self.remaining() != 0 {
            if self.flag()? {
                return Err(BAD);
            }
        }
        Ok(())
    }
    pub fn align(&mut self) -> Result<(), ConversionError> {
        if !self.flag()? {
            return Err(BAD);
        }
        while !self.at.is_multiple_of(8) {
            if self.flag()? {
                return Err(BAD);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Header {
    pub kind: u8,
    pub temporal: u8,
}
pub(super) fn header(nal: &[u8]) -> Result<Header, ConversionError> {
    let head = nal.get(..2).ok_or(BAD)?;
    let kind = (head[0] >> 1) & 63;
    if head[0] & 0x80 != 0 || head[1].trailing_zeros() >= 3 {
        return Err(BAD);
    }
    if head[0] & 1 != 0 || head[1] >> 3 != 0 {
        return Err(ConversionError("native-hevc-multilayer-not-supported"));
    }
    let temporal = (head[1] & 7) - 1;
    if (matches!(kind, 2..=5) && temporal == 0)
        || (matches!(kind, 16..=21 | 32 | 33 | 36 | 37) && temporal != 0)
        || matches!(kind, 10..=15 | 22..=31 | 41..=63)
    {
        return Err(BAD);
    }
    Ok(Header { kind, temporal })
}

pub(super) enum Rbsp<'a> {
    Borrowed(&'a [u8]),
    Owned(Zeroizing<Vec<u8>>),
}
impl std::ops::Deref for Rbsp<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }
}
pub(super) fn rbsp(nal: &[u8]) -> Result<Rbsp<'_>, ConversionError> {
    header(nal)?;
    if nal.len() > MAX_BYTES {
        return Err(ConversionError("native-hevc-access-unit-size-limit"));
    }
    if nal.len() < 3 {
        return Err(BAD);
    }
    let mut zeros = 0;
    let mut first_escape = None;
    let mut at = 2;
    while at < nal.len() {
        let byte = nal[at];
        if zeros == 2 {
            if byte <= 2 {
                return Err(BAD);
            }
            if byte == 3 {
                if nal.get(at + 1).is_none_or(|n| *n > 3) {
                    return Err(BAD);
                }
                first_escape.get_or_insert(at);
                zeros = 0;
                at += 1;
                continue;
            }
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        at += 1;
    }
    let Some(escape) = first_escape else {
        return Ok(Rbsp::Borrowed(&nal[2..]));
    };
    let mut bytes = Zeroizing::new(Vec::new());
    bytes.try_reserve_exact(nal.len() - 2).map_err(|_| BAD)?;
    bytes.extend_from_slice(&nal[2..escape]);
    let mut at = escape + 1;
    let mut zeros = 0;
    while at < nal.len() {
        let byte = nal[at];
        at += 1;
        if zeros == 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        bytes.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    Ok(Rbsp::Owned(bytes))
}

pub(super) fn original_offset(nal: &[u8], rbsp_bytes: usize) -> Result<usize, ConversionError> {
    let mut left = rbsp_bytes;
    let mut at = 2;
    let mut zeros = 0;
    while left != 0 {
        let byte = *nal.get(at).ok_or(BAD)?;
        at += 1;
        if zeros == 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        left -= 1;
    }
    Ok(at)
}

pub(super) fn log2_ceil(value: u32) -> u32 {
    32 - value.saturating_sub(1).leading_zeros()
}
