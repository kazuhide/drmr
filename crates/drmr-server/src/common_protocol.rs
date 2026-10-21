//! Private, bounded transport between the console and the existing CLI handlers.
//!
//! This protocol carries resolved operator paths. It is never an HTTP response.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fmt,
    path::{Component, Path, PathBuf},
};

pub const JOB_ARGUMENT: &str = "--internal-common-job-v1";
pub const REQUEST_LIMIT: usize = 256 * 1024;
pub const RESULT_LIMIT: usize = 64 * 1024 * 1024;
pub const PROGRESS_LIMIT: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationName {
    Analyze,
    Plan,
    Convert,
    Verify,
}
impl OperationName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Analyze => "analyze",
            Self::Plan => "plan",
            Self::Convert => "convert",
            Self::Verify => "verify",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ChildSource {
    Local {
        input_file: PathBuf,
    },
    #[serde(alias = "hls-url")]
    Url {
        url: String,
        download_dir: PathBuf,
        #[serde(deserialize_with = "required_nullable")]
        fetch_policy_file: Option<PathBuf>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ChildOperation {
    Analyze {
        source: ChildSource,
    },
    Plan {
        input_file: PathBuf,
        output_root: PathBuf,
        save_file: PathBuf,
        output_spec_file: PathBuf,
    },
    Convert {
        plan_file: PathBuf,
    },
    Verify {
        plan_file: PathBuf,
    },
}
impl ChildOperation {
    pub fn name(&self) -> OperationName {
        match self {
            Self::Analyze { .. } => OperationName::Analyze,
            Self::Plan { .. } => OperationName::Plan,
            Self::Convert { .. } => OperationName::Convert,
            Self::Verify { .. } => OperationName::Verify,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorTools {
    pub packager_bin: String,
    pub hls_validator_bin: String,
    pub dash_validator_bin: String,
}
impl Default for OperatorTools {
    fn default() -> Self {
        Self {
            packager_bin: "packager".into(),
            hls_validator_bin: "mediastreamvalidator".into(),
            dash_validator_bin: "php".into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRuntime {
    #[serde(deserialize_with = "required_nullable")]
    pub keys_file: Option<PathBuf>,
    #[serde(deserialize_with = "required_nullable")]
    pub max_workers: Option<u8>,
    pub tools: OperatorTools,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildChannels {
    pub result_file: PathBuf,
    pub progress_file: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRequest {
    pub protocol_version: u8,
    pub nonce: String,
    pub operation: ChildOperation,
    pub runtime: ChildRuntime,
    pub channels: ChildChannels,
}
impl ChildRequest {
    /// Validate transport invariants before constructing CLI arguments directly.
    pub fn validate(&self, request_file: &Path) -> Result<(), ProtocolError> {
        let absolute = |p: &Path| {
            p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
        };
        let optional = |p: &Option<PathBuf>| p.as_deref().is_none_or(absolute);
        let parent = request_file.parent().ok_or(ProtocolError::Invalid)?;
        let channel = |p: &Path, name: &str| {
            absolute(p) && p.parent() == Some(parent) && p.file_name().is_some_and(|n| n == name)
        };
        let paths_valid = match &self.operation {
            ChildOperation::Analyze {
                source: ChildSource::Local { input_file },
            } => absolute(input_file),
            ChildOperation::Analyze {
                source:
                    ChildSource::Url {
                        url,
                        download_dir,
                        fetch_policy_file,
                    },
            } => !url.is_empty() && absolute(download_dir) && optional(fetch_policy_file),
            ChildOperation::Plan {
                input_file,
                output_root,
                save_file,
                output_spec_file,
            } => [input_file, output_root, save_file, output_spec_file]
                .iter()
                .all(|p| absolute(p)),
            ChildOperation::Convert { plan_file } | ChildOperation::Verify { plan_file } => {
                absolute(plan_file)
            }
        };
        if self.protocol_version != 1
            || !correlation_id(&self.nonce)
            || !absolute(request_file)
            || !optional(&self.runtime.keys_file)
            || self
                .runtime
                .max_workers
                .is_some_and(|n| !(1..=64).contains(&n))
            || [
                &self.runtime.tools.packager_bin,
                &self.runtime.tools.hls_validator_bin,
                &self.runtime.tools.dash_validator_bin,
            ]
            .iter()
            .any(|p| p.is_empty() || p.contains('\0'))
            || !channel(&self.channels.result_file, "result.json")
            || !channel(&self.channels.progress_file, "progress.json")
            || !paths_valid
        {
            return Err(ProtocolError::Invalid);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildResult {
    pub protocol_version: u8,
    pub nonce: String,
    pub operation: OperationName,
    pub payload: Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildProgress {
    pub protocol_version: u8,
    pub nonce: String,
    pub operation: OperationName,
    pub sequence: u64,
    pub elapsed_ms: u64,
    pub last_activity: Option<String>,
    pub scopes_started: u64,
    pub scopes_closed: u64,
    pub workers: Option<u64>,
}
impl ChildProgress {
    pub fn valid(&self, nonce: &str, operation: OperationName) -> bool {
        self.protocol_version == 1
            && self.nonce == nonce
            && self.operation == operation
            && self.last_activity.as_deref().is_none_or(activity)
            && self.workers.is_none_or(|n| (1..=64).contains(&n))
    }
}

pub fn activity(value: &str) -> bool {
    matches!(
        value,
        "configuration"
            | "acquisition"
            | "input-preparation"
            | "inspection"
            | "normalization"
            | "packaging"
            | "verification"
            | "publication"
    )
}
pub fn correlation_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A present JSON null is allowed; an omitted request field is an error.
pub fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    TooLarge,
    Invalid,
}
impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "request-too-large",
            Self::Invalid => "invalid-json",
        })
    }
}
impl std::error::Error for ProtocolError {}

/// Inspect original bytes before typed parsing can collapse an object into a map.
pub fn decode<T: DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T, ProtocolError> {
    if bytes.len() > limit {
        return Err(ProtocolError::TooLarge);
    }
    serde_json::from_slice::<UniqueJson>(bytes).map_err(|_| ProtocolError::Invalid)?;
    serde_json::from_slice(bytes).map_err(|_| ProtocolError::Invalid)
}
struct UniqueJson;
impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueJson;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("one JSON value with unique object fields")
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<UniqueJson, A::Error> {
                while a.next_element::<UniqueJson>()?.is_some() {}
                Ok(UniqueJson)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<UniqueJson, A::Error> {
                let mut keys = BTreeSet::new();
                while let Some(key) = a.next_key::<String>()? {
                    if !keys.insert(key) {
                        return Err(serde::de::Error::custom("duplicate-json-field"));
                    }
                    a.next_value::<UniqueJson>()?;
                }
                Ok(UniqueJson)
            }
        }
        d.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_urls_share_one_canonical_kind_and_accept_the_legacy_hls_alias() {
        let directory = std::env::temp_dir().join("retained-source");
        for file in ["master.m3u8", "manifest.mpd", "source.mp4"] {
            for kind in ["url", "hls-url"] {
                let source = serde_json::json!({"kind":kind,
                    "url":format!("https://example.invalid/{file}"),
                    "download_dir":directory,"fetch_policy_file":null});
                let parsed: ChildSource =
                    decode(&serde_json::to_vec(&source).unwrap(), REQUEST_LIMIT).unwrap();
                assert!(matches!(parsed, ChildSource::Url { .. }));
                let mut canonical = source;
                canonical["kind"] = "url".into();
                assert_eq!(serde_json::to_value(parsed).unwrap(), canonical);
            }
        }
    }
    #[test]
    fn duplicate_decoded_keys_are_rejected_at_every_depth() {
        for raw in [r#"{"a":{"x":1,"x":2}}"#, r#"{"a":[{"x":1,"\u0078":2}]}"#] {
            assert_eq!(
                decode::<Value>(raw.as_bytes(), 1024).unwrap_err(),
                ProtocolError::Invalid
            );
        }
        assert!(decode::<Value>(br#"{"a":[{"x":1},{"x":2}]}"#, 1024).is_ok());
    }
    #[test]
    fn trailing_values_and_oversized_bodies_are_not_accepted() {
        assert_eq!(
            decode::<Value>(b"{}{}", 1024).unwrap_err(),
            ProtocolError::Invalid
        );
        assert_eq!(
            decode::<Value>(b"{}", 1).unwrap_err(),
            ProtocolError::TooLarge
        );
    }
    #[test]
    fn nullable_request_fields_must_be_present_and_unknown_fields_fail() {
        let good = br#"{"keys_file":null,"max_workers":null,"tools":{"packager_bin":"packager","hls_validator_bin":"mediastreamvalidator","dash_validator_bin":"php"}}"#;
        assert!(decode::<ChildRuntime>(good, REQUEST_LIMIT).is_ok());
        for field in ["keys_file", "max_workers"] {
            let mut value: Value = serde_json::from_slice(good).unwrap();
            value.as_object_mut().unwrap().remove(field);
            assert!(
                decode::<ChildRuntime>(&serde_json::to_vec(&value).unwrap(), REQUEST_LIMIT)
                    .is_err()
            );
        }
        let raw = br#"{"kind":"verify","plan_file":"/plan.json","overwrite":true}"#;
        assert!(decode::<ChildOperation>(raw, REQUEST_LIMIT).is_err());
    }
    #[test]
    fn progress_cannot_cross_an_operation_or_publish_arbitrary_activity() {
        let mut p = ChildProgress {
            protocol_version: 1,
            nonce: "a".repeat(32),
            operation: OperationName::Verify,
            sequence: 1,
            elapsed_ms: 1,
            last_activity: Some("verification".into()),
            scopes_started: 2,
            scopes_closed: 1,
            workers: Some(2),
        };
        assert!(p.valid(&"a".repeat(32), OperationName::Verify));
        assert!(!p.valid(&"b".repeat(32), OperationName::Verify));
        assert!(!p.valid(&"a".repeat(32), OperationName::Convert));
        p.last_activity = Some("private-source-path".into());
        assert!(!p.valid(&"a".repeat(32), OperationName::Verify));
    }
}
