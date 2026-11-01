use super::{BAD, ConversionError};
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
                .checked_add(usize::try_from(width).map_err(|_| BAD)?)
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
    pub fn flag(&mut self) -> Result<bool, ConversionError> {
        Ok(self.u(1)? != 0)
    }
    pub fn signed(&mut self, width: u32) -> Result<i16, ConversionError> {
        let value = i16::try_from(self.u(width)?).map_err(|_| BAD)?;
        Ok(if self.flag()? { -value } else { value })
    }
    pub fn align(&mut self) -> Result<(), ConversionError> {
        while !self.at.is_multiple_of(8) {
            if self.flag()? {
                return Err(BAD);
            }
        }
        Ok(())
    }
    pub fn bytes(&self) -> usize {
        self.at / 8
    }
}
