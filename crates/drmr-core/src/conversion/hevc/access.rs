//! Access-unit ordering, picture identity and original slice entry-point membership.
use super::{
    AccessUnit, BAD, CONFIG, Configuration, ConversionError, RandomAccess, Terminal, bits, sei,
    slice,
};

#[derive(Default)]
struct PictureEvidence {
    picture: Option<slice::Picture>,
    last_address: Option<u32>,
    slices: u32,
    observed_types: u32,
    pending_entry: Option<(u32, u32)>,
}
impl PictureEvidence {
    fn record(&mut self, config: &Configuration, nal: &[u8]) -> Result<(), ConversionError> {
        let header = slice::parse(nal, &config.sps, &config.pps)?;
        let address = config.pps.tile_scan_address(header.address, &config.sps)?;
        if (self.slices == 0 && (!header.first || header.dependent || address != 0))
            || (self.slices != 0 && header.first)
            || self.last_address.is_some_and(|old| address <= old)
        {
            return Err(BAD);
        }
        if let Some(old) = &self.picture {
            if old.nal != header.nal
                || old.no_prior != header.no_prior
                || header.picture.as_ref().is_some_and(|new| new != old)
            {
                return Err(BAD);
            }
        } else {
            self.picture = header.picture;
        }
        if let Some((start, count)) = self.pending_entry
            && config.pps.entry_points(start, address)? != count
        {
            return Err(BAD);
        }
        self.pending_entry = Some((address, header.entry_points));
        if let Some(kind) = header.slice_type {
            self.observed_types |= 1 << kind;
        }
        self.slices = self.slices.checked_add(1).ok_or(BAD)?;
        self.last_address = Some(address);
        Ok(())
    }
    fn finish(
        self,
        config: &Configuration,
        aud: Option<u32>,
    ) -> Result<AccessUnit, ConversionError> {
        let picture = self.picture.ok_or(BAD)?;
        if self.slices == 0 {
            return Err(BAD);
        }
        if let Some((start, count)) = self.pending_entry
            && config.pps.entry_points(start, config.sps.ctbs()?)? != count
        {
            return Err(BAD);
        }
        if aud.is_some_and(|kind| match kind {
            0 => self.observed_types & !4 != 0,
            1 => self.observed_types & !6 != 0,
            _ => false,
        }) {
            return Err(BAD);
        }
        let random_access = match picture.nal.kind {
            16..=18 => RandomAccess::Bla,
            19 | 20 => RandomAccess::Idr,
            21 => RandomAccess::Cra,
            _ => RandomAccess::None,
        };
        Ok(AccessUnit {
            slice_count: self.slices,
            is_sync: random_access != RandomAccess::None,
            random_access,
            leading_picture: matches!(picture.nal.kind, 6..=9),
            temporal: picture.nal.temporal,
            hrd_operation_points: 0x7f,
            terminal: Terminal::None,
        })
    }
}

struct SeiEvidence {
    timing: bool,
    recovery: Option<(i32, bool, bool)>,
    associated_temporal: Option<u8>,
    hrd_operation_points: u8,
}
impl SeiEvidence {
    fn record(&mut self, observed: &sei::Evidence, temporal: u8) -> Result<(), ConversionError> {
        if observed.picture_timing && std::mem::replace(&mut self.timing, true) {
            return Err(BAD);
        }
        if let Some(point) = observed.recovery
            && self.recovery.replace(point).is_some()
        {
            return Err(BAD);
        }
        if observed.associated_picture
            && self
                .associated_temporal
                .replace(temporal)
                .is_some_and(|old| old != temporal)
        {
            return Err(BAD);
        }
        self.hrd_operation_points &= observed.hrd_operation_points;
        if self.hrd_operation_points == 0 {
            return Err(BAD);
        }
        Ok(())
    }
}

