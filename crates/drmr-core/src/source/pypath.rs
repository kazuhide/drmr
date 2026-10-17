//! `pathlib` as the clear-HLS reader uses it: `Path.resolve` (non-strict, `CPython` 3.11.15 the
//! reference version: `posixpath.realpath` — its recursive `_joinrealpath` and 3.13's iterative rewrite agree,
//! 29 cases measured — then the `stat` of the result that raises `RuntimeError` on a symlink
//! loop, which 3.13 dropped; symlinks followed, missing components kept, `..` applied after the
//! prefix is resolved), `as_uri`, `with_suffix`, `stem`, `relative_to.as_posix`.

use super::Escaped;
use super::pyurl::quote_from_bytes;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// `Path(path).resolve`: absolute, every symlink that exists followed, nothing required to
/// exist. It fails: an `OSError` when the working directory is unreadable
/// (`os.getcwd`, for a relative `path`), a `ValueError` when a component it reaches holds a
/// NUL (`os.lstat` refuses an embedded null byte, and `realpath` catches only `OSError`), and a
/// `RuntimeError` when the result is a symlink loop: 3.11's `resolve` `stat`s what `realpath`
/// returned and raises on `ELOOP` alone (3.13 dropped that `stat`).
#[cfg(unix)]
pub fn resolve(path: &Path) -> Result<PathBuf, Escaped> {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    fn join(base: &[u8], name: &[u8]) -> Vec<u8> {
        let mut out = base.to_vec();
        if out != b"/" {
            out.push(b'/');
        }
        out.extend_from_slice(name);
        out
    }
    let as_path = |bytes: &[u8]| PathBuf::from(OsString::from_vec(bytes.to_vec()));

    let filename = path.as_os_str().as_bytes();
    let mut rest: Vec<Option<Vec<u8>>> = filename
        .split(|byte| *byte == b'/')
        .rev()
        .map(|part| Some(part.to_vec()))
        .collect();
    let mut part_count = rest.len();
    let mut resolved: Vec<u8> = if filename.starts_with(b"/") {
        b"/".to_vec()
    } else {
        std::env::current_dir()
            .map_err(|_| Escaped::new("OSError"))?
            .into_os_string()
            .into_vec()
    };
    // Symlink paths to their fully resolved targets; `None` while one is being resolved, which
    // is how a loop is recognised (non-strict: the looping link is kept as it is).
    let mut seen: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
    while part_count > 0 {
        let Some(name) = rest.pop().expect("part_count counts the parts left") else {
            let link = rest
                .pop()
                .flatten()
                .expect("a marker follows its symlink path");
            seen.insert(link, Some(resolved.clone()));
            continue;
        };
        part_count -= 1;
        if name.is_empty() || name == b"." {
            continue;
        }
        if name == b".." {
            let slash = resolved.iter().rposition(|byte| *byte == b'/').unwrap_or(0);
            resolved.truncate(slash);
            if resolved.is_empty() {
                resolved = b"/".to_vec();
            }
            continue;
        }
        if name.contains(&0) {
            return Err(Escaped::new("ValueError"));
        }
        let newpath = join(&resolved, &name);
        let is_link = std::fs::symlink_metadata(as_path(&newpath))
            .is_ok_and(|meta| meta.file_type().is_symlink());
        if !is_link {
            resolved = newpath;
            continue;
        }
        if let Some(cached) = seen.get(&newpath) {
            resolved = cached.clone().unwrap_or(newpath);
            continue;
        }
        let Ok(target) = std::fs::read_link(as_path(&newpath)) else {
            resolved = newpath;
            continue;
        };
        let target = target.into_os_string().into_vec();
        if target.starts_with(b"/") {
            resolved = b"/".to_vec();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Some(newpath));
        rest.push(None);
        let parts: Vec<Option<Vec<u8>>> = target
            .split(|byte| *byte == b'/')
            .rev()
            .map(|part| Some(part.to_vec()))
            .collect();
        part_count += parts.len();
        rest.extend(parts);
    }
    // `Path.resolve` of 3.11: `p.stat` on the result; `ELOOP` becomes
    // `RuntimeError("Symlink loop from …")`, every other error passes (non-strict).
    if let Err(error) = std::fs::metadata(as_path(&resolved))
        && error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
    {
        return Err(Escaped::new("RuntimeError"));
    }
    Ok(as_path(&resolved))
}

/// `Path(path).resolve` where the platform has no POSIX paths: the canonical path when it
/// exists, else the absolute path.
#[cfg(not(unix))]
pub fn resolve(path: &Path) -> Result<PathBuf, Escaped> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(Escaped::new("ValueError"));
    }
    match std::fs::canonicalize(path) {
        Ok(real) => Ok(real),
        Err(_) => std::path::absolute(path).map_err(|_| Escaped::new("OSError")),
    }
}

/// `path.as_uri` of an absolute path: `file://` and the path's bytes percent-encoded
/// (`quote_from_bytes(os.fsencode(path))`).
pub fn as_uri(path: &Path) -> String {
    format!("file://{}", quote_from_bytes(&os_bytes(path)))
}

#[cfg(unix)]
fn os_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().replace('\\', "/").into_bytes()
}

/// `path.stem` as text.
pub fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `path.with_suffix(suffix)` (`suffix` with its leading dot).
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    path.with_extension(suffix.trim_start_matches('.'))
}

/// `path.relative_to(directory).as_posix`, `None` when `path` is not under `directory`
/// (Python's `ValueError`).
pub fn relative_posix(path: &Path, directory: &Path) -> Option<String> {
    let relative = path.strip_prefix(directory).ok()?;
    let parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// `path.is_relative_to(root)` between two resolved paths (component-wise).
pub fn is_relative_to(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}
