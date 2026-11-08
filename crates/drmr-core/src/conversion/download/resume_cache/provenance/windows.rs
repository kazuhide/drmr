use super::super::{ConversionError, UNSAFE};
use std::fs::File;

pub(super) fn create(file: &File, nonce: &[u8; 32]) -> Result<(), ConversionError> {
    let directory = file.metadata().map_err(|_| UNSAFE)?.is_dir();
    private_fs::create_object_nonce(file, directory, nonce).map_err(|_| UNSAFE)
}

pub(super) fn read(file: &File) -> Result<[u8; 32], ConversionError> {
    let directory = file.metadata().map_err(|_| UNSAFE)?.is_dir();
    private_fs::read_object_nonce(file, directory).map_err(|_| UNSAFE)
}
