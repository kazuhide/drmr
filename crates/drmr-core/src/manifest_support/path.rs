//! `pathlib` and `posixpath` as `CPython` 3.11 implements the parts the manifest modules use:
//! `str(PurePosixPath(p))`, `.name` / `.stem` / `.parent` / `.parents`, `Path.resolve`
//! (`os.path.realpath(strict=False)`), `os.path.relpath`, `PurePath.relative_to`.
//!
//! Paths are handled as text with POSIX semantics, like `posixpath`; a path that is not valid
//! UTF-8 is a `ValueError` here, where Python would carry surrogate escapes until it encodes
//! the text (a documented divergence that only non-UTF-8 file names reach).

use super::py::{PyErr, PyOsError, repr_str};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// `os.fspath(path)` as text.
pub fn fspath(path: &Path) -> Result<String, PyErr> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        PyErr::Value(format!(
            "path is not valid UTF-8: {}",
            repr_str(&path.to_string_lossy())
        ))
    })
}

/// `PurePosixPath` parts: the root (`""`, `"/"` or `"//"`) and the components (`.` and empty
/// ones dropped, `..` kept).
fn parse(text: &str) -> (&'static str, Vec<&str>) {
    let stripped = text.trim_start_matches('/');
    let root = match text.len() - stripped.len() {
        0 => "",
        2 => "//",
        _ => "/",
    };
    let parts = stripped
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    (root, parts)
}

