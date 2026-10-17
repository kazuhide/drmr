//! Native equivalent of the server's controlled CLI fixture; no real media is generated.
use super::protocol::ToolSpec;
use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Write},
    path::Path,
    thread,
    time::Duration,
};
fn failure() -> io::Error {
    io::Error::other("invalid server fixture configuration")
}
pub(super) fn run(args: &[OsString], spec: &ToolSpec) -> io::Result<i32> {
    let root = Path::new(spec.values.get("control_dir").ok_or_else(failure)?);
    let mut index = 0u64;
    let call = loop {
        let call = root.join("calls").join(index.to_string());
        match fs::create_dir(&call) {
            Ok(()) => break call,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                index = index.checked_add(1).ok_or_else(failure)?
            }
            Err(error) => return Err(error),
        }
    };
    let mut argv = Vec::new();
    for arg in args {
        argv.extend(arg.to_string_lossy().as_bytes());
        argv.push(b'\n');
    }
    fs::write(call.join("argv"), argv)?;
    write_environment(&call.join("drm"), &["DRM_KID", "DRM_KEY", "DRM_IV"])?;
    // Readiness is published last, after the complete argument and DRM captures exist.
    write_environment(&call.join("inherited.tmp"), &["PATH", "AMBIENT_MARKER"])?;
    fs::rename(call.join("inherited.tmp"), call.join("inherited"))?;
    let lines = root.join("lines");
    if lines.is_file() {
        io::copy(&mut fs::File::open(lines)?, &mut io::stdout())?;
        io::stdout().flush()?;
    }
    if root.join("convert").is_file() {
        convert(args)?;
        writeln!(
            io::stdout(),
            "{{\"stage\": \"report\", \"level\": \"info\", \"message\": \"converted\"}}"
        )?;
        return Ok(0);
    }
    if root.join("hold").is_file() {
        loop {
            match fs::read_to_string(root.join("release")) {
                Ok(value) => {
                    let code = value.trim().parse().map_err(|_| failure())?;
                    match fs::remove_file(root.join("release")) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                    return Ok(code);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(error) => return Err(error),
            }
        }
    }
    let code = root.join("code");
    if code.is_file() {
        fs::read_to_string(code)?
            .trim()
            .parse()
            .map_err(|_| failure())
    } else {
        Ok(0)
    }
}
fn write_environment(path: &Path, names: &[&str]) -> io::Result<()> {
    let mut bytes = Vec::new();
    for name in names {
        bytes.extend(
            env::var_os(name)
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
        bytes.push(b'\n');
    }
    fs::write(path, bytes)
}
fn option<'a>(args: &'a [OsString], name: &str) -> io::Result<&'a OsStr> {
    args.windows(2)
        .rev()
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_os_str())
        .ok_or_else(failure)
}
fn convert(args: &[OsString]) -> io::Result<()> {
    let output = Path::new(option(args, "--output")?);
    let asset = option(args, "--asset-id")?;
    let base = option(args, "--manifest-local-url-base")?
        .to_str()
        .ok_or_else(failure)?;
    let manifest = Path::new(option(args, "--mpd")?);
    let download = manifest.ancestors().nth(4).ok_or_else(failure)?;
    let destination = output.join(asset);
    let manifests = destination
        .join("manifests/local/host")
        .join(asset)
        .join("dash");
    fs::create_dir_all(&manifests)?;
    let media = destination.join("cbcs");
    fs::create_dir_all(&media)?;
    let source = fs::read_to_string(manifest)?;
    let mut rewritten = String::new();
    for line in source.lines() {
        let mut line = line.replacen("value=\"cenc\"", "value=\"cbcs\"", 1);
        if let Some(begin) = line.find("<BaseURL>") {
            let start = begin + "<BaseURL>".len();
            if let Some(end) = line[start..].find("</BaseURL>") {
                if !line[start..start + end].contains('<') {
                    line.replace_range(start..start + end, base);
                }
            }
        }
        rewritten.push_str(&line);
        rewritten.push('\n');
    }
    fs::write(manifests.join("manifest.mpd"), rewritten)?;
    copy_tree(&download.join("cdn.example.test").join(asset), &media)
}
fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            return Err(failure());
        }
    }
    Ok(())
}
