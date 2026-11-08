//! Fixed-size provenance on newly created cache objects, independent of inode reuse.
//! The token is not secret; a trusted-account author can deliberately copy it.
use super::{ConversionError, FileIdentity, UNSAFE};
use std::fs::File;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(not(any(unix, windows)))]
mod platform {
    use super::{ConversionError, File, UNSAFE};
    pub(super) fn create(_: &File, _: &[u8; 32]) -> Result<(), ConversionError> {
        Err(UNSAFE)
    }
    pub(super) fn read(_: &File) -> Result<[u8; 32], ConversionError> {
        Err(UNSAFE)
    }
}
#[cfg(test)]
mod tests;

pub(super) const TOKEN_HEX_LENGTH: usize = 96;

/// Call only for a new cache-owned object created by the current attempt.
/// Existing or missing provenance is never repaired during admission.
pub(super) fn create(file: &File) -> Result<[u8; 32], ConversionError> {
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|_| ConversionError("secure-random-unavailable"))?;
    platform::create(file, &nonce)?;
    if read(file)? != nonce {
        return Err(UNSAFE);
    }
    Ok(nonce)
}

pub(super) fn read(file: &File) -> Result<[u8; 32], ConversionError> {
    platform::read(file)
}

pub(super) fn token(file: &File) -> Result<String, ConversionError> {
    let native = FileIdentity::read(file).map_err(|_| UNSAFE)?;
    Ok(combine(native, read(file)?))
}

pub(super) fn combine(native: FileIdentity, nonce: [u8; 32]) -> String {
    let mut token = native.stable_token();
    token.push_str(&hex::encode(nonce));
    token
}

#[cfg(all(test, unix))]
pub(super) fn remove_for_test(file: &File) -> Result<(), ConversionError> {
    unix::remove(file)
}
