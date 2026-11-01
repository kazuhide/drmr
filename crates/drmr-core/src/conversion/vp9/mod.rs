//! Bounded VP9 frame and persistent reference-header evidence.
//!
//! Syntax: VP9 Bitstream and Decoding Process Specification v0.7, sections 6 through 8:
//! <https://storage.googleapis.com/downloads.webmproject.org/docs/vp9/vp9-bitstream-specification-v0.7-20170222-draft.pdf>.
//! MP4 binding: <https://www.webmproject.org/vp9/mp4/>.
//! Compressed-header probabilities and entropy-coded tiles are not decoded here.
use super::ConversionError;
mod bits;
mod config;
mod header;
mod state;
mod superframe;
pub(in crate::conversion) use config::Configuration;
const BAD: ConversionError = ConversionError("invalid-vp9-syntax");
const CONFIG: ConversionError = ConversionError("native-vp9-configuration-not-supported");
const MAX_BYTES: usize = 8 * 1024 * 1024;
pub(in crate::conversion) const PROTECTION_NOT_SUPPORTED: ConversionError =
    ConversionError("vp9-protection-scheme-not-supported");

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Metadata {
    pub profile: u8,
    pub level: u8,
    pub bit_depth: u8,
    pub chroma_subsampling: u8,
    pub full_range: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "FrameInfo evidence preserves the independent display and coding flags required by its callers."
)]
pub(in crate::conversion) struct FrameInfo {
    pub offset: usize,
    pub size: usize,
    pub header_bytes: usize,
    pub show_frame: bool,
    pub show_existing: bool,
    pub is_key: bool,
    pub is_intra: bool,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
}
#[derive(Debug)]
pub(in crate::conversion) struct Sample {
    pub frames: Vec<FrameInfo>,
    pub index_bytes: usize,
    pub displayed_pictures: u8,
    pub is_sync: bool,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
}
pub(in crate::conversion) struct Parser {
    config: Configuration,
    state: state::State,
    max_width: u32,
    max_height: u32,
    failed: bool,
}
impl Parser {
    pub fn new(
        config: Configuration,
        entry_width: u32,
        entry_height: u32,
    ) -> Result<Self, ConversionError> {
        if entry_width == 0 || entry_height == 0 || entry_width > 65535 || entry_height > 65535 {
            return Err(ConversionError("native-vp9-dimensions-exceed-sample-entry"));
        }
        Ok(Self {
            config,
            state: state::State::new(),
            max_width: entry_width,
            max_height: entry_height,
            failed: false,
        })
    }
    pub fn sample(&mut self, bytes: &[u8]) -> Result<Sample, ConversionError> {
        if self.failed {
            return Err(BAD);
        }
        let result = self.checked_sample(bytes);
        match result {
            Ok(mut sample) => {
                // Replay only this bounded sample with uninitialized references/contexts.
                // This proves independence of the whole sample, including hidden frames.
                let mut independent =
                    Self::new(self.config.clone(), self.max_width, self.max_height)?;
                sample.is_sync = independent.checked_sample(bytes).is_ok();
                Ok(sample)
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }
    fn checked_sample(&mut self, bytes: &[u8]) -> Result<Sample, ConversionError> {
        let layout = superframe::split(bytes)?;
        let mut frames = Vec::with_capacity(layout.frames.len());
        let mut shown = None;
        let mut count = 0_u8;
        for range in layout.frames {
            let mut frame = header::parse(
                bytes.get(range.clone()).ok_or(BAD)?,
                &self.config,
                &mut self.state,
                self.max_width,
                self.max_height,
            )?;
            frame.offset = range.start;
            if frame.show_frame {
                count = count.checked_add(1).ok_or(BAD)?;
                shown = Some((
                    frame.width,
                    frame.height,
                    frame.render_width,
                    frame.render_height,
                ));
            }
            frames.push(frame);
        }
        if count != 1 {
            return Err(ConversionError("native-vp9-display-count-not-supported"));
        }
        let (width, height, render_width, render_height) = shown.ok_or(BAD)?;
        Ok(Sample {
            frames,
            index_bytes: layout.index_bytes,
            displayed_pictures: count,
            is_sync: false,
            width,
            height,
            render_width,
            render_height,
        })
    }
}
#[cfg(test)]
pub(in crate::conversion) fn test_configuration() -> Vec<u8> {
    tests::config(0, 8, 1)
}
#[cfg(test)]
pub(in crate::conversion) fn test_key_frame() -> (Vec<u8>, usize) {
    frame_tests::key(0, 8, 1, true, 128, 128)
}
#[cfg(test)]
pub(in crate::conversion) fn test_show_existing_frame() -> Vec<u8> {
    frame_tests::existing(0, 7, 2)
}
#[cfg(test)]
pub(in crate::conversion) fn test_superframe(frames: &[&[u8]], magnitude: usize) -> Vec<u8> {
    frame_tests::superframe(frames, magnitude)
}
#[cfg(test)]
mod tests;

#[cfg(test)]
mod frame_tests;
