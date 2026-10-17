//! Embedded browser assets and optional static directory overrides.

use crate::http::{FileFacts, FileSource};
use include_dir::{Dir, include_dir};
use std::path::{Path, PathBuf};

/// `static/` as it was when this binary was built.
pub static EMBEDDED: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/static");

/// Where the UI comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StaticRoot {
    /// The copy embedded at build time.
    Embedded,
    /// A directory on disk (`--static-dir`).
    Dir(PathBuf),
}

/// `posixpath.normpath` of a relative name, then `safe_join`'s refusals (absolute, `..`).
fn safe_name(filename: &str) -> Option<String> {
    let normalized = crate::pyutil::normpath(Path::new(filename));
    let text = crate::pyutil::path_text(&normalized);
    if text.starts_with('/') || text == ".." || text.starts_with("../") {
        return None;
    }
    Some(text)
}

/// The file `/static/<filename>` names, or `None` (404).
pub fn lookup(root: &StaticRoot, filename: &str) -> Option<FileFacts> {
    let name = safe_name(filename)?;
    match root {
        StaticRoot::Embedded => {
            let file = EMBEDDED.get_file(&name)?;
            Some(embedded_facts(&name, file.contents(), file.metadata()))
        }
        StaticRoot::Dir(directory) => {
            let path = directory.join(&name);
            if !crate::pyutil::is_file(&path) {
                return None;
            }
            FileFacts::stat(&path).ok()
        }
    }
}

/// `index.html` / `report.html` (sent by their own routes).
pub fn page(root: &StaticRoot, name: &str) -> std::io::Result<FileFacts> {
    match root {
        StaticRoot::Embedded => EMBEDDED
            .get_file(name)
            .map(|file| embedded_facts(name, file.contents(), file.metadata()))
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound)),
        StaticRoot::Dir(directory) => FileFacts::stat(&directory.join(name)),
    }
}

#[allow(clippy::cast_precision_loss)] // reason: a whole-second build time is exact in a double
fn embedded_facts(
    name: &str,
    data: &'static [u8],
    metadata: Option<&include_dir::Metadata>,
) -> FileFacts {
    let mtime = metadata
        .and_then(|metadata| {
            metadata
                .modified()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
        })
        .map_or(0.0, |elapsed| elapsed.as_secs() as f64);
    FileFacts {
        path: PathBuf::from(format!("/static/{name}")),
        source: FileSource::Embedded(data),
        size: u64::try_from(data.len()).unwrap_or(u64::MAX),
        mtime,
    }
}

