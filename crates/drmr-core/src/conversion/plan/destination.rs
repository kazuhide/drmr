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
        let identity = FileIdentity::read(&held).map_err(|_| UNSAFE)?;
        let guard = Self {
            output: output.to_path_buf(),
            parent: held,
            identity,
            #[cfg(windows)]
            _ancestors: ancestors,
        };
        guard.check()?;
        Ok(guard)
    }
    pub(in crate::conversion) fn check(&self) -> Result<(), ConversionError> {
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
    pub(in crate::conversion) fn commit(&self, source: &Path) -> Result<(), ConversionError> {
        self.check()?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            rustix::fs::renameat_with(
                rustix::fs::CWD,
                source,
                &self.parent,
                self.output.file_name().ok_or(UNSAFE)?,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|_| ConversionError("output-exists-or-publish-failed"))
        }
        #[cfg(windows)]
        {
            files::atomic_move(source, &self.output)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = source;
            Err(UNSAFE)
        }
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
        }
    }
    pub(in crate::conversion) fn check_plan_destination(&self) -> Result<(), ConversionError> {
        self.planned_destination
            .as_ref()
            .map_or(Ok(()), |guard| guard.check())
    }
    pub(in crate::conversion) fn commit_publication(
        &self,
        source: &Path,
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
        files::atomic_move(source, destination)
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
