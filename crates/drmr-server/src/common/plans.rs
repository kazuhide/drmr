//! Saved plans retain their existing schema and exact bytes.
use super::{
    files::{BoundPath, Files, read_private},
    wire::{Error, Result, unavailable},
};
use drmr_core::{
    conversion::{
        SourceInventory,
        plan::{self, SavedPlan},
    },
    native,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub const LIMIT: usize = 32 * 1024 * 1024;
#[derive(Clone)]
pub struct Plan {
    pub reference: String,
    pub session: String,
    pub binding: BoundPath,
    pub digest: String,
    pub id: String,
    pub schema: u32,
    pub required_depth: drmr_core::conversion::ValidationDepth,
    pub outputs: Value,
    pub processing: Option<Value>,
    pub destination: PathBuf,
    pub destinations: Vec<PathBuf>,
    pub source_entrypoint: PathBuf,
    pub source_ref: Option<String>,
    pub analysis_ref: Option<String>,
}
impl Plan {
    pub fn read(&self) -> Result<Vec<u8>> {
        let b = read_private(&self.binding, LIMIT)?;
        if digest(&b) != self.digest {
            return Err(unavailable());
        }
        Ok(b)
    }
    pub fn view(&self) -> Value {
        let mut view = json!({"plan_ref":self.reference,"session_id":self.session,"plan_id":self.id,"plan_schema_version":self.schema,"required_depth":self.required_depth,"outputs":self.outputs,"destination_state":destination_state(&self.destination,&self.destinations),"export_url":format!("/api/common/plans/{}/export",self.reference)});
        if let Some(processing) = &self.processing {
            view["processing"] = processing.clone();
        }
        view
    }
}
pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn load(path: &Path, files: &Files, session: &str, reference: String) -> Result<Plan> {
    let binding = BoundPath::capture(path)?;
    let bytes = read_private(&binding, LIMIT)?;
    crate::common_protocol::decode::<Value>(&bytes, LIMIT)
        .map_err(|_| Error::new(422, "execution-plan-invalid"))?;
    let saved = plan::read_saved(path).map_err(|_| Error::new(422, "execution-plan-invalid"))?;
    let (source, destination, id, schema, outputs) = match &saved {
        SavedPlan::Single(p) => (
            &p.source,
            p.intent.output.clone(),
            p.plan_id.clone(),
            p.schema_version,
            json!([{"id":null,"format":p.intent.format,"target":p.intent.target}]),
        ),
        SavedPlan::Bundle(p) => (
            &p.source,
            p.output_root.clone(),
            p.plan_id.clone(),
            p.schema_version,
            json!(p.outputs),
        ),
    };
    let required_depth = match &saved {
        SavedPlan::Single(p) => p.minimum_depth(),
        SavedPlan::Bundle(p) => p.minimum_depth(),
    };
    let processing = match &saved {
        SavedPlan::Single(plan) => plan.processing.map(|selection| json!(selection)),
        SavedPlan::Bundle(plan) => plan.processing.as_ref().map(|choices| json!(choices)),
    };
    let destinations = match &saved {
        SavedPlan::Single(p) => vec![p.intent.output.clone()],
        SavedPlan::Bundle(p) => p
            .outputs
            .iter()
            .map(|o| {
                p.output_root.join(&o.id).join(match o.format {
                    drmr_core::inspection::OutputFormat::Mp4 => "media.mp4",
                    drmr_core::inspection::OutputFormat::Dash => "manifest.mpd",
                    _ => "master.m3u8",
                })
            })
            .collect(),
    };
    bindings(source, files)?;
    files.allowed_output(&destination)?;
    binding.check()?;
    Ok(Plan {
        reference,
        session: session.into(),
        binding,
        digest: digest(&bytes),
        id,
        schema,
        required_depth,
        outputs,
        processing,
        destination,
        destinations,
        source_entrypoint: source.root.join(&source.entrypoint),
        source_ref: None,
        analysis_ref: None,
    })
}
pub fn source(plan: &Plan) -> Result<SourceInventory> {
    plan.read()?;
    match plan::read_saved(&plan.binding.path).map_err(|_| unavailable())? {
        SavedPlan::Single(p) => Ok(p.source),
        SavedPlan::Bundle(p) => Ok(p.source),
    }
}
pub fn bindings(source: &SourceInventory, files: &Files) -> Result<()> {
    files.allowed_media(&source.root)?;
    files.allowed_media(&source.root.join(&source.entrypoint))?;
    for file in &source.files {
        files.allowed_media(&source.root.join(&file.path))?;
    }
    Ok(())
}
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = native::create_private_new(path).map_err(|_| unavailable())?;
    f.write_all(bytes)
        .and_then(|()| f.flush())
        .map_err(|_| unavailable())
}
fn destination_state(root: &Path, children: &[PathBuf]) -> &'static str {
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return "absent",
        Err(_) => return "unavailable",
        Ok(m) if m.file_type().is_symlink() => return "unavailable",
        Ok(_) => {}
    }
    let mut present = 0;
    for child in children {
        match std::fs::symlink_metadata(child) {
            Ok(m) if m.file_type().is_symlink() => return "unavailable",
            Ok(_) => present += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return "unavailable",
        }
    }
    if present == children.len() {
        "present"
    } else {
        "mixed"
    }
}