/// `str(PurePosixPath(text))`.
pub fn pure(text: &str) -> String {
    let (root, parts) = parse(text);
    let joined = format!("{root}{}", parts.join("/"));
    if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// `PurePosixPath(text).name`.
pub fn name(text: &str) -> &str {
    parse(text).1.last().copied().unwrap_or("")
}

/// `PurePosixPath(text).stem`: the name without its last suffix (a leading dot or a trailing
/// dot is not a suffix).
pub fn stem(text: &str) -> &str {
    let name = name(text);
    match name.rfind('.') {
        Some(index) if index > 0 && index < name.len() - 1 => &name[..index],
        _ => name,
    }
}

/// `str(PurePosixPath(text).parent)`.
pub fn parent(text: &str) -> String {
    let (root, mut parts) = parse(text);
    parts.pop();
    let joined = format!("{root}{}", parts.join("/"));
    if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// `[str(p) for p in PurePosixPath(text).parents]`, nearest first.
pub fn parents(text: &str) -> Vec<String> {
    let (root, parts) = parse(text);
    (0..parts.len())
        .rev()
        .map(|count| {
            let joined = format!("{root}{}", parts[..count].join("/"));
            if joined.is_empty() {
                ".".to_string()
            } else {
                joined
            }
        })
        .collect()
}

/// `posixpath.normpath(text)`.
pub fn normpath(text: &str) -> String {
    if text.is_empty() {
        return ".".to_string();
    }
    let initial = if text.starts_with("//") && !text.starts_with("///") {
        2
    } else {
        usize::from(text.starts_with('/'))
    };
    let mut components: Vec<&str> = Vec::new();
    for component in text.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        let keep = component != ".."
            || (initial == 0 && components.is_empty())
            || components.last() == Some(&"..");
        if keep {
            components.push(component);
        } else if !components.is_empty() {
            components.pop();
        }
    }
    let path = format!("{}{}", "/".repeat(initial), components.join("/"));
    if path.is_empty() {
        ".".to_string()
    } else {
        path
    }
}

fn current_dir() -> Result<String, PyErr> {
    let cwd = std::env::current_dir()
        .map_err(|err| PyErr::Os(PyOsError::from_io(&err, Path::new("."))))?;
    fspath(&cwd)
}

/// `posixpath.join(a, b)`.
fn join(head: &str, tail: &str) -> String {
    if tail.starts_with('/') {
        tail.to_string()
    } else if head.is_empty() || head.ends_with('/') {
        format!("{head}{tail}")
    } else {
        format!("{head}/{tail}")
    }
}

/// `posixpath.split(text)`.
fn split(text: &str) -> (String, String) {
    let index = text.rfind('/').map_or(0, |index| index + 1);
    let (head, tail) = text.split_at(index);
    let head = if !head.is_empty() && head.chars().any(|c| c != '/') {
        head.trim_end_matches('/')
    } else {
        head
    };
    (head.to_string(), tail.to_string())
}

/// `posixpath.abspath(text)`.
pub fn abspath(text: &str) -> Result<String, PyErr> {
    if text.starts_with('/') {
        Ok(normpath(text))
    } else {
        Ok(normpath(&join(&current_dir()?, text)))
    }
}

/// `Path(text).resolve` (non-strict `os.path.realpath`): symbolic links resolved as far as
/// the path exists, `..` applied after resolution, the result absolute and normalized. A
/// symbolic-link loop leaves the rest of the path unresolved (Python 3.11's `realpath`; its
/// `resolve` then raises `RuntimeError`, which no manifest path reaches).
pub fn resolve(path: &Path) -> Result<PathBuf, PyErr> {
    let text = fspath(path)?;
    let mut seen = HashMap::new();
    let (resolved, _) = join_realpath(String::new(), &text, &mut seen)?;
    Ok(PathBuf::from(pure(&abspath(&resolved)?)))
}

fn join_realpath(
    mut path: String,
    rest: &str,
    seen: &mut HashMap<String, Option<String>>,
) -> Result<(String, bool), PyErr> {
    let mut rest = rest;
    if let Some(stripped) = rest.strip_prefix('/') {
        rest = stripped;
        path = "/".to_string();
    }
    while !rest.is_empty() {
        let (name, remainder) = rest.split_once('/').unwrap_or((rest, ""));
        rest = remainder;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            if path.is_empty() {
                path = "..".to_string();
            } else {
                let (head, tail) = split(&path);
                path = if tail == ".." {
                    join(&join(&head, ".."), "..")
                } else {
                    head
                };
            }
            continue;
        }
        let new_path = join(&path, name);
        let is_link = std::fs::symlink_metadata(&new_path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if !is_link {
            path = new_path;
            continue;
        }
        if let Some(cached) = seen.get(&new_path) {
            match cached {
                Some(resolved) => {
                    path.clone_from(resolved);
                    continue;
                }
                None => return Ok((join(&new_path, rest), false)),
            }
        }
        seen.insert(new_path.clone(), None);
        let target = std::fs::read_link(&new_path)
            .map_err(|err| PyErr::Os(PyOsError::from_io(&err, Path::new(&new_path))))?;
        let target = fspath(&target)?;
        let (resolved, ok) = join_realpath(std::mem::take(&mut path), &target, seen)?;
        if !ok {
            return Ok((join(&resolved, rest), false));
        }
        path.clone_from(&resolved);
        seen.insert(new_path, Some(resolved));
    }
    Ok((path, true))
}

/// `os.path.relpath(path, start)` (3.11).
pub fn relpath(path: &str, start: &str) -> Result<String, PyErr> {
    let start_absolute = abspath(start)?;
    let path_absolute = abspath(path)?;
    let start_list: Vec<&str> = start_absolute
        .split('/')
        .filter(|x| !x.is_empty())
        .collect();
    let path_list: Vec<&str> = path_absolute.split('/').filter(|x| !x.is_empty()).collect();
    let common = start_list
        .iter()
        .zip(&path_list)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative: Vec<&str> = vec![".."; start_list.len() - common];
    relative.extend(&path_list[common..]);
    if relative.is_empty() {
        Ok(".".to_string())
    } else {
        Ok(relative.join("/"))
    }
}

/// `str(PurePosixPath(path).relative_to(base))` with 3.11's `ValueError` message.
pub fn relative_to(path: &str, base: &str) -> Result<String, PyErr> {
    // 3.11 compares `[drv, root] + parts` (or the bare parts of a relative path).
    let absolute = |text: &str| -> (bool, Vec<String>) {
        let (root, parts) = parse(text);
        let mut out: Vec<String> = Vec::new();
        if !root.is_empty() {
            out.push(String::new());
            out.push(root.to_string());
        }
        out.extend(parts.iter().map(|part| (*part).to_string()));
        (!root.is_empty(), out)
    };
    let (rooted, parts) = absolute(path);
    let (_, base_parts) = absolute(base);
    let count = base_parts.len();
    let mismatch = if count == 0 {
        rooted
    } else {
        parts.len() < count || parts[..count] != base_parts[..]
    };
    if mismatch {
        return Err(PyErr::Value(format!(
            "{} is not in the subpath of {} OR one path is relative and the other is absolute.",
            repr_str(&pure(path)),
            repr_str(&pure(base))
        )));
    }
    let rest = parts[count..].join("/");
    Ok(if rest.is_empty() {
        ".".to_string()
    } else {
        rest
    })
}
