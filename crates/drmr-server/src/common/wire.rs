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
    HlsUrl {
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
