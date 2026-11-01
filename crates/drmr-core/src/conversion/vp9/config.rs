//! The `FullBox` version/flags belong to `vpcC` and cannot be guessed from frame bytes.
use super::{CONFIG, ConversionError, Metadata};
use crate::sha256::Sha256;
#[derive(Clone, Debug)]
pub(in crate::conversion) struct Configuration {
    raw: [u8; 12],
    metadata: Metadata,
}
impl Configuration {
    pub fn from_vpcc(data: &[u8]) -> Result<Self, ConversionError> {
        if data.len() != 12 || data[..4] != [1, 0, 0, 0] || data[10..] != [0, 0] {
            return Err(CONFIG);
        }
        let profile = data[4];
        let level = data[5];
        let depth = data[6] >> 4;
        let chroma = (data[6] >> 1) & 7;
        let range = data[6] & 1 != 0;
        if !matches!(
            level,
            0 | 10 | 11 | 20 | 21 | 30 | 31 | 40 | 41 | 50 | 51 | 52 | 60 | 61 | 62
        ) || !matches!(
            (profile, depth, chroma),
            (0, 8, 0 | 1) | (1, 8, 2 | 3) | (2, 10 | 12, 0 | 1) | (3, 10 | 12, 2 | 3)
        ) {
            return Err(CONFIG);
        }
        if !matches!(data[7], 1 | 2 | 4..=12 | 22)
            || !matches!(data[8], 1 | 2 | 4..=18)
            || !matches!(data[9],0..=2|4..=14)
            || (data[9] == 0 && chroma != 3)
        {
            return Err(CONFIG);
        }
        Ok(Self {
            raw: data.try_into().map_err(|_| CONFIG)?,
            metadata: Metadata {
                profile,
                level,
                bit_depth: depth,
                chroma_subsampling: chroma,
                full_range: range,
                colour_primaries: data[7],
                transfer_characteristics: data[8],
                matrix_coefficients: data[9],
            },
        })
    }
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
    pub fn codec_string(&self) -> String {
        let m = &self.metadata;
        format!(
            "vp09.{:02}.{:02}.{:02}.{:02}.{:02}.{:02}.{:02}.{:02}",
            m.profile,
            m.level,
            m.bit_depth,
            m.chroma_subsampling,
            m.colour_primaries,
            m.transfer_characteristics,
            m.matrix_coefficients,
            u8::from(m.full_range)
        )
    }
    pub fn semantic_identity(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"vp9-configuration");
        hash.update(self.raw);
        hash.finalize()
    }
    pub fn semantic_eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
    pub(super) fn color(&self, color: Color) -> Result<(), ConversionError> {
        let m = &self.metadata;
        let chroma = match (color.sub_x, color.sub_y) {
            (true, true) => 0,
            (true, false) => 2,
            (false, false) => 3,
            _ => {
                return Err(ConversionError(
                    "native-vp9-chroma-subsampling-not-supported",
                ));
            }
        };
        if color.profile != m.profile
            || color.depth != m.bit_depth
            || color.range != m.full_range
            || !(chroma == m.chroma_subsampling || (chroma == 0 && m.chroma_subsampling == 1))
        {
            return Err(ConversionError("native-vp9-configuration-mismatch"));
        }
        let accepts = |value, allowed: &[u8]| value == 2 || allowed.contains(&value);
        let (primaries, matrix): (&[u8], &[u8]) = match color.space {
            0 => return Ok(()),
            1 => (&[5, 6], &[5, 6]),
            2 => (&[1], &[1]),
            3 => (&[6], &[6]),
            4 => (&[7], &[7]),
            5 => (&[9], &[9, 10]),
            7 => (&[1], &[0]),
            _ => return Err(CONFIG),
        };
        if !accepts(m.colour_primaries, primaries)
            || !accepts(m.matrix_coefficients, matrix)
            || (color.space == 7 && !accepts(m.transfer_characteristics, &[13]))
        {
            return Err(ConversionError("native-vp9-configuration-mismatch"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Color {
    pub profile: u8,
    pub depth: u8,
    pub sub_x: bool,
    pub sub_y: bool,
    pub range: bool,
    pub space: u8,
}
