//! Portable display expectations for native filesystem paths in committed test results.
use std::path::Path;

pub fn portable_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            format!("//{}", rest.replace('\\', "/"))
        } else {
            text.strip_prefix(r"\\?\")
                .unwrap_or(&text)
                .replace('\\', "/")
        }
    } else {
        text.into_owned()
    }
}
