//! DASH resource addressing is independent of sample protection and processing.
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Choose how a static DASH manifest names and times its media segments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum DashAddressing {
    /// Numbered resources with an exact `SegmentTimeline`.
    #[default]
    #[serde(rename = "template-timeline")]
    #[value(name = "template-timeline")]
    Template,
    /// Numbered resources with a checked nominal `SegmentTemplate` duration.
    #[serde(rename = "template")]
    #[value(name = "template")]
    TemplateDuration,
    /// Explicit resource URLs with an exact `SegmentTimeline`.
    List,
    /// One indexed fragmented MP4 file for each representation.
    Base,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_names_distinguish_nominal_templates_from_explicit_timelines() {
        for (name, mode) in [
            ("template", DashAddressing::TemplateDuration),
            ("template-timeline", DashAddressing::Template),
            ("list", DashAddressing::List),
            ("base", DashAddressing::Base),
        ] {
            assert_eq!(DashAddressing::from_str(name, false).unwrap(), mode);
            assert_eq!(serde_json::to_value(mode).unwrap(), name);
            assert_eq!(
                serde_json::from_value::<DashAddressing>(serde_json::json!(name)).unwrap(),
                mode
            );
        }
        assert_eq!(DashAddressing::default(), DashAddressing::Template);
        assert!(DashAddressing::from_str("template-duration", false).is_err());
        assert!(
            serde_json::from_value::<DashAddressing>(serde_json::json!("template-duration"))
                .is_err()
        );
    }
}
