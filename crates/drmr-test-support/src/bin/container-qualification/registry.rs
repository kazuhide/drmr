//! Bounded, immutable fixture records. This is never part of the runtime image.
use std::{fs, io::Read as _, path::Path};

pub(super) const MAX_INVOCATIONS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Invocation {
    pub tool: u32,
    pub descendant: Option<u32>,
    pub has_lock: bool,
    pub complete: bool,
}

#[derive(Debug)]
pub(super) struct Invalid {
    pub check: &'static str,
}

fn invalid(check: &'static str) -> Invalid {
    Invalid { check }
}

fn file(path: &Path) -> Result<Vec<u8>, Invalid> {
    let metadata = fs::symlink_metadata(path).map_err(|_| invalid("registry-record"))?;
    if !metadata.file_type().is_file() || metadata.len() > 32 {
        return Err(invalid("registry-record"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err(invalid("registry-record"));
        }
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(33).read_to_end(&mut bytes))
        .map_err(|_| invalid("registry-record"))?;
    if bytes.len() > 32 {
        return Err(invalid("registry-record"));
    }
    Ok(bytes)
}

fn pid(bytes: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(bytes).ok()?;
    let value: u32 = text.strip_suffix('\n')?.parse().ok()?;
    (value > 1 && i32::try_from(value).is_ok() && text == format!("{value}\n")).then_some(value)
}

fn pending_pid(bytes: &[u8]) -> bool {
    if bytes.ends_with(b"\n") {
        return pid(bytes).is_some();
    }
    bytes.len() <= 10 && bytes.first() != Some(&b'0') && bytes.iter().all(u8::is_ascii_digit)
}

pub(super) fn read(root: &Path, include_partial: bool) -> Result<Vec<Invocation>, Invalid> {
    if !fs::symlink_metadata(root)
        .map_err(|_| invalid("registry-entry"))?
        .file_type()
        .is_dir()
    {
        return Err(invalid("registry-entry"));
    }
    let entries = fs::read_dir(root).map_err(|_| invalid("registry-entry"))?;
    let mut records = Vec::new();
    for entry in entries.take(MAX_INVOCATIONS + 1) {
        if records.len() == MAX_INVOCATIONS {
            return Err(invalid("registry-bounds"));
        }
        let entry = entry.map_err(|_| invalid("registry-entry"))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| invalid("registry-entry"))?;
        let tool: u32 = name.parse().map_err(|_| invalid("registry-entry"))?;
        if tool <= 1
            || i32::try_from(tool).is_err()
            || name != tool.to_string()
            || !entry
                .file_type()
                .map_err(|_| invalid("registry-entry"))?
                .is_dir()
        {
            return Err(invalid("registry-entry"));
        }
        let path = entry.path();
        if !include_partial {
            match fs::symlink_metadata(path.join("complete")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    records.push(Invocation {
                        tool,
                        descendant: None,
                        has_lock: false,
                        complete: false,
                    });
                    continue;
                }
                Err(_) => return Err(invalid("registry-record")),
                Ok(_) => {}
            }
        }
        let mut descendant = None;
        let mut has_lock = false;
        let mut ready = false;
        let mut complete = false;
        let mut pending = false;
        let mut pid_pending = false;
        let mut ready_pending = false;
        for (index, member) in fs::read_dir(&path)
            .map_err(|_| invalid("registry-record"))?
            .take(8)
            .enumerate()
        {
            if index == 7 {
                return Err(invalid("registry-bounds"));
            }
            let member = member.map_err(|_| invalid("registry-record"))?;
            let name = member.file_name();
            let bytes = file(&member.path())?;
            match name.to_str() {
                Some("descendant.pid") => {
                    descendant = Some(pid(&bytes).ok_or_else(|| invalid("registry-record"))?);
                }
                Some("descendant.lock") if bytes.is_empty() => has_lock = true,
                Some("descendant.lock.ready") if bytes == b"ready\n" => ready = true,
                Some("complete") if bytes == b"ready\n" => complete = true,
                Some("descendant.pid.pending") if pending_pid(&bytes) => {
                    pending = true;
                    pid_pending = true;
                }
                Some("descendant.lock.ready.pending") if b"ready\n".starts_with(&bytes) => {
                    pending = true;
                    ready_pending = true;
                }
                Some("complete.pending") if b"ready\n".starts_with(&bytes) => {
                    pending = true;
                }
                _ => return Err(invalid("registry-record")),
            }
        }
        if descendant == Some(tool)
            || (pid_pending && descendant.is_some())
            || (ready_pending && ready)
            || (has_lock && descendant.is_none())
            || (ready && (!has_lock || descendant.is_none()))
            || (complete && (!ready || !has_lock || descendant.is_none() || pending))
        {
            return Err(invalid("registry-record"));
        }
        records.push(Invocation {
            tool,
            descendant,
            has_lock,
            complete,
        });
    }
    records.sort_unstable_by_key(|record| record.tool);
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, pid: u32, name: &str, bytes: &[u8]) {
        let entry = root.join(pid.to_string());
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join(name), bytes).unwrap();
    }

    #[test]
    fn late_and_partial_records_remain_in_the_terminal_inventory() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("21")).unwrap();
        write(root.path(), 22, "descendant.pid.pending", b"23");
        write(root.path(), 24, "descendant.pid", b"25\n");
        write(root.path(), 24, "descendant.lock", b"");
        write(root.path(), 24, "descendant.lock.ready", b"ready\n");
        write(root.path(), 24, "complete", b"ready\n");
        let records = read(root.path(), true).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].tool, 21);
        assert!(!records[0].complete);
        assert_eq!(records[1].tool, 22);
        assert!(records[1].descendant.is_none());
        assert_eq!(records[2].descendant, Some(25));
        assert!(records[2].has_lock && records[2].complete);
    }

    #[test]
    fn malformed_or_contradictory_records_are_never_omitted() {
        for (name, bytes) in [
            ("unknown", &b""[..]),
            ("descendant.pid", &b"1\n"[..]),
            ("descendant.pid", &b"023\n"[..]),
            ("descendant.pid", &b"22\n"[..]),
            ("descendant.pid", &b"2147483648\n"[..]),
            ("descendant.pid", &b"23"[..]),
            ("descendant.lock", &b"not-empty"[..]),
            ("descendant.lock.ready", &b"ready\n"[..]),
            ("complete", &b"ready\n"[..]),
            ("descendant.pid.pending", &b"23\n24"[..]),
            ("complete.pending", &b"arbitrary"[..]),
        ] {
            let root = tempfile::tempdir().unwrap();
            write(root.path(), 22, name, bytes);
            assert_eq!(
                read(root.path(), true).unwrap_err().check,
                "registry-record"
            );
        }
        for (published, pending) in [
            ("descendant.pid", "descendant.pid.pending"),
            ("descendant.lock.ready", "descendant.lock.ready.pending"),
            ("complete", "complete.pending"),
        ] {
            let root = tempfile::tempdir().unwrap();
            write(root.path(), 22, "descendant.pid", b"23\n");
            write(root.path(), 22, "descendant.lock", b"");
            write(root.path(), 22, "descendant.lock.ready", b"ready\n");
            write(root.path(), 22, "complete", b"ready\n");
            let bytes: &[u8] = if published == "descendant.pid" {
                b"23\n"
            } else {
                b"ready\n"
            };
            write(root.path(), 22, pending, bytes);
            assert_eq!(
                read(root.path(), true).unwrap_err().check,
                "registry-record"
            );
        }
    }

    #[test]
    fn noncanonical_names_and_overfull_registries_fail_closed() {
        for name in ["01", "1", "-2", "not-a-pid", "2147483648", "4294967296"] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join(name)).unwrap();
            assert_eq!(read(root.path(), true).unwrap_err().check, "registry-entry");
        }
        let root = tempfile::tempdir().unwrap();
        for pid in 2..=(u32::try_from(MAX_INVOCATIONS).unwrap() + 2) {
            fs::create_dir(root.path().join(pid.to_string())).unwrap();
        }
        assert_eq!(
            read(root.path(), true).unwrap_err().check,
            "registry-bounds"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_shared_record_inodes_are_refused() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("22")).unwrap();
        assert_eq!(read(root.path(), true).unwrap_err().check, "registry-entry");
        fs::remove_file(root.path().join("22")).unwrap();
        fs::create_dir(root.path().join("22")).unwrap();
        let external = outside.path().join("pid");
        fs::write(&external, b"23\n").unwrap();
        symlink(&external, root.path().join("22/descendant.pid")).unwrap();
        assert_eq!(
            read(root.path(), true).unwrap_err().check,
            "registry-record"
        );
        fs::remove_file(root.path().join("22/descendant.pid")).unwrap();
        fs::hard_link(external, root.path().join("22/descendant.pid")).unwrap();
        assert_eq!(
            read(root.path(), true).unwrap_err().check,
            "registry-record"
        );
    }
}
