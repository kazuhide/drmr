//! Strict control messages. Native paths never enter this HTTP vocabulary.
use crate::common_protocol::{OperationName, required_nullable};
use drmr_core::conversion::plan::bundle::BundleOutputIntent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Media,
    SavedPlan,
    Keys,
    FetchPolicy,
    OutputParent,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Source {
    Local {
        input_ref: String,
    },
    Url {
        url: String,
        #[serde(deserialize_with = "required_nullable")]
        fetch_policy_ref: Option<String>,
    },
    SavedPlan {
        plan_file_ref: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    pub schema_version: u8,
    pub server_id: String,
    pub creation_request_id: String,
    pub source: Source,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    Analyze,
    Plan {
        analysis_ref: String,
        output_parent_ref: String,
        destination_name: String,
        outputs: Vec<BundleOutputIntent>,
    },
    Convert {
        plan_ref: String,
    },
    Verify {
        plan_ref: String,
    },
}
impl Operation {
    pub fn name(&self) -> OperationName {
        match self {
            Self::Analyze => OperationName::Analyze,
            Self::Plan { .. } => OperationName::Plan,
            Self::Convert { .. } => OperationName::Convert,
            Self::Verify { .. } => OperationName::Verify,
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    #[serde(deserialize_with = "required_nullable")]
    pub keys_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub max_workers: Option<u8>,
    #[serde(default)]
    pub decode: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitJob {
    pub schema_version: u8,
    pub server_id: String,
    pub session_id: String,
    pub request_id: String,
    pub operation: Operation,
    pub runtime: Runtime,
}
#[derive(Clone, Copy, Debug)]
pub struct Error {
    pub status: u16,
    pub code: &'static str,
}
impl Error {
    pub const fn new(status: u16, code: &'static str) -> Self {
        Self { status, code }
    }
    pub fn value(self, job: Option<&str>, activity: Option<&str>) -> Value {
        json!({"code":self.code,"hint":self.hint(),"job_id":job,"last_activity":activity})
    }
    fn hint(self) -> &'static str {
        match self.code {
            "request-conflict" => "This request identifier already belongs to another intent.",
            "global-job-busy" | "session-busy" => {
                "Observe the current job before starting another operation."
            }
            "cleanup-unconfirmed" => {
                "Keep the current owner and confirm cleanup before continuing."
            }
            "request-limit-reached" | "session-limit-reached" => {
                "Remove a finished session and explicitly import its saved plan again."
            }
            "analysis-source-changed" => "Analyze the changed source again and save a new plan.",
            "destination-exists" => {
                "Use independent verification for existing output, or save a plan with a new destination."
            }
            "result-invalid" => {
                "The owned result could not be confirmed. Keep the saved plan and inspect the current output before continuing."
            }
            "private-config-unavailable" => {
                "Select a protected configuration from the operator's private configuration folder."
            }
            "worker-limit-exceeded" => "Choose a worker limit within the server maximum.",
            "server-shutting-down" => {
                "The server is shutting down. Continue observing its current job."
            }
            _ => {
                "The operation could not be confirmed. Check the current session and explicitly choose the next action."
            }
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub fn invalid() -> Error {
    Error::new(400, "invalid-request")
}
pub fn unavailable() -> Error {
    Error::new(404, "reference-not-found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_decode_defaults_to_native_and_is_part_of_request_identity() {
        let native = json!({"keys_ref":null,"max_workers":null});
        let parsed: Runtime = serde_json::from_value(native.clone()).unwrap();
        assert!(!parsed.decode);
        let mut decoded = native.clone();
        decoded["decode"] = json!(true);
        let decoded: Runtime = serde_json::from_value(decoded).unwrap();
        assert!(decoded.decode);
        assert!(parsed != decoded);
        for invalid in [Value::Null, json!("true"), json!(1)] {
            let mut value = native.clone();
            value["decode"] = invalid;
            assert!(serde_json::from_value::<Runtime>(value).is_err());
        }
    }

    #[test]
    fn source_urls_refuse_retired_aliases_and_keep_strict_fields() {
        for file in ["master.m3u8", "manifest.mpd", "source.mp4"] {
            let canonical = json!({"kind":"url",
                "url":format!("https://example.invalid/{file}"),"fetch_policy_ref":null});
            let current: Source = serde_json::from_value(canonical.clone()).unwrap();
            let mut legacy = canonical.clone();
            legacy["kind"] = "hls-url".into();
            assert!(serde_json::from_value::<Source>(legacy).is_err());
            assert_eq!(serde_json::to_value(current).unwrap(), canonical);

            let mut missing = canonical.clone();
            missing.as_object_mut().unwrap().remove("fetch_policy_ref");
            assert!(serde_json::from_value::<Source>(missing).is_err());
            let mut extra = canonical;
            extra["download_dir"] = "/private-path".into();
            assert!(serde_json::from_value::<Source>(extra).is_err());
        }
    }
}
