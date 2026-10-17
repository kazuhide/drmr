//! Strict operator configuration; secret strings and input bytes are wiped on drop.

use super::{ContentKey, ConversionError, InputKeys, OutputKey};
use serde::Deserialize;
use std::io::Read;
use std::path::Path;
use zeroize::{Zeroize, Zeroizing};

const LIMIT: u64 = 65_536;
const INVALID: ConversionError = ConversionError("key-file-invalid");

/// Separate decryption and encryption domains; no environment fallback is applied.
#[derive(Debug)]
pub struct OperatorKeys {
    pub input: InputKeys,
    pub output: Option<OutputKey>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    schema_version: u32,
    #[serde(default)]
    input: RawInput,
    output: Option<RawKey>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInput {
    #[serde(default)]
    keys: Vec<RawKey>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKey {
    kid: String,
    key: String,
}

impl Drop for RawKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

/// Read at most 64 KiB from a protected, regular, singly linked operator file.
pub fn read(path: &Path) -> Result<OperatorKeys, ConversionError> {
    let file = private_file(path)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() as u64 > LIMIT {
        return Err(INVALID);
    }
    let raw: RawFile = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    if raw.schema_version != 1 {
        return Err(INVALID);
    }
    let input = raw
        .input
        .keys
        .iter()
        .map(|key| ContentKey::from_hex(&key.kid, &key.key))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| INVALID)?;
    let input = InputKeys::new(input).map_err(|_| INVALID)?;
    let output = raw
        .output
        .as_ref()
        .map(|key| OutputKey::from_hex(&key.kid, &key.key))
        .transpose()
        .map_err(|_| INVALID)?;
    Ok(OperatorKeys { input, output })
}

#[cfg(unix)]
fn private_file(path: &Path) -> Result<std::fs::File, ConversionError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let metadata = std::fs::symlink_metadata(path).map_err(|_| INVALID)?;
    if !metadata.is_file() {
        return Err(INVALID);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(path)
        .map_err(|_| INVALID)?;
    let metadata = file.metadata().map_err(|_| INVALID)?;
    if !metadata.is_file()
        || metadata.len() > LIMIT
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(INVALID);
    }
    Ok(file)
}

#[cfg(not(unix))]
fn private_file(_path: &Path) -> Result<std::fs::File, ConversionError> {
    Err(INVALID)
}
