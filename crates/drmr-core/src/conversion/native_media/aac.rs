//! Bounded AAC-LC `AudioSpecificConfig`, including declared SBR/PS and program configuration.
use super::ConversionError;
const BAD: ConversionError = ConversionError("native-aac-config-invalid");
const UNSUPPORTED: ConversionError = ConversionError("native-aac-config-not-supported");
const RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];
const CHANNELS: [u16; 15] = [0, 1, 2, 3, 4, 5, 6, 8, 0, 0, 0, 7, 8, 24, 8];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct AacConfiguration {
    pub object_type: u8,
    pub core_object_type: u8,
    pub core_sample_rate: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_configuration: u8,
    pub frame_samples: u16,
    pub sbr: bool,
    pub ps: bool,
}
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Bits<'_> {
    fn get(&mut self, count: usize) -> Result<u32, ConversionError> {
        if count > 32
            || self
                .at
                .checked_add(count)
                .is_none_or(|end| end > self.bytes.len() * 8)
        {
            return Err(BAD);
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from((self.bytes[self.at / 8] >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Ok(value)
    }
    fn flag(&mut self) -> Result<bool, ConversionError> {
        Ok(self.get(1)? != 0)
    }
    fn skip(&mut self, count: usize) -> Result<(), ConversionError> {
        self.at = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len() * 8)
            .ok_or(BAD)?;
        Ok(())
    }
    fn remaining(&self) -> usize {
        self.bytes.len() * 8 - self.at
    }
    fn peek(&mut self, count: usize) -> Result<u32, ConversionError> {
        let at = self.at;
        let value = self.get(count);
        self.at = at;
        value
    }
    fn zero_tail(&mut self) -> Result<(), ConversionError> {
        while self.remaining() != 0 {
            if self.get(1)? != 0 {
                return Err(UNSUPPORTED);
            }
        }
        Ok(())
    }
}
fn read_object_type(bits: &mut Bits<'_>) -> Result<u8, ConversionError> {
    let value = bits.get(5)?;
    u8::try_from(if value == 31 {
        32 + bits.get(6)?
    } else {
        value
    })
    .map_err(|_| BAD)
}
fn frequency(bits: &mut Bits<'_>) -> Result<u32, ConversionError> {
    let index = bits.get(4)?;
    let rate = if index == 15 {
        bits.get(24)?
    } else {
        *RATES.get(index as usize).ok_or(BAD)?
    };
    if rate == 0 {
        return Err(BAD);
    }
    Ok(rate)
}
fn program_config(bits: &mut Bits<'_>, rate: u32) -> Result<u16, ConversionError> {
    bits.get(4)?;
    if bits.get(2)? != 1 {
        return Err(UNSUPPORTED);
    }
    let index = bits.get(4)?;
    if RATES.get(index as usize) != Some(&rate) {
        return Err(BAD);
    }
    let front = bits.get(4)?;
    let side = bits.get(4)?;
    let back = bits.get(4)?;
    let lfe = bits.get(2)?;
    let assoc = bits.get(3)?;
    let coupling = bits.get(4)?;
    if bits.flag()? {
        bits.get(4)?;
    }
    if bits.flag()? {
        bits.get(4)?;
    }
    if bits.flag()? {
        bits.get(2)?;
        bits.get(1)?;
    }
    let mut channels = 0_u16;
    let mut elements = 0_u32;
    for _ in 0..front + side + back {
        let pair = bits.flag()?;
        let tag = bits.get(4)?;
        let index = tag + if pair { 16 } else { 0 };
        let mask = 1_u32 << index;
        if elements & mask != 0 {
            return Err(BAD);
        }
        elements |= mask;
        channels = channels.checked_add(if pair { 2 } else { 1 }).ok_or(BAD)?;
    }
    let mut lfe_tags = 0_u16;
    for _ in 0..lfe {
        let tag = bits.get(4)?;
        let mask = 1_u16 << tag;
        if lfe_tags & mask != 0 {
            return Err(BAD);
        }
        lfe_tags |= mask;
        channels = channels.checked_add(1).ok_or(BAD)?;
    }
    for _ in 0..assoc {
        bits.get(4)?;
    }
    for _ in 0..coupling {
        bits.get(1)?;
        bits.get(4)?;
    }
    while !bits.at.is_multiple_of(8) {
        if bits.get(1)? != 0 {
            return Err(BAD);
        }
    }
    let comment = bits.get(8)?;
    bits.skip(
        usize::try_from(comment)
            .map_err(|_| BAD)?
            .checked_mul(8)
            .ok_or(BAD)?,
    )?;
    if channels == 0 {
        return Err(BAD);
    }
    Ok(channels)
}
impl AacConfiguration {
    pub fn parse(bytes: &[u8]) -> Result<Self, ConversionError> {
        if bytes.len() < 2 || bytes.len() > 65_535 {
            return Err(BAD);
        }
        let mut bits = Bits { bytes, at: 0 };
        let object_type = read_object_type(&mut bits)?;
        let core_sample_rate = frequency(&mut bits)?;
        let channel_configuration = u8::try_from(bits.get(4)?).map_err(|_| BAD)?;
        let mut sbr = matches!(object_type, 5 | 29);
        let mut ps = object_type == 29;
        let mut sample_rate = core_sample_rate;
        let core_object_type = if sbr {
            sample_rate = frequency(&mut bits)?;
            read_object_type(&mut bits)?
        } else {
            object_type
        };
        if core_object_type != 2 {
            return Err(UNSUPPORTED);
        }
        let short = bits.flag()?;
        if bits.flag()? {
            bits.get(14)?;
            return Err(UNSUPPORTED);
        }
        let extension = bits.flag()?;
        let mut channels = if channel_configuration == 0 {
            program_config(&mut bits, core_sample_rate)?
        } else {
            let channels = *CHANNELS
                .get(usize::from(channel_configuration))
                .ok_or(BAD)?;
            if channels == 0 {
                return Err(BAD);
            }
            channels
        };
        if extension && bits.flag()? {
            return Err(UNSUPPORTED);
        }
        if bits.remaining() >= 16 && bits.peek(11)? == 0x2b7 {
            if sbr {
                return Err(BAD);
            }
            bits.get(11)?;
            if read_object_type(&mut bits)? != 5 {
                return Err(UNSUPPORTED);
            }
            sbr = bits.flag()?;
            if sbr {
                sample_rate = frequency(&mut bits)?;
            }
        }
        if bits.remaining() >= 12 && bits.peek(11)? == 0x548 {
            bits.get(11)?;
            ps = bits.flag()?;
        }
        if ps {
            if !sbr || channels != 1 {
                return Err(BAD);
            }
            channels = 2;
        }
        bits.zero_tail()?;
        let ratio = if sample_rate == core_sample_rate {
            1
        } else if sample_rate == core_sample_rate.checked_mul(2).ok_or(BAD)? {
            2
        } else {
            return Err(UNSUPPORTED);
        };
        let frame_samples: u16 = (if short { 960 } else { 1024 }) * ratio;
        Ok(Self {
            object_type,
            core_object_type,
            core_sample_rate,
            sample_rate,
            channels,
            channel_configuration,
            frame_samples,
            sbr,
            ps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Writer {
        bytes: Vec<u8>,
        bits: usize,
    }
    impl Writer {
        fn put(&mut self, n: usize, value: u32) {
            for bit in (0..n).rev() {
                if self.bits.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let at = self.bytes.len() - 1;
                self.bytes[at] |= u8::from((value >> bit) & 1 != 0) << (7 - self.bits % 8);
                self.bits += 1;
            }
        }
        fn align(&mut self) {
            while !self.bits.is_multiple_of(8) {
                self.put(1, 0);
            }
        }
    }
    fn basic(index: u32, channels: u32, short: bool) -> Writer {
        let mut writer = Writer::default();
        writer.put(5, 2);
        writer.put(4, index);
        writer.put(4, channels);
        writer.put(1, u32::from(short));
        writer.put(2, 0);
        writer
    }
    #[test]
    fn lc_all_standard_rates_and_960_or_1024_frames_are_typed() {
        for (index, rate) in RATES.into_iter().enumerate() {
            for short in [false, true] {
                let value =
                    AacConfiguration::parse(&basic(u32::try_from(index).unwrap(), 2, short).bytes)
                        .unwrap();
                assert_eq!(value.sample_rate, rate);
                assert_eq!(value.channels, 2);
                assert_eq!(value.frame_samples, if short { 960 } else { 1024 });
            }
        }
    }
    #[test]
    fn explicit_frequency_and_extended_standard_channel_configurations_are_supported() {
        let mut writer = Writer::default();
        writer.put(5, 2);
        writer.put(4, 15);
        writer.put(24, 50_000);
        writer.put(4, 2);
        writer.put(3, 0);
        assert_eq!(
            AacConfiguration::parse(&writer.bytes).unwrap().sample_rate,
            50_000
        );
        for index in [11, 12, 13, 14] {
            assert_eq!(
                AacConfiguration::parse(&basic(4, index, false).bytes)
                    .unwrap()
                    .channels,
                CHANNELS[index as usize]
            );
        }
    }
    #[test]
    fn program_configuration_is_bounded_and_supplies_actual_channels() {
        let mut writer = basic(4, 0, false);
        writer.put(4, 0);
        writer.put(2, 1);
        writer.put(4, 4);
        writer.put(4, 1);
        writer.put(4, 0);
        writer.put(4, 0);
        writer.put(2, 0);
        writer.put(3, 0);
        writer.put(4, 0);
        writer.put(3, 0);
        writer.put(1, 1);
        writer.put(4, 0);
        writer.align();
        writer.put(8, 0);
        let value = AacConfiguration::parse(&writer.bytes).unwrap();
        assert_eq!(value.channels, 2);
        for length in 0..writer.bytes.len() {
            assert!(AacConfiguration::parse(&writer.bytes[..length]).is_err());
        }
    }
    #[test]
    fn explicit_sbr_and_parametric_stereo_keep_core_and_presented_rates_distinct() {
        for object in [5, 29] {
            let mut writer = Writer::default();
            writer.put(5, object);
            writer.put(4, 7);
            writer.put(4, if object == 29 { 1 } else { 2 });
            writer.put(4, 4);
            writer.put(5, 2);
            writer.put(3, 0);
            let value = AacConfiguration::parse(&writer.bytes).unwrap();
            assert_eq!(value.core_sample_rate, 22_050);
            assert_eq!(value.sample_rate, 44_100);
            assert_eq!(value.channels, 2);
            assert_eq!(value.frame_samples, 2048);
            assert!(value.sbr);
            assert_eq!(value.ps, object == 29);
        }
    }
    #[test]
    fn implicit_sbr_ps_extensions_are_fully_consumed() {
        let mut writer = basic(7, 1, false);
        writer.put(11, 0x2b7);
        writer.put(5, 5);
        writer.put(1, 1);
        writer.put(4, 4);
        writer.put(11, 0x548);
        writer.put(1, 1);
        let value = AacConfiguration::parse(&writer.bytes).unwrap();
        assert!(value.sbr && value.ps);
        assert_eq!(value.channels, 2);
        assert_eq!(value.sample_rate, 44_100);
    }
    #[test]
    fn reserved_rates_channels_truncations_and_unknown_trailing_bits_fail() {
        for index in [13, 14] {
            assert!(AacConfiguration::parse(&basic(index, 2, false).bytes).is_err());
        }
        for index in [8, 9, 10, 15] {
            assert!(AacConfiguration::parse(&basic(4, index, false).bytes).is_err());
        }
        let mut bytes = basic(4, 2, false).bytes;
        bytes.push(1);
        assert!(AacConfiguration::parse(&bytes).is_err());
        for length in 0..2 {
            assert!(AacConfiguration::parse(&bytes[..length]).is_err());
        }
    }
}
