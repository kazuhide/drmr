//! Atomic intent admission and ownership-bound observation of existing CLI jobs.
use super::{
    CommonService, Session, State,
    child::{Preparation, Prepared},
    files::{self, BoundPath},
    plans, projection,
    wire::{self, Error, Operation, Result, Source, SubmitJob, unavailable},
};
use crate::{
    common_protocol::{self as protocol, ChildOperation, ChildSource, OperationName},
    http::Reply,
    jobs::JobError,
    pyutil,
};
use drmr_core::conversion::{SourceInventory, plan::bundle::OutputProcessing};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

pub(super) struct Claim {
    pub request: SubmitJob,
    pub state: &'static str,
    pub job: Option<String>,
    pub error: Option<Error>,
}
pub(super) struct Analysis {
    pub id: String,
    pub session: String,
    pub job: String,
    pub source_ref: String,
    pub source: BoundPath,
    pub report: String,
    pub raw: Arc<Prepared>,
}
pub(super) struct Job {
    pub id: String,
    pub session: String,
    pub request: String,
    pub operation: OperationName,
    pub revision: u64,
    pub state: &'static str,
    pub stop: bool,
    pub supervision: bool,
    pub cleanup: bool,
    pub exit: Option<i32>,
    pub source_ref: Option<String>,
    pub analysis: Option<String>,
    pub plan: Option<String>,
    pub plan_id: Option<String>,
    pub report: Option<String>,
    pub progress: Option<protocol::ChildProgress>,
    pub error: Option<Error>,
    pub prepared: Arc<Prepared>,
    pub input: Option<BoundPath>,
    pub expected_inventory: Option<Arc<Prepared>>,
    pub expected_plan: Option<plans::Plan>,
    pub phase: Phase,
    pub public_report: Option<(BoundPath, String)>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Observing,
    Finalizing,
    Terminal,
}
pub(super) struct RetainedPreparation {
    session: String,
    request: String,
    error: Error,
    prepared: Option<Arc<Prepared>>,
}
struct Intent {
    input: Option<BoundPath>,
    fetch: Option<BoundPath>,
    keys: Option<BoundPath>,
    plan: Option<plans::Plan>,
    analysis: Option<String>,
    expected_inventory: Option<Arc<Prepared>>,
    operation: ChildOperation,
    outputs: Option<Vec<drmr_core::conversion::plan::bundle::BundleOutputIntent>>,
    maximum: u8,
    decode: bool,
}
struct Completion {
    id: String,
    prepared: Arc<Prepared>,
    observation: crate::jobs::JobObservation,
    operation: OperationName,
    schema: Option<u32>,
    plan_id: Option<String>,
    session: String,
    request: String,
    expected_inventory: Option<Arc<Prepared>>,
    expected_plan: Option<plans::Plan>,
    input: Option<BoundPath>,
    origin_analysis: Option<String>,
}
struct Validated {
    projected: projection::Projected,
    plan: Option<plans::Plan>,
    source: Option<(BoundPath, String)>,
    payload_version: Value,
}
struct Published {
    success: bool,
    kind: &'static str,
    reference: String,
    binding: BoundPath,
    digest: String,
    plan: Option<plans::Plan>,
    source: Option<(BoundPath, String)>,
}
impl Intent {
    fn new(operation: ChildOperation) -> Self {
        Self {
            input: None,
            fetch: None,
            keys: None,
            plan: None,
            analysis: None,
            expected_inventory: None,
            operation,
            outputs: None,
            maximum: 64,
            decode: false,
        }
    }
}
impl CommonService {
    pub(super) fn receipt(&self, s: &Session, c: &Claim) -> Value {
        self.envelope(json!({"session_id":s.id,"request_id":c.request.request_id,"operation":c.request.operation.name(),"state":c.state,"job_id":c.job,"error":c.error.map(|e|e.value(c.job.as_deref(),None))}))
    }
    fn job_value(&self, j: &Job) -> Value {
        self.envelope(json!({"job_id":j.id,"session_id":j.session,"request_id":j.request,"operation":j.operation,"revision":j.revision,"state":j.state,"stop_requested":j.stop,"supervision_confirmed":j.supervision,"cleanup_confirmed":j.cleanup,"exit_code":j.exit,"source_ref":j.source_ref,"analysis_ref":j.analysis,"plan_ref":j.plan,"plan_id":j.plan_id,"report_ref":j.report,"progress":j.progress.as_ref().map(|p|json!({"sequence":p.sequence,"elapsed_ms":p.elapsed_ms,"last_activity":p.last_activity,"scopes_started":p.scopes_started,"scopes_closed":p.scopes_closed,"workers":p.workers})),"error":j.error.map(|e|e.value(Some(&j.id),j.progress.as_ref().and_then(|p|p.last_activity.as_deref())))}))
    }
    pub(super) fn submit(self: &Arc<Self>, bytes: &[u8]) -> Result<Value> {
        let mut request: SubmitJob =
            protocol::decode(bytes, protocol::REQUEST_LIMIT).map_err(|_| wire::invalid())?;
        if request.schema_version != 1 || !protocol::correlation_id(&request.request_id) {
            return Err(wire::invalid());
        }
        if request.server_id != self.id {
            return Err(Error::new(409, "server-changed"));
        }
        if let Operation::Plan { outputs, .. } = &mut request.operation {
            for o in outputs {
                o.drms.sort();
            }
        }
        if let Some(replay) = self.claim(&request)? {
            return Ok(replay);
        }
        let token = pyutil::uuid4_hex();
        if let Err(error) = self.manager.reserve_common(&token) {
            let error = manager_error(&error);
            self.reject(&request, error);
            return Err(error);
        }
        let intent = match self.prepare_intent(&request, &token) {
            Ok(intent) => intent,
            Err(error) => return Err(self.admission_failed(&request, &token, None, error)),
        };
        let mut prepared =
            match Prepared::allocate(&self.private, token.clone(), intent.operation.clone()) {
                Ok(prepared) => prepared,
                Err(error) => return Err(self.admission_failed(&request, &token, None, error)),
            };
        if let Err(error) = prepared.configure(Preparation {
            keys: intent.keys.as_ref(),
            fetch: intent.fetch.as_ref(),
            plan: intent.plan.as_ref(),
            outputs: intent.outputs.as_deref(),
            maximum: intent.maximum,
            decode: intent.decode,
        }) {
            return Err(self.admission_failed(&request, &token, Some(prepared), error));
        }
        let (handle, supervision, error) =
            match self.manager.submit_common(&token, &prepared.request) {
                Ok(handle) => (handle, true, None),
                Err(JobError::SupervisionFailed(handle)) => (
                    handle,
                    false,
                    Some(Error::new(503, "job-supervision-failed")),
                ),
                Err(error) => {
                    return Err(self.admission_failed(
                        &request,
                        &token,
                        Some(prepared),
                        manager_error(&error),
                    ));
                }
            };
        self.register(&request, intent, prepared, &handle, supervision, error)
    }
    fn claim(&self, request: &SubmitJob) -> Result<Option<Value>> {
        let mut s = self.lock();
        if s.closing {
            return Err(Error::new(503, "server-shutting-down"));
        }
        let session = s
            .sessions
            .get_mut(&request.session_id)
            .ok_or(Error::new(404, "session-not-found"))?;
        if let Some(c) = session.requests.get(&request.request_id) {
            if c.request.operation != request.operation || c.request.runtime != request.runtime {
                return Err(Error::new(409, "request-conflict"));
            }
            if let Some(id) = c.job.clone() {
                return s
                    .jobs
                    .get(&id)
                    .map(|j| Some(self.job_value(j)))
                    .ok_or_else(unavailable);
            }
            return Err(c.error.unwrap_or(Error::new(409, "request-pending")));
        }
        if session.requests.len() >= 32 {
            return Err(Error::new(409, "request-limit-reached"));
        }
        session.requests.insert(
            request.request_id.clone(),
            Claim {
                request: request.clone(),
                state: "pending",
                job: None,
                error: None,
            },
        );
        if session.active.is_some() {
            let c = session
                .requests
                .get_mut(&request.request_id)
                .ok_or_else(unavailable)?;
            c.state = "rejected";
            c.error = Some(Error::new(409, "session-busy"));
            return Err(Error::new(409, "session-busy"));
        }
        session.active = Some(request.request_id.clone());

        Ok(None)
    }
    fn register(
        self: &Arc<Self>,
        request: &SubmitJob,
        intent: Intent,
        prepared: Prepared,
        handle: &crate::jobs::JobHandle,
        supervision: bool,
        error: Option<Error>,
    ) -> Result<Value> {
        let id = handle.job_id.clone();
        let origin_analysis = intent
            .analysis
            .clone()
            .or_else(|| intent.plan.as_ref().and_then(|p| p.analysis_ref.clone()));
        let source_ref = {
            let s = self.lock();
            match &request.operation {
                Operation::Analyze => match &s
                    .sessions
                    .get(&request.session_id)
                    .ok_or_else(unavailable)?
                    .source
                {
                    Source::Local { input_ref } => Some(input_ref.clone()),
                    _ => None,
                },
                _ => origin_analysis
                    .as_ref()
                    .and_then(|id| s.analyses.get(id))
                    .map(|a| a.source_ref.clone())
                    .or_else(|| intent.plan.as_ref().and_then(|p| p.source_ref.clone())),
            }
        };
        let job = Job {
            id: id.clone(),
            session: request.session_id.clone(),
            request: request.request_id.clone(),
            operation: request.operation.name(),
            revision: 0,
            state: "running",
            stop: handle.stop_requested,
            supervision,
            cleanup: false,
            exit: None,
            source_ref,
            analysis: origin_analysis,
            plan: intent.plan.as_ref().map(|p| p.reference.clone()),
            plan_id: intent.plan.as_ref().map(|p| p.id.clone()),
            report: None,
            progress: None,
            error,
            prepared: Arc::new(prepared),
            input: intent.input,
            expected_inventory: intent.expected_inventory,
            expected_plan: intent.plan,
            phase: Phase::Observing,
            public_report: None,
        };
        let view = self.job_value(&job);
        {
            let mut s = self.lock();
            let session = s
                .sessions
                .get_mut(&request.session_id)
                .ok_or_else(unavailable)?;
            let claim = session
                .requests
                .get_mut(&request.request_id)
                .ok_or_else(unavailable)?;
            claim.state = "accepted";
            claim.job = Some(id.clone());
            s.jobs.insert(id.clone(), job);
        }
        self.monitor(&id);
        Ok(view)
    }
    fn monitor(self: &Arc<Self>, id: &str) {
        if self.config.hooks.monitor {
            let service = self.clone();
            let owner = id.to_owned();
            if self
                .workers
                .spawn("common-job-monitor", move || {
                    loop {
                        service.refresh(&owner);
                        let done = service
                            .lock()
                            .jobs
                            .get(&owner)
                            .is_none_or(|j| j.phase == Phase::Terminal);
                        if done || !service.workers.wait(Duration::from_millis(100)) {
                            break;
                        }
                    }
                })
                .is_err()
            {
                let _ = self.manager.request_stop(id);
            }
        }
    }
    fn reject(&self, r: &SubmitJob, error: Error) {
        Self::reject_claim(&mut self.lock(), &r.session_id, &r.request_id, error);
    }
    fn reject_claim(s: &mut State, session_id: &str, request_id: &str, error: Error) {
        if let Some(session) = s.sessions.get_mut(session_id) {
            if let Some(c) = session.requests.get_mut(request_id) {
                c.state = "rejected";
                c.error = Some(error);
            }
            if session.active.as_deref() == Some(request_id) {
                session.active = None;
            }
        }
    }
    fn owns_pending(s: &State, session: &str, request: &str) -> bool {
        s.sessions.get(session).is_some_and(|session| {
            session.active.as_deref() == Some(request)
                && session
                    .requests
                    .get(request)
                    .is_some_and(|c| c.state == "pending" && c.job.is_none())
        })
    }
    fn admission_failed(
        &self,
        request: &SubmitJob,
        token: &str,
        prepared: Option<Prepared>,
        error: Error,
    ) -> Error {
        let mut s = self.lock();
        // The same mutex fences DELETE, replay, cleanup, and ownership release.
        if Self::owns_pending(&s, &request.session_id, &request.request_id)
            && prepared.as_ref().is_none_or(|p| p.discard().is_ok())
            && self.manager.release_common_preparation(token).is_ok()
        {
            Self::reject_claim(&mut s, &request.session_id, &request.request_id, error);
            return error;
        }
        let held = Error::new(503, "cleanup-unconfirmed");
        let _ = self.manager.retain_common_preparation(token);
        if let Some(c) = s
            .sessions
            .get_mut(&request.session_id)
            .and_then(|session| session.requests.get_mut(&request.request_id))
        {
            c.error = Some(held);
        }
        s.preparations.insert(
            token.into(),
            RetainedPreparation {
                session: request.session_id.clone(),
                request: request.request_id.clone(),
                error,
                prepared: prepared.map(Arc::new),
            },
        );
        held
    }
    fn refresh_preparations(&self) {
        let mut s = self.lock();
        let tokens: Vec<_> = s.preparations.keys().cloned().collect();
        for token in tokens {
            let Some(p) = s.preparations.get(&token) else {
                continue;
            };
            if !Self::owns_pending(&s, &p.session, &p.request)
                || p.prepared.as_ref().is_some_and(|p| p.discard().is_err())
            {
                continue;
            }
            if self.manager.release_common_preparation(&token).is_err() {
                continue;
            }
            if let Some(p) = s.preparations.remove(&token) {
                Self::reject_claim(&mut s, &p.session, &p.request, p.error);
            }
        }
    }
    fn prepare_intent(&self, request: &SubmitJob, nonce: &str) -> Result<Intent> {
        let intent = {
            let s = self.lock();
            let session = s
                .sessions
                .get(&request.session_id)
                .ok_or_else(unavailable)?;
            let maximum =
                u8::try_from(self.config.max_workers.unwrap_or(64)).map_err(|_| wire::invalid())?;
            if request
                .runtime
                .max_workers
                .is_some_and(|n| n == 0 || n > maximum)
            {
                return Err(Error::new(422, "worker-limit-exceeded"));
            }
            let mut intent = match &request.operation {
                Operation::Analyze => self.prepare_analysis(&s, session, nonce)?,
                Operation::Plan { .. } => self.prepare_plan(&s, session, request, nonce)?,
                Operation::Convert { .. } | Operation::Verify { .. } => {
                    Self::prepare_execution(&s, session, request)?
                }
            };
            intent.keys = request
                .runtime
                .keys_ref
                .as_deref()
                .map(|id| s.files.file(id, wire::Role::Keys))
                .transpose()?;
            intent.maximum = request.runtime.max_workers.unwrap_or(maximum);
            intent.decode = request.runtime.decode;
            intent
        };
        Ok(intent)
    }
    fn prepare_analysis(&self, s: &State, session: &Session, nonce: &str) -> Result<Intent> {
        match &session.source {
            Source::Local { .. } => {
                let input = session.source_binding.clone().ok_or_else(unavailable)?;
                input.check()?;
                let mut intent = Intent::new(ChildOperation::Analyze {
                    source: ChildSource::Local {
                        input_file: input.path.clone(),
                    },
                });
                intent.input = Some(input);
                Ok(intent)
            }
            Source::Url {
                url,
                fetch_policy_ref,
            } => {
                let mut intent = Intent::new(ChildOperation::Analyze {
                    source: ChildSource::Url {
                        url: url.clone(),
                        download_dir: self.retained.join("downloads").join(nonce),
                        fetch_policy_file: None,
                    },
                });
                intent.fetch = fetch_policy_ref
                    .as_deref()
                    .map(|id| s.files.file(id, wire::Role::FetchPolicy))
                    .transpose()?;
                Ok(intent)
            }
            Source::SavedPlan { .. } => Err(Error::new(422, "selection-not-permitted")),
        }
    }
    fn prepare_plan(
        &self,
        s: &State,
        session: &Session,
        request: &SubmitJob,
        nonce: &str,
    ) -> Result<Intent> {
        let Operation::Plan {
            analysis_ref,
            output_parent_ref,
            destination_name,
            outputs,
        } = &request.operation
        else {
            return Err(wire::invalid());
        };
        if !safe_leaf(destination_name) || !(1..=8).contains(&outputs.len()) {
            return Err(wire::invalid());
        }
        let a = s
            .analyses
            .get(analysis_ref)
            .filter(|a| a.session == session.id)
            .ok_or_else(unavailable)?;
        a.source.check()?;
        let parent = s.files.output_parent(output_parent_ref)?;
        let destination = parent.path.join(destination_name);
        s.files.allowed_output(&destination)?;
        if destination.try_exists().map_err(|_| unavailable())? {
            return Err(Error::new(409, "destination-exists"));
        }
        let mut intent = Intent::new(ChildOperation::Plan {
            input_file: a.source.path.clone(),
            output_root: destination,
            save_file: self.retained.join("plans").join(format!("{nonce}.json")),
            output_spec_file: self.private.join(nonce).join("output-spec.json"),
            processing: None,
        });
        intent.input = Some(a.source.clone());
        intent.analysis = Some(a.id.clone());
        intent.expected_inventory = Some(a.raw.clone());
        intent.outputs = Some(outputs.clone());
        Ok(intent)
    }
    fn prepare_execution(s: &State, session: &Session, request: &SubmitJob) -> Result<Intent> {
        let (Operation::Convert { plan_ref } | Operation::Verify { plan_ref }) = &request.operation
        else {
            return Err(wire::invalid());
        };
        let p = s
            .plans
            .get(plan_ref)
            .filter(|p| p.session == session.id)
            .cloned()
            .ok_or_else(unavailable)?;
        p.read()?;
        plans::load(&p.binding.path, &s.files, &session.id, p.reference.clone())?;
        let convert = matches!(&request.operation, Operation::Convert { .. });
        if convert && p.destination.try_exists().map_err(|_| unavailable())? {
            return Err(Error::new(409, "destination-exists"));
        }
        let operation = if convert {
            ChildOperation::Convert {
                plan_file: p.binding.path.clone(),
                processing: None,
            }
        } else {
            ChildOperation::Verify {
                plan_file: p.binding.path.clone(),
            }
        };
        let mut intent = Intent::new(operation);
        intent.plan = Some(p);
        Ok(intent)
    }
    pub(super) fn refresh_active(&self) {
        self.refresh_preparations();
        let ids: Vec<_> = self
            .lock()
            .jobs
            .values()
            .filter(|j| j.phase != Phase::Terminal)
            .map(|j| j.id.clone())
            .collect();
        for id in ids {
            self.refresh(&id);
        }
    }
    fn refresh(&self, id: &str) {
        let Some(completion) = self.completion(id) else {
            return;
        };
        // Native cleanup precedes opening the immutable result; FileIdentity and nonce are both checked.
        let result = self.validate_result(&completion);
        if completion.prepared.cleanup().is_err() {
            let mut s = self.lock();
            if let Some(j) = s.jobs.get_mut(id) {
                j.phase = Phase::Observing;
                j.error = Some(Error::new(503, "cleanup-unconfirmed"));
                j.revision = j.revision.saturating_add(1);
            }
            return;
        }
        let result = result.and_then(|validated| self.publish(&completion, validated));
        self.finish(&completion, result);
    }
    fn completion(&self, id: &str) -> Option<Completion> {
        let prepared = self
            .lock()
            .jobs
            .get(id)
            .filter(|j| j.phase == Phase::Observing)?
            .prepared
            .clone();
        let observation = match self.manager.observation(id) {
            Ok(o) => o,
            Err(e) => {
                if let Some(j) = self.lock().jobs.get_mut(id) {
                    j.error = Some(manager_error(&e));
                    j.revision = j.revision.saturating_add(1);
                }
                return None;
            }
        };
        let progress = prepared.progress();
        let mut s = self.lock();
        let j = s.jobs.get_mut(id).filter(|j| j.phase == Phase::Observing)?;
        let old = (
            j.stop,
            j.supervision,
            j.exit,
            j.progress.as_ref().map(|p| p.sequence),
        );
        j.stop = observation.handle.stop_requested;
        j.supervision = observation.supervision_confirmed;
        j.exit = observation.handle.exit_code;
        if let Some(p) = progress.filter(|p| {
            j.progress
                .as_ref()
                .is_none_or(|old| p.sequence > old.sequence && p.elapsed_ms >= old.elapsed_ms)
        }) {
            j.progress = Some(p);
        }
        if old
            != (
                j.stop,
                j.supervision,
                j.exit,
                j.progress.as_ref().map(|p| p.sequence),
            )
        {
            j.revision = j.revision.saturating_add(1);
        }
        if !observation.cleanup_confirmed {
            return None;
        }
        j.phase = Phase::Finalizing;
        Some(Completion {
            id: id.into(),
            prepared,
            observation,
            operation: j.operation,
            schema: j.expected_plan.as_ref().map(|p| p.schema),
            plan_id: j.plan_id.clone(),
            session: j.session.clone(),
            request: j.request.clone(),
            expected_inventory: j.expected_inventory.clone(),
            expected_plan: j.expected_plan.clone(),
            input: j.input.clone(),
            origin_analysis: j.analysis.clone(),
        })
    }
    fn validate_result(&self, c: &Completion) -> Result<Validated> {
        let result = c.prepared.read_result()?;
        let projected =
            projection::project(c.operation, c.schema, c.plan_id.as_deref(), &result.payload)?;
        if let Some(plan) = &c.expected_plan
            && matches!(plan.schema, 2 | 4 | 6)
            && projected.kind != "failure"
        {
            let ids = |v: &Value| -> Result<Vec<Value>> {
                Ok(v.as_array()
                    .ok_or_else(unavailable)?
                    .iter()
                    .map(|o| o["id"].clone())
                    .collect())
            };
            if ids(&plan.outputs)? != ids(&result.payload["outputs"])? {
                return Err(Error::new(503, "result-invalid"));
            }
        }
        let plan = if c.operation == OperationName::Plan && projected.success {
            Some(self.validate_new_plan(c, &result.payload)?)
        } else {
            None
        };
        let source = if c.operation == OperationName::Analyze && projected.success {
            Some(self.validate_analysis_source(c, &result.payload)?)
        } else {
            None
        };
        Ok(Validated {
            projected,
            plan,
            source,
            payload_version: result.payload["schema_version"].clone(),
        })
    }
    fn validate_new_plan(&self, c: &Completion, payload: &Value) -> Result<plans::Plan> {
        let ChildOperation::Plan { save_file, .. } = &c.prepared.operation else {
            return Err(unavailable());
        };
        let mut plan = {
            let s = self.lock();
            plans::load(save_file, &s.files, &c.session, pyutil::uuid4_hex())?
        };
        plan.analysis_ref.clone_from(&c.origin_analysis);
        plan.source_ref = c
            .origin_analysis
            .as_ref()
            .and_then(|id| self.lock().analyses.get(id).map(|a| a.source_ref.clone()));
        if payload["plan_id"] != plan.id || !matches!(plan.schema, 2 | 4 | 6) {
            return Err(Error::new(503, "result-invalid"));
        }
        let expected_outputs = {
            let state = self.lock();
            let request = &state
                .sessions
                .get(&c.session)
                .ok_or_else(unavailable)?
                .requests
                .get(&c.request)
                .ok_or_else(unavailable)?
                .request
                .operation;
            let Operation::Plan { outputs, .. } = request else {
                return Err(unavailable());
            };
            outputs.clone()
        };
        let actual_outputs: Vec<drmr_core::conversion::plan::bundle::BundleOutputIntent> =
            serde_json::from_value(plan.outputs.clone())
                .map_err(|_| Error::new(503, "result-invalid"))?;
        if actual_outputs != expected_outputs {
            return Err(Error::new(503, "result-invalid"));
        }
        // Common jobs set no global mode, so an omitted output mode requests Auto.
        if plan.schema == 6 {
            let choices: Vec<OutputProcessing> = serde_json::from_value(
                plan.processing
                    .clone()
                    .ok_or_else(|| Error::new(503, "result-invalid"))?,
            )
            .map_err(|_| Error::new(503, "result-invalid"))?;
            if choices.len() != expected_outputs.len()
                || choices
                    .iter()
                    .zip(&expected_outputs)
                    .any(|(choice, output)| {
                        choice.id != output.id
                            || choice.selection.requested != output.processing.unwrap_or_default()
                    })
            {
                return Err(Error::new(503, "result-invalid"));
            }
        }
        let analysis = c
            .expected_inventory
            .as_ref()
            .ok_or_else(unavailable)?
            .read_result()?;
        let inventory: SourceInventory = serde_json::from_value(analysis.payload["source"].clone())
            .map_err(|_| unavailable())?;
        if plans::source(&plan)? != inventory {
            return Err(Error::new(409, "analysis-source-changed"));
        }
        Ok(plan)
    }
    fn validate_analysis_source(
        &self,
        c: &Completion,
        payload: &Value,
    ) -> Result<(BoundPath, String)> {
        let inventory: SourceInventory =
            serde_json::from_value(payload["source"].clone()).map_err(|_| unavailable())?;
        let path = inventory.root.join(&inventory.entrypoint);
        match &c.prepared.operation {
            ChildOperation::Analyze {
                source: ChildSource::Local { input_file },
            } if &path == input_file => {}
            ChildOperation::Analyze {
                source: ChildSource::Url { download_dir, .. },
            } if path.starts_with(download_dir) => {}
            _ => return Err(Error::new(503, "result-invalid")),
        }
        if let Some(input) = &c.input {
            input.check()?;
        }
        let mut s = self.lock();
        plans::bindings(&inventory, &s.files)?;
        let source = s.files.allowed_media(&path)?;
        // Reference exhaustion is a refusal, never a successful analysis without its source binding.
        let reference = s.files.issue_media(&path)?;
        Ok((source, reference))
    }
    fn publish(&self, c: &Completion, v: Validated) -> Result<Published> {
        let reference = pyutil::uuid4_hex();
        let path = c.prepared.directory.path.join("public-report.json");
        let bytes = serde_json::to_vec(&self.envelope(json!({
            "report_ref":reference,"session_id":c.session,"job_id":c.id,"request_id":c.request,
            "operation":c.operation,"analysis_ref":c.origin_analysis,
            "plan_ref":v.plan.as_ref().or(c.expected_plan.as_ref()).map(|p|p.reference.clone()),
            "plan_id":v.plan.as_ref().map(|p|p.id.clone()).or(c.plan_id.clone()),
            "report_schema_version":v.payload_version,"report_kind":v.projected.kind,"report":v.projected.value
        }))).map_err(|_| unavailable())?;
        if bytes.len() > protocol::RESULT_LIMIT {
            return Err(Error::new(503, "result-invalid"));
        }
        plans::write_new(&path, &bytes)?;
        Ok(Published {
            success: v.projected.success,
            kind: v.projected.kind,
            reference,
            binding: BoundPath::capture(&path)?,
            digest: plans::digest(&bytes),
            plan: v.plan,
            source: v.source,
        })
    }
    fn finish(&self, c: &Completion, result: Result<Published>) {
        let mut s = self.lock();
        let mut analysis = None;
        let mut plan = None;
        let report = match result {
            Ok(mut published) => {
                let success = published.success
                    && c.observation.handle.exit_code == Some(0)
                    && c.observation.supervision_confirmed
                    && !c.observation.handle.stop_requested;
                if success {
                    if let Some((source, source_ref)) = published.source.take() {
                        analysis = Some(Analysis {
                            id: pyutil::uuid4_hex(),
                            session: c.session.clone(),
                            job: c.id.clone(),
                            source_ref,
                            source,
                            report: published.reference.clone(),
                            raw: c.prepared.clone(),
                        });
                    }
                    plan = published.plan.take();
                }
                Some(published)
            }
            Err(e) => {
                if let Some(j) = s.jobs.get_mut(&c.id) {
                    j.error = Some(e);
                }
                None
            }
        };
        let analysis_binding = analysis
            .as_ref()
            .map(|a| (a.id.clone(), a.source_ref.clone()));
        if let Some(a) = analysis {
            s.analyses.insert(a.id.clone(), a);
        }
        if let Some(p) = &plan {
            s.plans.insert(p.reference.clone(), p.clone());
            if let Some(session) = s.sessions.get_mut(&c.session) {
                session.plans.push(p.reference.clone());
            }
        }
        if let Some(j) = s.jobs.get_mut(&c.id) {
            Self::finish_job(j, c, report, analysis_binding, plan);
        }
        if let Some(session) = s.sessions.get_mut(&c.session)
            && session.active.as_deref() == Some(&c.request)
        {
            session.active = None;
        }
        drop(s);
        if self.manager.release_common(&c.id).is_err() {
            let mut s = self.lock();
            if let Some(j) = s.jobs.get_mut(&c.id) {
                j.phase = Phase::Observing;
                j.state = "running";
                j.cleanup = false;
                j.error = Some(Error::new(503, "cleanup-unconfirmed"));
            }
            if let Some(session) = s.sessions.get_mut(&c.session) {
                session.active = Some(c.request.clone());
            }
        }
    }
    fn finish_job(
        j: &mut Job,
        c: &Completion,
        report: Option<Published>,
        analysis: Option<(String, String)>,
        plan: Option<plans::Plan>,
    ) {
        j.cleanup = true;
        j.phase = Phase::Terminal;
        if let Some((id, source_ref)) = analysis {
            j.source_ref = Some(source_ref);
            j.analysis = Some(id);
        }
        if let Some(p) = plan {
            j.plan = Some(p.reference);
            j.plan_id = Some(p.id);
        }
        let success = report.as_ref().is_some_and(|r| r.success)
            && c.observation.handle.exit_code == Some(0)
            && c.observation.supervision_confirmed;
        j.state = if c.observation.handle.stop_requested {
            "interrupted"
        } else if success {
            "succeeded"
        } else {
            "failed"
        };
        if let Some(r) = report {
            j.report = Some(r.reference);
            j.public_report = Some((r.binding, r.digest));
            if r.kind == "failure" && j.error.is_none() {
                j.error = Some(Error::new(422, "operation-failed"));
            }
        }
        if !success && j.state != "interrupted" && j.error.is_none() {
            j.error = Some(
                if c.operation == OperationName::Verify
                    && c.observation.handle.exit_code == Some(1)
                    && j.report.is_some()
                {
                    Error::new(422, "verification-mismatch")
                } else {
                    Error::new(503, "result-invalid")
                },
            );
        }
        if j.state == "interrupted" {
            j.error = Some(Error::new(422, "cancelled"));
        }
        j.revision = j.revision.saturating_add(1);
    }
    pub(super) fn job(&self, id: &str) -> Result<Value> {
        self.refresh(id);
        self.lock()
            .jobs
            .get(id)
            .map(|j| self.job_value(j))
            .ok_or(Error::new(404, "job-not-found"))
    }
    pub(super) fn stop(&self, id: &str) -> Result<Value> {
        if !self.lock().jobs.contains_key(id) {
            return Err(Error::new(404, "job-not-found"));
        }
        self.manager
            .request_stop(id)
            .map_err(|e| manager_error(&e))?;
        self.job(id)
    }
    pub(super) fn analysis(&self, id: &str) -> Result<Value> {
        let s = self.lock();
        let a = s.analyses.get(id).ok_or_else(unavailable)?;
        Ok(self.envelope(json!({"analysis_ref":a.id,"session_id":a.session,"job_id":a.job,"source_ref":a.source_ref,"report_ref":a.report})))
    }
    pub(super) fn plan(&self, id: &str) -> Result<Value> {
        let s = self.lock();
        let p = s.plans.get(id).ok_or_else(unavailable)?;
        p.read()?;
        Ok(self.envelope(p.view()))
    }
    pub(super) fn export(&self, id: &str) -> Result<Reply> {
        let p = self.lock().plans.get(id).cloned().ok_or_else(unavailable)?;
        let bytes = p.read()?;
        let mut reply = Reply::bytes(200, "application/json", bytes);
        reply.set_header("Content-Disposition", "attachment; filename=drmr-plan.json");
        reply.set_header("Cache-Control", "no-store");
        Ok(reply)
    }
    pub(super) fn report(&self, id: &str) -> Result<Value> {
        let s = self.lock();
        let j = s
            .jobs
            .values()
            .find(|j| j.report.as_deref() == Some(id))
            .ok_or_else(unavailable)?;
        let (binding, digest) = j.public_report.as_ref().ok_or_else(unavailable)?;
        let bytes = files::read_private(binding, protocol::RESULT_LIMIT)?;
        if plans::digest(&bytes) != *digest {
            return Err(unavailable());
        }
        let mut v: Value =
            protocol::decode(&bytes, protocol::RESULT_LIMIT).map_err(|_| unavailable())?;
        v["analysis_ref"] = json!(j.analysis);
        v["plan_ref"] = json!(j.plan);
        v["plan_id"] = json!(j.plan_id);
        Ok(v)
    }
    pub fn shutdown(&self) -> std::io::Result<()> {
        self.refresh_active();
        self.workers.shutdown()?;
        let s = self.lock();
        if s.jobs.values().any(|j| j.phase != Phase::Terminal)
            || !s.preparations.is_empty()
            || s.sessions.values().any(|session| session.active.is_some())
        {
            return Err(std::io::Error::other("common-cleanup-unconfirmed"));
        }
        Ok(())
    }
}
fn manager_error(e: &JobError) -> Error {
    match e {
        JobError::AlreadyRunning => Error::new(409, "global-job-busy"),
        JobError::ShuttingDown => Error::new(503, "server-shutting-down"),
        JobError::CleanupUnconfirmed => Error::new(503, "cleanup-unconfirmed"),
        JobError::SupervisionFailed(_) => Error::new(503, "job-supervision-failed"),
        JobError::NotFound(_) => Error::new(404, "job-not-found"),
        JobError::Launch(_) => Error::new(503, "common-service-unavailable"),
    }
}
fn safe_leaf(s: &str) -> bool {
    let upper = s.split('.').next().unwrap_or_default().to_ascii_uppercase();
    !s.is_empty()
        && s.len() <= 128
        && !matches!(s, "." | "..")
        && !s.ends_with([' ', '.'])
        && !s
            .chars()
            .any(|c| c.is_control() || "/\\:<>\"|?*".contains(c))
        && !matches!(
            upper.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "CLOCK$"
                | "CONIN$"
                | "CONOUT$"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}
