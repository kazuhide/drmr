//! Select a real demux pass-through or a held TS-output response without recording arguments.
use crate::protocol::ToolSpec;
use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::Path,
    process::{Command, Stdio},
};

pub fn run(args: &[OsString], spec: &ToolSpec) -> io::Result<Option<i32>> {
    let program = Path::new(&spec.values["real_packager"]);
    if !program.is_absolute()
        || !program.is_file()
        || fs::canonicalize(program)? == fs::canonicalize(env::current_exe()?)?
    {
        return Err(io::Error::other("invalid actual packager"));
    }
    let master = args
        .iter()
        .filter(|arg| *arg == "--hls_master_playlist_output")
        .count();
    let descriptors: Vec<_> = args
        .iter()
        .map(|arg| arg.as_encoded_bytes())
        .filter(|arg| arg.starts_with(b"input="))
        .collect();
    let held = master == 1
        && !descriptors.is_empty()
        && descriptors.iter().all(|arg| {
            arg.split(|byte| *byte == b',')
                .any(|field| field.starts_with(b"segment_template=") && field.ends_with(b".ts"))
        });
    let demux = master == 0
        && !descriptors.is_empty()
        && descriptors.iter().all(|arg| {
            let fields: Vec<_> = arg.split(|byte| *byte == b',').collect();
            fields
                .iter()
                .any(|field| *field == b"stream=video" || *field == b"stream=audio")
                && fields
                    .iter()
                    .any(|field| field.starts_with(b"output=") && field.ends_with(b".mp4"))
                && !fields
                    .iter()
                    .any(|field| field.starts_with(b"segment_template="))
        });
    if !held && !demux {
        return Err(io::Error::other("unexpected packager route"));
    }
    let audit = spec
        .values
        .get("packager_route_audit")
        .ok_or_else(|| io::Error::other("missing route audit"))?;
    if held {
        record(audit, b"ts-output\n")?;
        return Ok(None);
    }
    // Inherit the supervised proxy's pipes and process group/job. No output is buffered here.
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .status()?;
    if status.success() {
        record(audit, b"demux\n")?;
    }
    Ok(Some(status.code().unwrap_or(125)))
}

fn record(path: &str, label: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)?.write_all(label)
}
