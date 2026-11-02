//! Atomic private report files share the same held-parent publication rules as saved plans.
use super::{destination, files};
use crate::conversion::ConversionError;
use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

const EXISTS: ConversionError = ConversionError("report-destination-exists");
const UNSAFE: ConversionError = ConversionError("report-destination-unsafe");
const WRITE: ConversionError = ConversionError("report-destination-write-failed");
const LIMIT: usize = 32 * 1024 * 1024;

/// Resolve a new report path and check its parent without creating or replacing an entry.
pub fn report_destination(path: &Path) -> Result<PathBuf, ConversionError> {
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(EXISTS);
    }
    let path = super::super::destination(path).map_err(|_| UNSAFE)?;
    destination::OutputGuard::new(&path).map_err(|_| UNSAFE)?;
    Ok(path)
}

/// Write an already-redacted JSON result once, with private permissions and atomic publication.
pub fn write_report_new(path: &Path, value: &serde_json::Value) -> Result<(), ConversionError> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .0
                .len()
                .checked_add(bytes.len())
                .is_none_or(|len| len >= LIMIT)
            {
                return Err(std::io::Error::other("report size limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let path = report_destination(path)?;
    let destination = destination::OutputGuard::new(&path).map_err(|_| UNSAFE)?;
    let mut bytes = Bounded(Vec::new());
    serde_json::to_writer_pretty(&mut bytes, value)
        .map_err(|_| ConversionError("report-destination-limit"))?;
    let stage = files::Stage::new(path.parent()).map_err(|_| WRITE)?;
    let staged = stage.path().join("report.json");
    let mut file = stage.create_file(&staged).map_err(|_| WRITE)?;
    file.write_all(&bytes.0)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|_| WRITE)?;
    let source = stage
        .created_file_source(&staged, &file)
        .map_err(|_| WRITE)?;
    drop(file);
    destination.commit(&source).map_err(|_| {
        if std::fs::symlink_metadata(&path).is_ok() {
            EXISTS
        } else {
            UNSAFE
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_is_private_new_and_not_replaced() {
        let root = crate::native::private_tempdir().unwrap();
        let path = root.path().join("result.json");
        let value = serde_json::json!({"status":"passed"});
        assert_eq!(
            report_destination(&path).unwrap(),
            root.path().canonicalize().unwrap().join("result.json")
        );
        write_report_new(&path, &value).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap(),
            value
        );
        assert_eq!(write_report_new(&path, &serde_json::json!({})), Err(EXISTS));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn symlink_and_symlink_parent_are_refused_without_touching_target() {
        let root = crate::native::private_tempdir().unwrap();
        let target = root.path().join("original.json");
        std::fs::write(&target, b"original").unwrap();
        let link = root.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(write_report_new(&link, &serde_json::json!({})), Err(EXISTS));
        let parent = root.path().join("linked-parent");
        std::os::unix::fs::symlink(root.path(), &parent).unwrap();
        assert_eq!(
            write_report_new(&parent.join("new.json"), &serde_json::json!({})),
            Err(UNSAFE)
        );
        assert_eq!(std::fs::read(target).unwrap(), b"original");
    }
}