pub(super) fn parse(
    config: &Configuration,
    data: &[u8],
    width: u8,
    codec: [u8; 4],
) -> Result<AccessUnit, ConversionError> {
    config.validate_entry(codec)?;
    let units = super::units(data, width)?;
    let mut evidence = PictureEvidence::default();
    let mut suffix = false;
    let mut ended = false;
    let mut terminal_kind = Terminal::None;
    let mut sei = SeiEvidence {
        timing: false,
        recovery: None,
        associated_temporal: None,
        hrd_operation_points: 0x7f,
    };
    let mut aud = None;
    for (index, nal) in units.iter().enumerate() {
        let head = bits::header(nal)?;
        if head.temporal > config.sps.max_sub {
            return Err(BAD);
        }
        match head.kind {
            32..=34 if evidence.picture.is_none() && !ended => {
                parameter(config, nal, head.kind, codec)?;
            }
            35 if index == 0 => {
                let rbsp = bits::rbsp(nal)?;
                let b = &mut bits::Bits::new(&rbsp);
                let kind = b.u(3)?;
                if kind > 2 {
                    return Err(BAD);
                }
                b.trailing()?;
                aud = Some(kind);
            }
            39 if evidence.picture.is_none() && !ended => {
                sei.record(&sei::parse(nal, &config.sps, config.vps.id)?, head.temporal)?;
            }
            0..=9 | 16..=21 if !suffix && !ended => evidence.record(config, nal)?,
            40 if evidence.picture.is_some() && !ended => {
                sei.record(&sei::parse(nal, &config.sps, config.vps.id)?, head.temporal)?;
                suffix = true;
            }
            36 if evidence.picture.is_some() && !ended => {
                terminal(nal)?;
                ended = true;
                terminal_kind = Terminal::Sequence;
            }
            37 if evidence.picture.is_some() && index + 1 == units.len() => {
                terminal(nal)?;
                ended = true;
                terminal_kind = Terminal::Bitstream;
            }
            38 if evidence.picture.is_some() && !ended => {
                let rbsp = bits::rbsp(nal)?;
                let (&last, fill) = rbsp.split_last().ok_or(BAD)?;
                if last != 0x80 || fill.iter().any(|b| *b != 255) {
                    return Err(BAD);
                }
                suffix = true;
            }
            _ => return Err(ConversionError("native-hevc-nal-order-not-supported")),
        }
    }
    let mut unit = evidence.finish(config, aud)?;
    temporal_rules(&units, unit.temporal, sei.associated_temporal)?;
    unit.hrd_operation_points = sei.hrd_operation_points;
    unit.terminal = terminal_kind;
    Ok(unit)
}
fn terminal(nal: &[u8]) -> Result<(), ConversionError> {
    // end_of_seq_rbsp and end_of_bitstream_rbsp are empty, without trailing bits.
    if nal.len() != 2 {
        return Err(BAD);
    }
    Ok(())
}
fn temporal_rules(
    units: &[&[u8]],
    temporal: u8,
    associated: Option<u8>,
) -> Result<(), ConversionError> {
    if associated.is_some_and(|value| value != temporal) {
        return Err(BAD);
    }
    for nal in units {
        let head = bits::header(nal)?;
        let valid = match head.kind {
            32 | 33 => temporal == 0,
            35 | 38 => head.temporal == temporal,
            34 | 39 | 40 => head.temporal >= temporal,
            _ => true,
        };
        if !valid {
            return Err(BAD);
        }
    }
    Ok(())
}
fn parameter(
    config: &Configuration,
    nal: &[u8],
    kind: u8,
    codec: [u8; 4],
) -> Result<(), ConversionError> {
    if codec == *b"hvc1" {
        return Err(ConversionError(
            "native-hevc-in-band-parameter-set-not-supported",
        ));
    }
    let original = config
        .arrays
        .iter()
        .find(|a| a.kind == kind)
        .ok_or(CONFIG)?;
    // The parameter payload must be identical. A repeated PPS may have a different
    // TemporalId when its header satisfies the containing AU's temporal constraints.
    if original.units[0].get(2..) != nal.get(2..) {
        return Err(ConversionError(
            "native-hevc-parameter-set-change-not-supported",
        ));
    }
    Ok(())
}
