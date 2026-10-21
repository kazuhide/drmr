//! The common console reuses the CLI and the manager's one supervised execution lease.
mod child;
mod files;
mod jobs;
mod plans;
mod projection;
mod wire;
use crate::common_protocol::{self as protocol, correlation_id};
use crate::{
    ServerConfig,
    http::{Reply, Req},
    jobs::ConversionJobManager,
    pyutil,
};
use files::{BoundPath, Files};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};
use wire::{Error, Result, Source, invalid, unavailable};

struct Session {
    id: String,
    creation: String,
    source: Source,
    source_binding: Option<BoundPath>,
    plans: Vec<String>,
    requests: BTreeMap<String, jobs::Claim>,
    active: Option<String>,
}
struct State {
    files: Files,
    sessions: BTreeMap<String, Session>,
    closed: VecDeque<(String, String)>,
    jobs: BTreeMap<String, jobs::Job>,
    preparations: BTreeMap<String, jobs::RetainedPreparation>,
    analyses: BTreeMap<String, jobs::Analysis>,
    plans: BTreeMap<String, plans::Plan>,
    closing: bool,
}
pub struct CommonService {
    id: String,
    config: Arc<ServerConfig>,
    manager: Arc<ConversionJobManager>,
    state: Mutex<State>,
    workers: Arc<crate::workers::Workers>,
    retained: std::path::PathBuf,
    private: std::path::PathBuf,
}
impl std::fmt::Debug for CommonService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommonService").finish_non_exhaustive()
    }
}
impl CommonService {
    pub fn new(
        config: Arc<ServerConfig>,
        manager: Arc<ConversionJobManager>,
    ) -> std::io::Result<Arc<Self>> {
        let files = Files::new(&config)?;
        let id = pyutil::uuid4_hex();
        let retained =
            drmr_core::native::resolve_path(&config.output_root.join("_common").join(&id))?;
        let private =
            drmr_core::native::resolve_path(&manager.jobs_dir().join("common").join(&id))?;
        for path in [
            &retained,
            &private,
            &retained.join("plans"),
            &retained.join("downloads"),
        ] {
            drmr_core::native::create_private_dir_all(path)?;
        }
        Ok(Arc::new(Self {
            id,
            config,
            manager,
            state: Mutex::new(State {
                files,
                sessions: BTreeMap::new(),
                closed: VecDeque::new(),
                jobs: BTreeMap::new(),
                preparations: BTreeMap::new(),
                analyses: BTreeMap::new(),
                plans: BTreeMap::new(),
                closing: false,
            }),
            workers: Arc::new(crate::workers::Workers::default()),
            retained,
            private,
        }))
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub fn denied(&self, path: &std::path::Path) -> bool {
        self.lock().files.http_denied(path)
    }
    pub fn begin_shutdown(&self) {
        self.lock().closing = true;
        self.workers.close();
    }
    fn envelope(&self, mut value: Value) -> Value {
        if let Some(object) = value.as_object_mut() {
            object.insert("schema_version".into(), json!(1));
            object.insert("server_id".into(), json!(self.id));
        }
        value
    }
    pub fn error_reply(&self, error: Error) -> Reply {
        Reply::json(
            error.status,
            &self.envelope(json!({"error":error.value(None,None)})),
        )
    }
    pub fn guard_reply(&self, status: u16) -> Reply {
        self.error_reply(Error::new(
            status,
            match status {
                413 => "request-too-large",
                415 => "json-required",
                403 => "same-origin-required",
                503 => "server-shutting-down",
                _ => "invalid-request",
            },
        ))
    }
    fn capability(&self) -> Value {
        let s = self.lock();
        let execution = self.manager.execution();
        let busy = execution.busy;
        let common_cleanup = execution
            .job_id
            .as_ref()
            .and_then(|id| s.jobs.get(id))
            .is_some_and(|j| j.error.is_some_and(|e| e.code == "cleanup-unconfirmed"));
        self.envelope(json!({"roots":s.files.roots(),"limits":{"sessions":8,"requests_per_session":32,"page_size_max":100,"http_body_bytes":262_144,"keys_file_bytes":65_536,"fetch_policy_bytes":65_536,"output_spec_bytes":65_536,"saved_plan_bytes":33_554_432,"result_bytes":67_108_864,"progress_bytes":8192,"max_outputs":8},"worker_policy":{"maximum":self.config.max_workers.unwrap_or(64),"automatic":true},"execution":{"availability":if s.closing || execution.closing {"closing"} else if execution.cleanup_unconfirmed || common_cleanup {"cleanup-unconfirmed"} else if busy {"busy"} else {"idle"},"job_id":execution.job_id,"owner":execution.owner},"observation":"poll"}))
    }
    fn session(&self, s: &Session) -> Value {
        let source = match &s.source {
            Source::Local { .. } => json!({"kind":"local","label":"Local input"}),
            Source::SavedPlan { .. } => json!({"kind":"saved-plan","label":"Saved plan"}),
            Source::HlsUrl { url, .. } => json!({"kind":"hls-url","display_url":display_url(url)}),
        };
        self.envelope(json!({"session_id":s.id,"creation_request_id":s.creation,"source":source,"plan_refs":s.plans,"request_ids":s.requests.keys().collect::<Vec<_>>(),"requests_used":s.requests.len(),"active_request_id":s.active}))
    }
    fn create(&self, bytes: &[u8]) -> Result<(u16, Value)> {
        let request: wire::CreateSession =
            protocol::decode(bytes, protocol::REQUEST_LIMIT).map_err(|_| invalid())?;
        if request.schema_version != 1 || !correlation_id(&request.creation_request_id) {
            return Err(invalid());
        }
        if request.server_id != self.id {
            return Err(Error::new(409, "server-changed"));
        }
        let mut s = self.lock();
        if s.closing {
            return Err(Error::new(503, "server-shutting-down"));
        }
        if let Some(old) = s
            .sessions
            .values()
            .find(|old| old.creation == request.creation_request_id)
        {
            return if old.source == request.source {
                Ok((200, self.session(old)))
            } else {
                Err(Error::new(409, "request-conflict"))
            };
        }
        if s.sessions.len() >= 8 {
            return Err(Error::new(409, "session-limit-reached"));
        }
        let binding = match &request.source {
            Source::Local { input_ref } => Some(s.files.file(input_ref, wire::Role::Media)?),
            Source::SavedPlan { plan_file_ref } => {
                Some(s.files.file(plan_file_ref, wire::Role::SavedPlan)?)
            }
            Source::HlsUrl {
                url,
                fetch_policy_ref,
            } => {
                let parsed = pyutil::urlparse(url).map_err(|_| invalid())?;
                if url.len() > 8192
                    || url.chars().any(char::is_control)
                    || !matches!(parsed.scheme.as_str(), "http" | "https")
                    || parsed.netloc.is_empty()
                {
                    return Err(invalid());
                }
                if let Some(id) = fetch_policy_ref {
                    s.files.file(id, wire::Role::FetchPolicy)?;
                }
                None
            }
        };
        let imported = if let Source::SavedPlan { .. } = &request.source {
            let original = binding.as_ref().ok_or_else(unavailable)?;
            let validated = plans::load(&original.path, &s.files, "", pyutil::uuid4_hex())?;
            let bytes = validated.read()?;
            let path = self
                .retained
                .join("plans")
                .join(format!("{}.json", pyutil::uuid4_hex()));
            plans::write_new(&path, &bytes)?;
            Some(path)
        } else {
            None
        };
        let session = Session {
            id: pyutil::uuid4_hex(),
            creation: request.creation_request_id,
            source: request.source,
            source_binding: binding,
            plans: Vec::new(),
            requests: BTreeMap::new(),
            active: None,
        };
        let mut session = session;
        if let Some(path) = imported {
            let mut plan = plans::load(&path, &s.files, &session.id, pyutil::uuid4_hex())?;
            plan.source_ref = Some(s.files.issue_media(&plan.source_entrypoint)?);
            session.plans.push(plan.reference.clone());
            s.plans.insert(plan.reference.clone(), plan);
        }
        let value = self.session(&session);
        s.sessions.insert(session.id.clone(), session);
        Ok((201, value))
    }
    fn dispatch_inner(self: &Arc<Self>, req: &Req) -> Result<Reply> {
        let tail = req
            .path
            .strip_prefix("/api/common/")
            .ok_or_else(unavailable)?;
        let parts: Vec<_> = tail.split('/').collect();
        if parts != ["files"] && !req.args.is_empty() {
            return Err(invalid());
        }
        self.refresh_active();
        let (status,value)=match (req.method.as_str(),parts.as_slice()) {
            ("GET"|"HEAD",["capabilities"]) => (200,self.capability()),
            ("GET"|"HEAD",["files"]) => (200,self.envelope(self.lock().files.page(req)?)),
            ("GET"|"HEAD",["sessions"]) => (200,self.envelope(json!({"sessions":self.lock().sessions.values().map(|s|self.session(s)).collect::<Vec<_>>()}))),
            ("POST",["sessions"]) => self.create(&req.body)?,
            ("POST",["jobs"]) => (202,self.submit(&req.body)?),
            ("GET"|"HEAD",["jobs",id]) => (200,self.job(id)?),
            ("POST",["jobs",id,"stop"]) => {let body:serde_json::Map<String,Value>=protocol::decode(&req.body,protocol::REQUEST_LIMIT).map_err(|_| invalid())?;if !body.is_empty(){return Err(invalid());}let value=self.stop(id)?;(if value["state"]=="running" {202}else{200},value)},
            ("GET"|"HEAD",["analyses",id]) => (200,self.analysis(id)?),
            ("GET"|"HEAD",["plans",id]) => (200,self.plan(id)?),
            ("GET"|"HEAD",["plans",id,"export"]) => return self.export(id),
            ("GET"|"HEAD",["reports",id]) => (200,self.report(id)?),
            ("GET"|"HEAD",["session-requests",id]) => {let s=self.lock();if let Some(session)=s.sessions.values().find(|x|x.creation==*id){(200,self.envelope(json!({"creation_request_id":id,"state":"created","session_id":session.id})))}else if let Some((_,sid))=s.closed.iter().find(|(creation,_)|creation==id){(200,self.envelope(json!({"creation_request_id":id,"state":"closed","session_id":sid})))}else{return Err(Error::new(404,"session-request-not-found"));}},
            ("GET"|"HEAD",["sessions",id]) => (200,self.session(self.lock().sessions.get(*id).ok_or(Error::new(404,"session-not-found"))?)),
            ("DELETE",["sessions",id]) => {if !req.body.is_empty(){return Err(invalid());}let mut s=self.lock();let session=s.sessions.get(*id).ok_or(Error::new(404,"session-not-found"))?;if session.active.is_some(){return Err(Error::new(409,"session-busy"));}for job in s.jobs.values().filter(|j|j.session==*id){self.manager.forget_common(&job.id).map_err(|_|Error::new(503,"cleanup-unconfirmed"))?;}let session=s.sessions.remove(*id).ok_or_else(unavailable)?;s.jobs.retain(|_,j|j.session!=session.id);s.analyses.retain(|_,a|a.session!=session.id);s.plans.retain(|_,p|p.session!=session.id);s.closed.push_back((session.creation,session.id));if s.closed.len()>256{s.closed.pop_front();}return Ok(Reply::no_content());},
            ("GET"|"HEAD",["sessions",sid,"requests",id])=>{let s=self.lock();let session=s.sessions.get(*sid).ok_or(Error::new(404,"session-not-found"))?;(200,self.receipt(session,session.requests.get(*id).ok_or(Error::new(404,"request-not-found"))?))},
            _=>return Err(unavailable()),
        };
        Ok(Reply::json(status, &value))
    }
    pub fn dispatch(self: &Arc<Self>, req: &Req) -> Reply {
        self.dispatch_inner(req)
            .unwrap_or_else(|e| self.error_reply(e))
    }
}
fn display_url(url: &str) -> String {
    pyutil::urlparse(url).map_or_else(
        |_| String::new(),
        |p| {
            format!(
                "{}://{}{}",
                p.scheme,
                p.netloc.rsplit('@').next().unwrap_or_default(),
                p.path
            )
        },
    )
}
