//! Every uncompressed-header field and declared compressed/tile partition boundary.
use super::{
    BAD, CONFIG, Configuration, ConversionError, FrameInfo,
    bits::Bits,
    config::Color,
    state::{Geometry, Reference, State},
};
fn color(b: &mut Bits<'_>, profile: u8) -> Result<Color, ConversionError> {
    let depth = if profile >= 2 {
        if b.flag()? { 12 } else { 10 }
    } else {
        8
    };
    let space = b.u8(3)?;
    if space == 6 || (space == 7 && profile & 1 == 0) {
        return Err(BAD);
    }
    let (range, sub_x, sub_y) = if space == 7 {
        if b.flag()? {
            return Err(BAD);
        }
        (true, false, false)
    } else {
        let range = b.flag()?;
        let (x, y) = if profile & 1 != 0 {
            let x = b.flag()?;
            let y = b.flag()?;
            if b.flag()? || x && y {
                return Err(BAD);
            }
            (x, y)
        } else {
            (true, true)
        };
        (range, x, y)
    };
    Ok(Color {
        profile,
        depth,
        sub_x,
        sub_y,
        range,
        space,
    })
}
fn size(b: &mut Bits<'_>) -> Result<(u32, u32), ConversionError> {
    Ok((u32::from(b.u16(16)?) + 1, u32::from(b.u16(16)?) + 1))
}
fn render(b: &mut Bits<'_>, width: u32, height: u32) -> Result<Geometry, ConversionError> {
    let (render_width, render_height) = if b.flag()? { size(b)? } else { (width, height) };
    Ok(Geometry {
        width,
        height,
        render_width,
        render_height,
    })
}
fn sync(b: &mut Bits<'_>) -> Result<(), ConversionError> {
    if b.u(24)? != 0x0049_8342 {
        return Err(BAD);
    }
    Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Key,
    Intra,
    Inter,
}
struct PictureData {
    color: Color,
    geometry: Geometry,
    refresh: u32,
}
struct CodedHeader {
    kind: FrameKind,
    show: bool,
    resilient: bool,
    reset: u32,
    picture: PictureData,
}
impl CodedHeader {
    fn parse(
        b: &mut Bits<'_>,
        profile: u8,
        config: &Configuration,
        state: &State,
    ) -> Result<Self, ConversionError> {
        let key = !b.flag()?;
        let show = b.flag()?;
        let resilient = b.flag()?;
        let (kind, reset) = if key {
            (FrameKind::Key, 0)
        } else {
            let intra = !show && b.flag()?;
            let reset = if resilient { 0 } else { b.u(2)? };
            (
                if intra {
                    FrameKind::Intra
                } else {
                    FrameKind::Inter
                },
                reset,
            )
        };
        let picture = if kind == FrameKind::Inter {
            inter_picture(b, state)?
        } else {
            intra_picture(b, profile, key, config, state)?
        };
        Ok(Self {
            kind,
            show,
            resilient,
            reset,
            picture,
        })
    }
    fn frame_context(
        &self,
        b: &mut Bits<'_>,
        state: &mut State,
    ) -> Result<(usize, bool), ConversionError> {
        let refresh = if self.resilient {
            false
        } else {
            let refresh = b.flag()?;
            b.flag()?;
            refresh
        };
        let declared = usize::from(b.u8(2)?);
        let context = if self.kind != FrameKind::Inter || self.resilient {
            state.independent();
            if self.kind == FrameKind::Key || self.resilient || self.reset == 3 {
                state.contexts = [true; 4];
            } else if self.reset == 2 {
                state.contexts[declared] = true;
            }
            0
        } else {
            declared
        };
        if !state.contexts[context] {
            return Err(ConversionError("native-vp9-frame-context-required"));
        }
        Ok((context, refresh))
    }
    fn finish(self, state: &mut State, size: usize, header_bytes: usize) -> FrameInfo {
        let g = self.picture.geometry;
        state.color = Some(self.picture.color);
        for index in 0..8 {
            if self.picture.refresh & (1 << index) != 0 {
                state.references[index] = Some(Reference {
                    geometry: g,
                    color: self.picture.color,
                });
            }
        }
        FrameInfo {
            offset: 0,
            size,
            header_bytes,
            show_frame: self.show,
            show_existing: false,
            is_key: self.kind == FrameKind::Key,
            is_intra: self.kind != FrameKind::Inter,
            width: g.width,
            height: g.height,
            render_width: g.render_width,
            render_height: g.render_height,
        }
    }
}
pub(super) fn parse(
    data: &[u8],
    config: &Configuration,
    state: &mut State,
    max_width: u32,
    max_height: u32,
) -> Result<FrameInfo, ConversionError> {
    // VP9 section 9.2.3 requires this for each frame, including indexed subframes.
    if data.last().is_some_and(|byte| *byte & 0xe0 == 0xc0) {
        return Err(BAD);
    }
    let b = &mut Bits::new(data);
    let profile = profile(b, config)?;
    if b.flag()? {
        return show_existing(b, data, config, state, max_width, max_height);
    }
    let header = CodedHeader::parse(b, profile, config, state)?;
    config.color(header.picture.color)?;
    state.bind_geometry(header.picture.geometry, max_width, max_height)?;
    let (context, refresh_context) = header.frame_context(b, state)?;
    loop_filter(b, state)?;
    quantization(b)?;
    segmentation(b, state)?;
    let header_bytes = partition_header(b, data, header.picture.geometry.width)?;
    if refresh_context {
        state.contexts[context] = true;
    }
    Ok(header.finish(state, data.len(), header_bytes))
}
fn profile(b: &mut Bits<'_>, config: &Configuration) -> Result<u8, ConversionError> {
    if b.u(2)? != 2 {
        return Err(BAD);
    }
    let low = b.u8(1)?;
    let profile = low | (b.u8(1)? << 1);
    if profile == 3 && b.flag()? {
        return Err(BAD);
    }
    if profile != config.metadata().profile {
        return Err(CONFIG);
    }
    Ok(profile)
}
fn show_existing(
    b: &mut Bits<'_>,
    data: &[u8],
    config: &Configuration,
    state: &mut State,
    max_width: u32,
    max_height: u32,
) -> Result<FrameInfo, ConversionError> {
    let reference = state.reference(b.u(3)?)?;
    config.color(reference.color)?;
    b.align()?;
    if data
        .get(b.bytes()..)
        .ok_or(BAD)?
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(BAD);
    }
    let g = reference.geometry;
    state.bind_geometry(g, max_width, max_height)?;
    Ok(FrameInfo {
        offset: 0,
        size: data.len(),
        header_bytes: data.len(),
        show_frame: true,
        show_existing: true,
        is_key: false,
        is_intra: false,
        width: g.width,
        height: g.height,
        render_width: g.render_width,
        render_height: g.render_height,
    })
}
fn intra_picture(
    b: &mut Bits<'_>,
    profile: u8,
    key: bool,
    config: &Configuration,
    state: &State,
) -> Result<PictureData, ConversionError> {
    sync(b)?;
    let color = if key || profile > 0 {
        color(b, profile)?
    } else {
        Color {
            profile: 0,
            depth: 8,
            sub_x: true,
            sub_y: true,
            space: 1,
            range: state
                .color
                .map_or(config.metadata().full_range, |old| old.range),
        }
    };
    let refresh = if key { 255 } else { b.u(8)? };
    let (w, h) = size(b)?;
    Ok(PictureData {
        color,
        geometry: render(b, w, h)?,
        refresh,
    })
}
fn inter_picture(b: &mut Bits<'_>, state: &State) -> Result<PictureData, ConversionError> {
    let refresh = b.u(8)?;
    let mut references = Vec::with_capacity(3);
    for _ in 0..3 {
        references.push(state.reference(b.u(3)?)?);
        b.flag()?;
    }
    let color = state
        .color
        .ok_or(ConversionError("native-vp9-reference-frame-required"))?;
    if references.iter().any(|reference| reference.color != color) {
        return Err(ConversionError("native-vp9-reference-format-mismatch"));
    }
    let mut found = None;
    for reference in &references {
        if b.flag()? {
            found = Some((reference.geometry.width, reference.geometry.height));
            break;
        }
    }
    let (w, h) = match found {
        Some(size) => size,
        None => size(b)?,
    };
    let geometry = render(b, w, h)?;
    if !references.iter().any(|r| {
        2 * w >= r.geometry.width
            && 2 * h >= r.geometry.height
            && w <= 16 * r.geometry.width
            && h <= 16 * r.geometry.height
    }) {
        return Err(BAD);
    }
    b.flag()?;
    if !b.flag()? {
        b.u(2)?;
    }
    Ok(PictureData {
        color,
        geometry,
        refresh,
    })
}
fn partition_header(b: &mut Bits<'_>, data: &[u8], width: u32) -> Result<usize, ConversionError> {
    let (columns, rows) = tiles(b, width)?;
    let compressed = usize::from(b.u16(16)?);
    b.align()?;
    let header_bytes = b.bytes();
    if compressed == 0 {
        return Err(BAD);
    }
    partitions(
        data,
        header_bytes.checked_add(compressed).ok_or(BAD)?,
        columns,
        rows,
    )?;
    Ok(header_bytes)
}
fn loop_filter(b: &mut Bits<'_>, state: &mut State) -> Result<(), ConversionError> {
    b.u(6)?;
    b.u(3)?;
    if b.flag()? && b.flag()? {
        for delta in &mut state.filter_references {
            if b.flag()? {
                *delta = b.signed(6)?;
            }
        }
        for delta in &mut state.filter_modes {
            if b.flag()? {
                *delta = b.signed(6)?;
            }
        }
    }
    Ok(())
}
fn quantization(b: &mut Bits<'_>) -> Result<(), ConversionError> {
    b.u(8)?;
    for _ in 0..3 {
        if b.flag()? {
            b.signed(4)?;
        }
    }
    Ok(())
}
fn probability(b: &mut Bits<'_>) -> Result<(), ConversionError> {
    if b.flag()? {
        b.u(8)?;
    }
    Ok(())
}
fn segmentation(b: &mut Bits<'_>, state: &mut State) -> Result<(), ConversionError> {
    if !b.flag()? {
        return Ok(());
    }
    if b.flag()? {
        for _ in 0..7 {
            probability(b)?;
        }
        if b.flag()? {
            for _ in 0..3 {
                probability(b)?;
            }
        }
    }
    if b.flag()? {
        state.segment_absolute = b.flag()?;
        for segment in 0..8 {
            for (feature, width) in [8, 6, 2, 0].into_iter().enumerate() {
                let enabled = b.flag()?;
                state.segment_enabled[segment][feature] = enabled;
                state.segment_data[segment][feature] = if enabled {
                    let value = i16::try_from(b.u(width)?).map_err(|_| BAD)?;
                    if feature < 2 && b.flag()? {
                        // Absolute features require a zero sign bit, even for zero magnitude.
                        if state.segment_absolute {
                            return Err(BAD);
                        }
                        -value
                    } else {
                        value
                    }
                } else {
                    0
                };
            }
        }
    }
    Ok(())
}
fn tiles(b: &mut Bits<'_>, width: u32) -> Result<(u32, u32), ConversionError> {
    let columns = width.div_ceil(64);
    let mut minimum = 0;
    while (64_u32 << minimum) < columns {
        minimum += 1;
    }
    let mut maximum = 1;
    while columns >> maximum >= 4 {
        maximum += 1;
    }
    maximum -= 1;
    if minimum > maximum {
        return Err(BAD);
    }
    let mut log_columns = minimum;
    while log_columns < maximum && b.flag()? {
        log_columns += 1;
        if log_columns > 6 {
            return Err(BAD);
        }
    }
    let log_rows = if b.flag()? {
        1 + u32::from(b.flag()?)
    } else {
        0
    };
    Ok((1 << log_columns, 1 << log_rows))
}
fn partitions(data: &[u8], mut at: usize, columns: u32, rows: u32) -> Result<(), ConversionError> {
    let count = columns.checked_mul(rows).ok_or(BAD)?;
    for index in 0..count {
        if index + 1 == count {
            if at >= data.len() {
                return Err(BAD);
            }
            return Ok(());
        }
        let end = at.checked_add(4).ok_or(BAD)?;
        let size = usize::try_from(u32::from_be_bytes(
            data.get(at..end).ok_or(BAD)?.try_into().map_err(|_| BAD)?,
        ))
        .map_err(|_| BAD)?;
        if size == 0 {
            return Err(BAD);
        }
        at = end
            .checked_add(size)
            .filter(|end| *end < data.len())
            .ok_or(BAD)?;
    }
    Err(BAD)
}
