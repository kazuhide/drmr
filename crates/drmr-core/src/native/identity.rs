//! Stable file-object identity without retaining a descriptor for every graph entry.
use std::{fs::File, io};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity(u64, u64);
impl FileIdentity {
    /// Identify the opened object without retaining one descriptor per resource.
    pub fn read(file: &File) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = file.metadata()?;
            Ok(Self(metadata.dev(), metadata.ino()))
        }
        #[cfg(windows)]
        {
            let information = winapi_util::file::information(file)?;
            Ok(Self(
                information.volume_serial_number(),
                information.file_index(),
            ))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = file;
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}
