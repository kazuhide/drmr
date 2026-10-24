//! Processing intent is separate from container selection and encryption protection.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Choose the processing unit independently of the requested output container.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessingMode {
    /// Select a qualified route for the actual input and output.
    #[default]
    Auto,
    /// Process original segments and retain their sample boundaries and timing.
    Segments,
    /// Join source segments when needed, then package the requested output.
    /// HLS and DASH output is segmented again.
    Stitch,
}

impl ProcessingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Segments => "segments",
            Self::Stitch => "stitch",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessingMode;

    #[test]
    fn public_modes_round_trip_without_accepting_unapproved_aliases() {
        for (name, mode) in [
            ("auto", ProcessingMode::Auto),
            ("segments", ProcessingMode::Segments),
            ("stitch", ProcessingMode::Stitch),
        ] {
            assert_eq!(
                serde_json::from_str::<ProcessingMode>(&format!("\"{name}\"")).unwrap(),
                mode
            );
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{name}\""));
            assert_eq!(mode.as_str(), name);
        }
        for invalid in ["\"tracks\"", "\"segment\"", "\"STITCH\"", "null", "{}"] {
            assert!(serde_json::from_str::<ProcessingMode>(invalid).is_err());
        }
        assert_eq!(ProcessingMode::default(), ProcessingMode::Auto);
    }
}
