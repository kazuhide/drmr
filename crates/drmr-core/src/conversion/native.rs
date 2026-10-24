//! Original-media sample evidence and independent native preservation checks.
mod audio_clock;
mod clock_profile;
mod compare;
mod duration_headers;
pub(super) mod durations;
mod media;
mod profile;
mod samples;
mod segments;
mod semantics;
#[cfg(test)]
mod tests;

pub(in crate::conversion) use clock_profile::ManifestAacClock;
use media::Track;
pub(super) use profile::{mp4_profile, transport_profile};
pub(in crate::conversion) use segments::SegmentedMedia;
