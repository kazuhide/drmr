//! Stable check identifiers and status strings shared by pipelines and reports.
//!
//! Console labels and help text use the same identifiers and are checked in tests.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A check id that no pipeline records.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown check id: {0}")]
pub struct UnknownCheck(pub String);

macro_rules! check_ids {
    ($($variant:ident => $id:literal,)*) => {
        /// A check id as the reports and the log records spell it.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum CheckId {
            $(#[doc = $id] $variant,)*
        }

        impl CheckId {
            /// Every id, catalog checks first, in checklist order.
            pub const ALL: &'static [CheckId] = &[$(CheckId::$variant,)*];

            /// The id string.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(CheckId::$variant => $id,)*
                }
            }
        }

        impl FromStr for CheckId {
            type Err = UnknownCheck;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                match text {
                    $($id => Ok(CheckId::$variant),)*
                    _ => Err(UnknownCheck(text.to_string())),
                }
            }
        }
    };
}

check_ids! {
    BoundaryAgreement => "6-1.boundary-agreement",
    DashPlayreadySync => "6-1.dash-playready-sync",
    DashSchemeCbcs => "6-1.dash-scheme-cbcs",
    DashSegmenttemplateUnchanged => "6-1.dash-segmenttemplate-unchanged",
    DashWidevineUnchanged => "6-1.dash-widevine-unchanged",
    HlsContentGate => "6-1.hls-content-gate",
    HlsExtinf => "6-1.hls-extinf",
    HlsIvAgreement => "6-1.hls-iv-agreement",
    HlsSegmentCount => "6-1.hls-segment-count",
    HlsVersionUnchanged => "6-1.hls-version-unchanged",
    IvAgreement => "6-1.iv-agreement",
    NumberOrigin => "6-1.number-origin",
    PsshAgreement => "6-1.pssh-agreement",
    SegmentDuration => "6-1.segment-duration",
    AudioConsolidationStructure => "6-2.audio-consolidation-structure",
    AudioPcmIdentity => "6-2.audio-pcm-identity",
    Framemd5 => "6-4.framemd5",
    DashOutOfScopeUntouched => "6-5.dash-out-of-scope-untouched",
    HlsMasterUntouched => "6-5.hls-master-untouched",
    DashConformance => "6-6.dash-conformance",
    SourceDecrypt => "source.decrypt",
    SelectionRecorded => "selection-recorded",
    SourceCorrespondencePass => "source-correspondence-pass",
    TimingPass => "timing-pass",
    DecodedIdentityPass => "decoded-identity-pass",
    CbcsSignalingPass => "cbcs-signaling-pass",
    TextPayloadPass => "text-payload-pass",
    TextTimingMapPass => "text-timing-map-pass",
}

impl CheckId {
    /// Whether this is a catalog check rather than a clear-HLS track check: a `6-x.*` id, or
    /// `source.decrypt`, which `pipeline` records when a rendition's source cannot be
    /// decrypted.
    pub fn is_catalog(self) -> bool {
        self.as_str().starts_with("6-") || self == CheckId::SourceDecrypt
    }
}

impl fmt::Display for CheckId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for CheckId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CheckId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A check's outcome as the reports write it: `PASS`, `FAIL: <reason>` or `SKIP: <reason>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// `PASS`.
    Pass,
    /// `FAIL`, with the reason after `: ` when one is given.
    Fail(Option<String>),
    /// `SKIP`, with the reason after `: ` when one is given.
    Skip(Option<String>),
}

impl Status {
    /// The status string's first word.
    pub fn verdict(&self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail(_) => "FAIL",
            Status::Skip(_) => "SKIP",
        }
    }

    /// Parse a report status string; `None` when it is none of the three forms.
    pub fn parse(text: &str) -> Option<Status> {
        if text == "PASS" {
            return Some(Status::Pass);
        }
        for (verdict, build) in [
            ("FAIL", Status::Fail as fn(Option<String>) -> Status),
            ("SKIP", Status::Skip as fn(Option<String>) -> Status),
        ] {
            if text == verdict {
                return Some(build(None));
            }
            if let Some(reason) = text
                .strip_prefix(verdict)
                .and_then(|r| r.strip_prefix(": "))
            {
                return Some(build(Some(reason.to_string())));
            }
        }
        None
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Pass => f.write_str("PASS"),
            Status::Fail(Some(reason)) => write!(f, "FAIL: {reason}"),
            Status::Fail(None) => f.write_str("FAIL"),
            Status::Skip(Some(reason)) => write!(f, "SKIP: {reason}"),
            Status::Skip(None) => f.write_str("SKIP"),
        }
    }
}

impl Serialize for Status {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Status {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Status::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("not a check status: {text}")))
    }
}
