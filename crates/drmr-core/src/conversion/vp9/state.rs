//! Only header-derived state is retained; no decoded pictures or entropy tables.
use super::{BAD, ConversionError, config::Color};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Geometry {
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
}
#[derive(Clone, Copy)]
pub(super) struct Reference {
    pub geometry: Geometry,
    pub color: Color,
}
pub(super) struct State {
    pub references: [Option<Reference>; 8],
    pub contexts: [bool; 4],
    pub color: Option<Color>,
    pub geometry: Option<Geometry>,
    pub filter_references: [i16; 4],
    pub filter_modes: [i16; 2],
    pub segment_enabled: [[bool; 4]; 8],
    pub segment_data: [[i16; 4]; 8],
    pub segment_absolute: bool,
}
impl State {
    pub fn new() -> Self {
        Self {
            references: [None; 8],
            contexts: [false; 4],
            color: None,
            geometry: None,
            filter_references: [0; 4],
            filter_modes: [0; 2],
            segment_enabled: [[false; 4]; 8],
            segment_data: [[0; 4]; 8],
            segment_absolute: false,
        }
    }
    pub fn independent(&mut self) {
        self.filter_references = [1, 0, -1, -1];
        self.filter_modes = [0; 2];
        self.segment_enabled = [[false; 4]; 8];
        self.segment_data = [[0; 4]; 8];
        self.segment_absolute = false;
    }
    pub fn reference(&self, index: u32) -> Result<Reference, ConversionError> {
        self.references
            .get(usize::try_from(index).map_err(|_| BAD)?)
            .copied()
            .flatten()
            .ok_or(ConversionError("native-vp9-reference-frame-required"))
    }
    pub fn bind_geometry(
        &mut self,
        geometry: Geometry,
        max_width: u32,
        max_height: u32,
    ) -> Result<(), ConversionError> {
        if geometry.width == 0
            || geometry.height == 0
            || geometry.render_width == 0
            || geometry.render_height == 0
        {
            return Err(BAD);
        }
        if geometry.width > max_width
            || geometry.height > max_height
            || geometry.render_width > max_width
            || geometry.render_height > max_height
        {
            return Err(ConversionError("native-vp9-dimensions-exceed-sample-entry"));
        }
        if self.geometry.is_some_and(|old| old != geometry) {
            return Err(ConversionError(
                "native-vp9-dynamic-dimensions-not-supported",
            ));
        }
        self.geometry = Some(geometry);
        Ok(())
    }
}
