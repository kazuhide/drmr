//! Handle-bound owner metadata; no path lookups and no variable-size allocation.
use super::super::{ConversionError, UNSAFE};
use std::fs::File;

const ATTRIBUTE: &str = "user.drmr.resume-provenance";

pub(super) fn create(file: &File, nonce: &[u8; 32]) -> Result<(), ConversionError> {
    rustix::fs::fsetxattr(file, ATTRIBUTE, nonce, rustix::fs::XattrFlags::CREATE)
        .map_err(|_| UNSAFE)
}

pub(super) fn read(file: &File) -> Result<[u8; 32], ConversionError> {
    let mut value = [0; 32];
    let length = rustix::fs::fgetxattr(file, ATTRIBUTE, &mut value[..]).map_err(|_| UNSAFE)?;
    if length != value.len() {
        return Err(UNSAFE);
    }
    Ok(value)
}

#[cfg(test)]
pub(super) fn remove(file: &File) -> Result<(), ConversionError> {
    rustix::fs::fremovexattr(file, ATTRIBUTE).map_err(|_| UNSAFE)
}
