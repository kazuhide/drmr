use super::{BAD, ConversionError};

pub(super) struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }
    pub fn bits(&mut self, width: u32) -> Result<u32, ConversionError> {
        if width > 32
            || self
                .at
                .checked_add(width as usize)
                .is_none_or(|n| n > self.data.len() * 8)
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
    pub fn flag(&mut self) -> Result<bool, ConversionError> {
        Ok(self.bits(1)? != 0)
    }
    pub fn ue(&mut self, maximum: u32) -> Result<u32, ConversionError> {
        let mut zeros = 0;
        while !self.flag()? {
            zeros += 1;
            if zeros > 31 {
                return Err(BAD);
            }
        }
        let value = ((1_u64 << zeros) - 1)
            .checked_add(u64::from(self.bits(zeros)?))
            .filter(|n| *n <= u64::from(maximum))
            .ok_or(BAD)?;
        u32::try_from(value).map_err(|_| BAD)
    }
    pub fn se(&mut self, minimum: i32, maximum: i32) -> Result<i32, ConversionError> {
        let code = self.ue(u32::MAX)?;
        let absolute = (i64::from(code) + 1) / 2;
        let value = if code & 1 == 0 { -absolute } else { absolute };
        if value < i64::from(minimum) || value > i64::from(maximum) {
            return Err(BAD);
        }
        i32::try_from(value).map_err(|_| BAD)
    }
    pub fn remaining(&self) -> usize {
        self.data.len() * 8 - self.at
    }
    pub fn position(&self) -> usize {
        self.at
    }
    pub fn more_rbsp_data(&self) -> bool {
        let mut at = self.at;
        if at >= self.data.len() * 8 || (self.data[at / 8] >> (7 - at % 8)) & 1 == 0 {
            return true;
        }
        at += 1;
        while at < self.data.len() * 8 {
            if (self.data[at / 8] >> (7 - at % 8)) & 1 != 0 {
                return true;
            }
            at += 1;
        }
        false
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
    pub fn byte_align_one(&mut self) -> Result<(), ConversionError> {
        while !self.at.is_multiple_of(8) {
            if !self.flag()? {
                return Err(BAD);
            }
        }
        Ok(())
    }
}

pub(super) enum Rbsp<'a> {
    Borrowed(&'a [u8]),
    Owned(zeroize::Zeroizing<Vec<u8>>),
}
impl std::ops::Deref for Rbsp<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }
}

/// Validate every emulation-prevention sequence before borrowing or removing bytes.
/// Only removal allocates; owned storage is erased on drop, including failed validation.
pub(super) fn rbsp(nal: &[u8], kind: u8) -> Result<Rbsp<'_>, ConversionError> {
    if nal.len() > 8 * 1024 * 1024 {
        return Err(ConversionError("ts-access-unit-size-limit"));
    }
    if nal.len() < 2 || nal[0] & 0x9f != kind || (matches!(kind, 5 | 7 | 8) && nal[0] & 0x60 == 0) {
        return Err(BAD);
    }
    let mut bytes: Option<zeroize::Zeroizing<Vec<u8>>> = None;
    let mut copied = 1;
    // Valid pairs end at EOF or precede a nonzero byte, so matches cannot overlap.
    for pair in memchr::memmem::find_iter(&nal[1..], &[0, 0]) {
        let end = pair + 3;
        match nal.get(end) {
            None => break,
            Some(0..=2) => return Err(BAD),
            Some(3) => {
                if nal.get(end + 1).is_none_or(|n| *n > 3) {
                    return Err(BAD);
                }
                let bytes = bytes.get_or_insert_with(|| zeroize::Zeroizing::new(Vec::new()));
                if bytes.capacity() == 0 {
                    // Reserve before copying plaintext; removing escapes only shrinks it.
                    bytes.try_reserve_exact(nal.len() - 1).map_err(|_| BAD)?;
                }
                bytes.extend_from_slice(&nal[copied..end]);
                copied = end + 1;
            }
            _ => {}
        }
    }
    if let Some(mut bytes) = bytes {
        bytes.extend_from_slice(&nal[copied..]);
        Ok(Rbsp::Owned(bytes))
    } else {
        Ok(Rbsp::Borrowed(&nal[1..]))
    }
}

#[cfg(test)]
#[path = "rbsp_tests.rs"]
mod tests;
