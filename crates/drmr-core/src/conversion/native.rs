//! Original-media sample evidence and independent native preservation checks.
mod audio_clock;
mod compare;
mod duration_headers;
pub(super) mod durations;
mod media;
mod profile;
mod samples;
mod semantics;
#[cfg(test)]
mod tests;

use media::Track;
pub(super) use profile::{mp4_profile, transport_profile};
