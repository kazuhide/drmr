//! Each copy binds its reserved object, copied bytes, and reopened original path.
use super::{
    CHANGED, ConversionError, FileIdentity, LIMIT, Path, PathBuf, SourceFile, execution::Control,
    files, open_confined,
};
use std::{fs::File, io::Read as _};

pub(super) struct Object {
    pub path: PathBuf,
    pub bytes: u64,
    pub identity: FileIdentity,
}

impl Object {
    pub fn inspect(
        root: &Path,
        path: &Path,
        expected: Option<&SourceFile>,
        remaining: u64,
    ) -> Result<Self, ConversionError> {
        let input = open_confined(root, path)?;
        let identity = FileIdentity::read(&input).map_err(|_| CHANGED)?;
        let bytes = input.metadata().map_err(|_| CHANGED)?.len();
        if bytes > remaining {
            return Err(LIMIT);
        }
        if expected.is_some_and(|file| file.bytes != bytes) {
            return Err(CHANGED);
        }
        Ok(Self {
            path: path.to_path_buf(),
            bytes,
            identity,
        })
    }

    pub fn copy_to(
        &self,
        root: &Path,
        destination: &Path,
        expected: Option<&SourceFile>,
        control: Control<'_>,
        after_chunk: &mut impl FnMut(u64),
    ) -> Result<(SourceFile, FileIdentity), ConversionError> {
        control.check()?;
        let mut input = open_confined(root, &self.path)?;
        if FileIdentity::read(&input).map_err(|_| CHANGED)? != self.identity
            || input.metadata().map_err(|_| CHANGED)?.len() != self.bytes
        {
            return Err(CHANGED);
        }
        let mut output = files::create_new(destination)?;
        let (sha256, bytes) = transfer(&mut input, &mut output, control, self.bytes, after_chunk)?;
        output.sync_all().map_err(|_| CHANGED)?;
        if bytes != self.bytes || expected.is_some_and(|file| file.sha256 != sha256) {
            return Err(CHANGED);
        }
        if FileIdentity::read(&open_confined(root, &self.path)?).map_err(|_| CHANGED)?
            != self.identity
        {
            return Err(CHANGED);
        }
        Ok((
            SourceFile {
                path: self.path.clone(),
                bytes,
                sha256,
            },
            self.identity,
        ))
    }
}

#[cfg(test)]
pub(super) fn copy_object(
    root: &Path,
    directories: &mut super::directories::Directories,
    path: &Path,
    expected: Option<&SourceFile>,
    remaining: u64,
    stop: &super::Stop,
    after_chunk: &mut impl FnMut(&Path, u64),
) -> Result<(SourceFile, FileIdentity), ConversionError> {
    super::cancelled(stop)?;
    let object = Object::inspect(root, path, expected, remaining)?;
    let destination = directories.prepare(path)?;
    object.copy_to(
        root,
        &destination,
        expected,
        Control::new(stop),
        &mut |bytes| after_chunk(path, bytes),
    )
}

pub(super) fn transfer(
    input: &mut File,
    output: &mut impl std::io::Write,
    control: Control<'_>,
    maximum: u64,
    after_chunk: &mut impl FnMut(u64),
) -> Result<(String, u64), ConversionError> {
    let mut bytes = 0u64;
    let mut hash = crate::sha256::Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        control.check()?;
        let count = input.read(&mut buffer).map_err(|_| CHANGED)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .filter(|bytes| *bytes <= maximum && *bytes <= super::super::MAX_MEDIA_BYTES)
            .ok_or(CHANGED)?;
        output.write_all(&buffer[..count]).map_err(|_| CHANGED)?;
        hash.update(&buffer[..count]);
        after_chunk(bytes);
    }
    Ok((hex::encode(hash.finalize()), bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write as _};

    struct FailingWriter(bool);
    impl io::Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0 {
                return Err(io::Error::other("synthetic write failure"));
            }
            self.0 = true;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn partial_write_failure_does_not_return_a_fingerprint_or_modify_the_source() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let bytes = vec![b'A'; 3 * 64 * 1024];
        std::fs::write(&path, &bytes).unwrap();
        let stop = super::super::Stop::new();
        let mut completed = 0;
        let result = transfer(
            &mut File::open(&path).unwrap(),
            &mut FailingWriter(false),
            Control::new(&stop),
            bytes.len() as u64,
            &mut |count| completed = count,
        );
        assert_eq!(result.unwrap_err(), CHANGED);
        assert_eq!(completed, 64 * 1024);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(!stop.is_set());
    }

    #[test]
    fn parallel_exclusive_creation_failure_keeps_existing_bytes_and_releases_writers() {
        let root = tempfile::tempdir().unwrap();
        let canonical = root.path().canonicalize().unwrap();
        std::fs::write(canonical.join("source"), b"retained source").unwrap();
        let object = Object::inspect(&canonical, Path::new("source"), None, 100).unwrap();
        let stage = files::Stage::new(None).unwrap();
        let stage_path = stage.path().to_path_buf();
        let existing = stage_path.join("0");
        files::create_new(&existing)
            .unwrap()
            .write_all(b"exclusive owner")
            .unwrap();
        let execution = super::super::execution::Execution::for_test(Some(4), 4, 512 * 1024 * 1024);
        let stop = super::super::Stop::new();
        let result = execution.run(&stop, 4, |index, control| {
            object.copy_to(
                &canonical,
                &stage_path.join(index.to_string()),
                None,
                control,
                &mut |_| {},
            )
        });
        assert_eq!(
            result.err(),
            Some(ConversionError("temporary-file-unavailable"))
        );
        assert_eq!(std::fs::read(&existing).unwrap(), b"exclusive owner");
        assert_eq!(
            std::fs::read(canonical.join("source")).unwrap(),
            b"retained source"
        );
        assert!(!stop.is_set());
        drop(stage);
        assert!(!stage_path.exists());
        execution.run(&stop, 4, |index, _| Ok(index)).unwrap();
    }
}
