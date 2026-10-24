//! Only a strictly parsed terminal AAC claim may be separated from raw sample evidence.
use super::media::{Media, audio_evidence};
use crate::conversion::{
    ConversionError,
    audio_presentation::{self, AudioPresentation},
    native_media,
};

const BAD: ConversionError = audio_presentation::MISMATCH;

pub(super) struct AudioProjection {
    expected: AudioPresentation,
}
impl AudioProjection {
    pub(super) fn new(expected: AudioPresentation) -> Result<Self, ConversionError> {
        AudioPresentation::new(expected.evidence, expected.presented_samples)?;
        Ok(Self { expected })
    }

    pub(super) fn metadata(
        &self,
        movie: &native_media::Movie,
        part: native_media::MetadataPart<'_>,
    ) -> Result<(), ConversionError> {
        let [track] = movie.tracks.as_slice() else {
            return Err(BAD);
        };
        match part {
            native_media::MetadataPart::Movie(bytes) => {
                let (claim, track_id) = audio_presentation::guarded_claim(bytes)?.ok_or(BAD)?;
                if claim != self.expected.claim()
                    || track.track_id != track_id
                    || track.sample_count != self.expected.evidence.coded_frames
                {
                    return Err(BAD);
                }
                Ok(())
            }
            native_media::MetadataPart::Fragment(bytes) => {
                audio_presentation::guarded_fragment(bytes, track.track_id)
            }
        }
    }

    pub(super) fn descriptor(
        &self,
        descriptor: &mut native_media::TrackDescriptor,
    ) -> Result<(), ConversionError> {
        if descriptor.index != 0
            || descriptor.edits.len() != 2
            || descriptor.sample_count != self.expected.evidence.coded_frames
        {
            return Err(BAD);
        }
        descriptor.edits.clear();
        Ok(())
    }

    pub(super) fn movie(&self, movie: &mut native_media::Movie) -> Result<(), ConversionError> {
        let [track] = movie.tracks.as_mut_slice() else {
            return Err(BAD);
        };
        self.descriptor(track)
    }

    pub(super) fn bind(&self, media: &Media) -> Result<(), ConversionError> {
        let [track] = media.tracks.as_slice() else {
            return Err(BAD);
        };
        let actual = self.expected.claim().bind(audio_evidence(track)?)?;
        if actual != self.expected {
            return Err(BAD);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