/// Every embedded file as `(relative path, bytes)`.
pub fn embedded_files() -> Vec<(String, &'static [u8])> {
    fn walk(dir: &'static Dir<'static>, out: &mut Vec<(String, &'static [u8])>) {
        for file in dir.files() {
            out.push((
                file.path().to_string_lossy().replace('\\', "/"),
                file.contents(),
            ));
        }
        for child in dir.dirs() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(&EMBEDDED, &mut out);
    out.sort();
    out
}

const TYPES: [(&str, &str); 149] = [
    (".3g2", "audio/3gpp2"),
    (".3gp", "audio/3gpp"),
    (".3gpp", "audio/3gpp"),
    (".3gpp2", "audio/3gpp2"),
    (".a", "application/octet-stream"),
    (".aac", "audio/aac"),
    (".adts", "audio/aac"),
    (".ai", "application/postscript"),
    (".aif", "audio/x-aiff"),
    (".aifc", "audio/x-aiff"),
    (".aiff", "audio/x-aiff"),
    (".ass", "audio/aac"),
    (".au", "audio/basic"),
    (".avi", "video/x-msvideo"),
    (".avif", "image/avif"),
    (".bat", "text/plain"),
    (".bcpio", "application/x-bcpio"),
    (".bin", "application/octet-stream"),
    (".bmp", "image/bmp"),
    (".c", "text/plain"),
    (".cdf", "application/x-netcdf"),
    (".cpio", "application/x-cpio"),
    (".csh", "application/x-csh"),
    (".css", "text/css"),
    (".csv", "text/csv"),
    (".dll", "application/octet-stream"),
    (".doc", "application/msword"),
    (".dot", "application/msword"),
    (".dvi", "application/x-dvi"),
    (".eml", "message/rfc822"),
    (".eps", "application/postscript"),
    (".etx", "text/x-setext"),
    (".exe", "application/octet-stream"),
    (".gif", "image/gif"),
    (".gtar", "application/x-gtar"),
    (".h", "text/plain"),
    (".h5", "application/x-hdf5"),
    (".hdf", "application/x-hdf"),
    (".heic", "image/heic"),
    (".heif", "image/heif"),
    (".htm", "text/html"),
    (".html", "text/html"),
    (".ico", "image/vnd.microsoft.icon"),
    (".ief", "image/ief"),
    (".jpe", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".jpg", "image/jpeg"),
    (".js", "application/javascript"),
    (".json", "application/json"),
    (".ksh", "text/plain"),
    (".latex", "application/x-latex"),
    (".loas", "audio/aac"),
    (".m1v", "video/mpeg"),
    (".m3u", "application/vnd.apple.mpegurl"),
    (".m3u8", "application/vnd.apple.mpegurl"),
    (".man", "application/x-troff-man"),
    (".me", "application/x-troff-me"),
    (".mht", "message/rfc822"),
    (".mhtml", "message/rfc822"),
    (".mif", "application/x-mif"),
    (".mjs", "application/javascript"),
    (".mov", "video/quicktime"),
    (".movie", "video/x-sgi-movie"),
    (".mp2", "audio/mpeg"),
    (".mp3", "audio/mpeg"),
    (".mp4", "video/mp4"),
    (".mpa", "video/mpeg"),
    (".mpe", "video/mpeg"),
    (".mpeg", "video/mpeg"),
    (".mpg", "video/mpeg"),
    (".ms", "application/x-troff-ms"),
    (".n3", "text/n3"),
    (".nc", "application/x-netcdf"),
    (".nq", "application/n-quads"),
    (".nt", "application/n-triples"),
    (".nws", "message/rfc822"),
    (".o", "application/octet-stream"),
    (".obj", "application/octet-stream"),
    (".oda", "application/oda"),
    (".opus", "audio/opus"),
    (".p12", "application/x-pkcs12"),
    (".p7c", "application/pkcs7-mime"),
    (".pbm", "image/x-portable-bitmap"),
    (".pdf", "application/pdf"),
    (".pfx", "application/x-pkcs12"),
    (".pgm", "image/x-portable-graymap"),
    (".pl", "text/plain"),
    (".png", "image/png"),
    (".pnm", "image/x-portable-anymap"),
    (".pot", "application/vnd.ms-powerpoint"),
    (".ppa", "application/vnd.ms-powerpoint"),
    (".ppm", "image/x-portable-pixmap"),
    (".pps", "application/vnd.ms-powerpoint"),
    (".ppt", "application/vnd.ms-powerpoint"),
    (".ps", "application/postscript"),
    (".pwz", "application/vnd.ms-powerpoint"),
    (".py", "text/x-python"),
    (".pyc", "application/x-python-code"),
    (".pyo", "application/x-python-code"),
    (".qt", "video/quicktime"),
    (".ra", "audio/x-pn-realaudio"),
    (".ram", "application/x-pn-realaudio"),
    (".ras", "image/x-cmu-raster"),
    (".rdf", "application/xml"),
    (".rgb", "image/x-rgb"),
    (".roff", "application/x-troff"),
    (".rtx", "text/richtext"),
    (".sgm", "text/x-sgml"),
    (".sgml", "text/x-sgml"),
    (".sh", "application/x-sh"),
    (".shar", "application/x-shar"),
    (".snd", "audio/basic"),
    (".so", "application/octet-stream"),
    (".src", "application/x-wais-source"),
    (".srt", "text/plain"),
    (".sv4cpio", "application/x-sv4cpio"),
    (".sv4crc", "application/x-sv4crc"),
    (".svg", "image/svg+xml"),
    (".swf", "application/x-shockwave-flash"),
    (".t", "application/x-troff"),
    (".tar", "application/x-tar"),
    (".tcl", "application/x-tcl"),
    (".tex", "application/x-tex"),
    (".texi", "application/x-texinfo"),
    (".texinfo", "application/x-texinfo"),
    (".tif", "image/tiff"),
    (".tiff", "image/tiff"),
    (".tr", "application/x-troff"),
    (".trig", "application/trig"),
    (".tsv", "text/tab-separated-values"),
    (".txt", "text/plain"),
    (".ustar", "application/x-ustar"),
    (".vcf", "text/x-vcard"),
    (".vtt", "text/vtt"),
    (".wasm", "application/wasm"),
    (".wav", "audio/x-wav"),
    (".webm", "video/webm"),
    (".webmanifest", "application/manifest+json"),
    (".wiz", "application/msword"),
    (".wsdl", "application/xml"),
    (".xbm", "image/x-xbitmap"),
    (".xlb", "application/vnd.ms-excel"),
    (".xls", "application/vnd.ms-excel"),
    (".xml", "text/xml"),
    (".xpdl", "application/xml"),
    (".xpm", "image/x-xpixmap"),
    (".xsl", "application/xml"),
    (".xwd", "image/x-xwindowdump"),
    (".zip", "application/zip"),
];

/// `mimetypes.guess_type(name)[0]` (the type only; this implementation does not add the
/// `Content-Encoding` of a compressed suffix).
pub fn guess_type(name: &str) -> Option<&'static str> {
    let extension = match name.rfind('.') {
        Some(index) if index > 0 && !name[..index].ends_with('/') => &name[index..],
        _ => return None,
    };
    TYPES
        .iter()
        .find(|(ext, _)| *ext == extension)
        .or_else(|| {
            let lower = extension.to_ascii_lowercase();
            TYPES.iter().find(|(ext, _)| *ext == lower)
        })
        .map(|(_, mimetype)| *mimetype)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_normalised_and_confined() {
        assert_eq!(safe_name("js/../app.css").as_deref(), Some("app.css"));
        assert_eq!(safe_name("../keys.env"), None);
        assert_eq!(safe_name(".."), None);
        assert!(lookup(&StaticRoot::Embedded, "app.css").is_some());
        assert!(lookup(&StaticRoot::Embedded, "js/../app.css").is_some());
        assert!(lookup(&StaticRoot::Embedded, "nope.js").is_none());
    }

    #[test]
    fn types_are_cpython_3_11s() {
        assert_eq!(guess_type("api.js"), Some("application/javascript"));
        assert_eq!(guess_type("font.woff2"), None);
        assert_eq!(guess_type("README.md"), None);
        assert_eq!(guess_type("LICENSE"), None);
        assert_eq!(guess_type("APP.CSS"), Some("text/css"));
        assert_eq!(guess_type(".css"), None, "a dot file has no extension");
    }
}
