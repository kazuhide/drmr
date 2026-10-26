//! Bind final publication to the output parent selected before source admission.
use super::super::{ConversionError, Converter, files};
use crate::native::FileIdentity;
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
};

const UNSAFE: ConversionError = ConversionError("execution-plan-destination-unsafe");

pub(in crate::conversion) struct OutputGuard {
    output: PathBuf,
    parent: File,
    identity: FileIdentity,
    staging_parent: Option<PathBuf>,
    #[cfg(windows)]
    _ancestors: Vec<File>,
}
impl OutputGuard {
    pub(in crate::conversion) fn new(output: &Path) -> Result<Self, ConversionError> {
        if super::super::destination(output).map_err(|_| UNSAFE)? != output {
            return Err(UNSAFE);
        }
        let parent = output.parent().ok_or(UNSAFE)?;
        #[cfg(windows)]
        let ancestors = hold_ancestors(parent)?;
        let held = open_directory(parent)?;
        #[cfg(unix)]
        files::check_workspace_parent(&held).map_err(|_| UNSAFE)?;
        let identity = FileIdentity::read(&held).map_err(|_| UNSAFE)?;
        let guard = Self {
            output: output.to_path_buf(),
            parent: held,
            identity,
            staging_parent: None,
            #[cfg(windows)]
            _ancestors: ancestors,
        };
        guard.check()?;
        Ok(guard)
    }
    pub(super) fn for_bundle_child(output: &Path, bundle: &Self) -> Result<Self, ConversionError> {
        bundle.check()?;
        let mut guard = Self::new(output)?;
        // Keep tool workspaces beside the bundle rather than nesting them in it.
        guard.staging_parent = Some(bundle.output.parent().ok_or(UNSAFE)?.to_path_buf());
        Ok(guard)
    }
    pub(in crate::conversion) fn check(&self) -> Result<(), ConversionError> {
        #[cfg(unix)]
        files::check_workspace_parent(&self.parent).map_err(|_| UNSAFE)?;
        if super::super::destination(&self.output).map_err(|_| UNSAFE)? != self.output
            || FileIdentity::read(&self.parent).map_err(|_| UNSAFE)? != self.identity
            || FileIdentity::read(&open_directory(self.output.parent().ok_or(UNSAFE)?)?)
                .map_err(|_| UNSAFE)?
                != self.identity
        {
            return Err(UNSAFE);
        }
        Ok(())
    }
    pub(in crate::conversion) fn commit(
        &self,
        source: &files::PublicationSource<'_>,
    ) -> Result<(), ConversionError> {
        self.commit_observed(source, || {})
    }
    fn commit_observed(
        &self,
        source: &files::PublicationSource<'_>,
        after_check: impl FnOnce(),
    ) -> Result<(), ConversionError> {
        self.check()?;
        after_check();
        source.commit(&self.parent, &self.output)
    }
}
impl Converter {
    pub(in crate::conversion) fn using_plan_destination(&self, guard: OutputGuard) -> Self {
        Self {
            backend: self.backend.clone(),
            maximum_workers: self.maximum_workers,
            downloaded: self.downloaded.clone(),
            frozen_source: self.frozen_source.clone(),
            planned_destination: Some(Arc::new(guard)),
            output_signaling: self.output_signaling.clone(),
            processing_mode: self.processing_mode,
            segment_duration_ms: self.segment_duration_ms,
            output_options: self.output_options.clone(),
            source_hls_target_ms: self.source_hls_target_ms,
            hls_target_policy: self.hls_target_policy,
        }
    }
    pub(in crate::conversion) fn check_plan_destination(&self) -> Result<(), ConversionError> {
        self.planned_destination
            .as_ref()
            .map_or(Ok(()), |guard| guard.check())
    }
    pub(in crate::conversion) fn output_stage(
        &self,
        destination: &Path,
    ) -> Result<files::Stage, ConversionError> {
        self.check_plan_destination()?;
        let destination = super::super::destination(destination)?;
        let parent = self
            .planned_destination
            .as_ref()
            .filter(|guard| guard.output == destination)
            .and_then(|guard| guard.staging_parent.as_deref())
            .or_else(|| destination.parent())
            .ok_or(ConversionError("output-path-invalid"))?;
        files::Stage::new(Some(parent))
    }
    pub(in crate::conversion) fn commit_publication(
        &self,
        source: &files::PublicationSource<'_>,
        destination: &Path,
    ) -> Result<(), ConversionError> {
        if let Some(guard) = &self.planned_destination {
            // Always check the saved parent, including before intermediate publication.
            // Otherwise a redirected final path could masquerade as a different destination.
            guard.check()?;
            if destination == guard.output {
                return guard.commit(source);
            }
        }
        source.commit_to_path(destination)
    }
}
fn open_directory(path: &Path) -> Result<File, ConversionError> {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.read(true).custom_flags(
            (rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options
            .access_mode(0x81)
            .share_mode(3)
            .custom_flags(0x0220_0000)
            .security_qos_flags(0x0001_0000);
    }
    let file = options.open(path).map_err(|_| UNSAFE)?;
    let metadata = file.metadata().map_err(|_| UNSAFE)?;
    if !metadata.is_dir() {
        return Err(UNSAFE);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(UNSAFE);
        }
    }
    Ok(file)
}
#[cfg(windows)]
fn hold_ancestors(path: &Path) -> Result<Vec<File>, ConversionError> {
    use std::path::Component;
    let mut prefix = PathBuf::new();
    let mut held = Vec::new();
    for part in path.components() {
        prefix.push(part);
        if !matches!(part, Component::Prefix(_)) {
            held.push(open_directory(&prefix)?);
        }
        if held.len() > 256 {
            return Err(UNSAFE);
        }
    }
    Ok(held)
}

#[cfg(test)]
mod tests;
