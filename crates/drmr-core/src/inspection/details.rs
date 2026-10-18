//! Scheme meanings and the limited evidence available to metadata inspection.
use super::{Evidence, Pattern, Protection, Track};
use serde::{Deserialize, Serialize};

/// Effective HLS methods observed in a media playlist, without key material or references.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HlsKeyMethod {
    #[serde(rename = "NONE")]
    None,
    #[serde(rename = "AES-128")]
    Aes128,
    #[serde(rename = "SAMPLE-AES")]
    SampleAes,
    #[serde(rename = "SAMPLE-AES-CTR")]
    SampleAesCtr,
    #[serde(rename = "UNKNOWN")]
    Unknown,
}
impl HlsKeyMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Aes128 => "AES-128",
            Self::SampleAes => "SAMPLE-AES",
            Self::SampleAesCtr => "SAMPLE-AES-CTR",
            Self::Unknown => "UNKNOWN",
        }
    }
    fn declaration(method: &str) -> Self {
        match method {
            "NONE" => Self::None,
            "AES-128" => Self::Aes128,
            "SAMPLE-AES" => Self::SampleAes,
            "SAMPLE-AES-CTR" => Self::SampleAesCtr,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DescriptionEvidence {
    ContainerDefault,
    ManifestDeclaration,
    RequestedTarget,
}
impl DescriptionEvidence {
    pub const fn description(self) -> &'static str {
        match self {
            Self::ContainerDefault => "container default",
            Self::ManifestDeclaration => "manifest declaration",
            Self::RequestedTarget => "requested target",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cipher {
    None,
    #[serde(rename = "aes-128-ctr")]
    Aes128Ctr,
    #[serde(rename = "aes-128-cbc")]
    Aes128Cbc,
    Mixed,
    Unknown,
}
impl Cipher {
    pub const fn description(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Aes128Ctr => "AES-128 in counter mode",
            Self::Aes128Cbc => "AES-128 in cipher block chaining mode",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EncryptionUnit {
    None,
    MediaSample,
    HlsResource,
    Mixed,
    Unknown,
}
impl EncryptionUnit {
    pub const fn description(self) -> &'static str {
        match self {
            Self::None => "none declared",
            Self::MediaSample => "media samples (encoded frames or audio packets)",
            Self::HlsResource => "complete HLS resources",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FactStatus {
    Known,
    Unknown,
    NotApplicable,
}
impl FactStatus {
    pub const fn description(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Unknown => "unknown",
            Self::NotApplicable => "not applicable",
        }
    }
}
/// A default pattern does not establish how any individual sample was encrypted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternDescription {
    pub status: FactStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Pattern>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SampleCoverage {
    Unknown,
    NotApplicable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectionDescription {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
    pub evidence: DescriptionEvidence,
    pub cipher: Cipher,
    pub encryption_unit: EncryptionUnit,
    pub pattern: PatternDescription,
    pub sample_coverage: SampleCoverage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_coverage_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hls_key_methods: Vec<HlsKeyMethod>,
}
impl ProtectionDescription {
    pub(super) fn new(protection: Protection, evidence: DescriptionEvidence) -> Self {
        let (cipher, unit) = match protection {
            Protection::Clear => (Cipher::None, EncryptionUnit::None),
            Protection::Cenc | Protection::Cens => (Cipher::Aes128Ctr, EncryptionUnit::MediaSample),
            Protection::Cbcs | Protection::Cbc1 => (Cipher::Aes128Cbc, EncryptionUnit::MediaSample),
            Protection::HlsAes128 => (Cipher::Aes128Cbc, EncryptionUnit::HlsResource),
            Protection::Mixed => (Cipher::Mixed, EncryptionUnit::Mixed),
            Protection::Unknown => (Cipher::Unknown, EncryptionUnit::Unknown),
        };
        let sample = !matches!(unit, EncryptionUnit::None | EncryptionUnit::HlsResource);
        Self {
            track_id: None,
            evidence,
            cipher,
            encryption_unit: unit,
            pattern: PatternDescription {
                status: if sample {
                    FactStatus::Unknown
                } else {
                    FactStatus::NotApplicable
                },
                value: None,
            },
            sample_coverage: if sample {
                SampleCoverage::Unknown
            } else {
                SampleCoverage::NotApplicable
            },
            sample_coverage_reason: sample.then(|| Self::coverage_reason(evidence).into()),
            hls_key_methods: Vec::new(),
        }
    }
    fn coverage_reason(evidence: DescriptionEvidence) -> &'static str {
        match evidence {
            DescriptionEvidence::RequestedTarget => "target-intent-not-sample-evidence",
            _ => "sample-auxiliary-data-not-inspected",
        }
    }
    pub(super) fn track(track: &Track, evidence: Evidence) -> Self {
        let mut result = Self::new(
            track.protection,
            match evidence {
                Evidence::ContainerMetadata => DescriptionEvidence::ContainerDefault,
                Evidence::Manifest => DescriptionEvidence::ManifestDeclaration,
            },
        );
        result.track_id = Some(track.id.clone());
        if let Some(pattern) = track.pattern {
            result.pattern = PatternDescription {
                status: FactStatus::Known,
                value: Some(pattern),
            };
        }
        result
    }
    pub(super) fn hls_methods(&mut self, methods: &std::collections::BTreeSet<String>) {
        self.hls_key_methods = methods
            .iter()
            .map(|method| HlsKeyMethod::declaration(method))
            .collect();
        let descriptions: Vec<_> = methods
            .iter()
            .map(|method| match method.as_str() {
                "NONE" => (Cipher::None, EncryptionUnit::None),
                "AES-128" => (Cipher::Aes128Cbc, EncryptionUnit::HlsResource),
                "SAMPLE-AES" => (Cipher::Aes128Cbc, EncryptionUnit::MediaSample),
                "SAMPLE-AES-CTR" => (Cipher::Aes128Ctr, EncryptionUnit::MediaSample),
                _ => (Cipher::Unknown, EncryptionUnit::Unknown),
            })
            .collect();
        if let Some(first) = descriptions.first() {
            self.cipher = if descriptions.iter().all(|d| d.0 == first.0) {
                first.0
            } else {
                Cipher::Mixed
            };
            self.encryption_unit = if descriptions.iter().all(|d| d.1 == first.1) {
                first.1
            } else {
                EncryptionUnit::Mixed
            };
            let sample = !matches!(
                self.encryption_unit,
                EncryptionUnit::None | EncryptionUnit::HlsResource
            );
            self.sample_coverage = if sample {
                SampleCoverage::Unknown
            } else {
                SampleCoverage::NotApplicable
            };
            self.sample_coverage_reason =
                sample.then(|| Self::coverage_reason(self.evidence).into());
            self.pattern.status = if sample {
                FactStatus::Unknown
            } else {
                FactStatus::NotApplicable
            };
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodCount {
    pub status: FactStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}
impl PeriodCount {
    pub(super) fn not_applicable() -> Self {
        Self {
            status: FactStatus::NotApplicable,
            count: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectionDetails {
    pub track_protection: Vec<ProtectionDescription>,
    pub periods: PeriodCount,
}
