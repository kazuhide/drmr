//! Real FFprobe passthrough with one explicitly selected profile field omitted.
use crate::protocol::ToolSpec;
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::Path,
    process::{Command, Stdio},
};
const LIMIT: u64 = 1024 * 1024;

fn captured(program: &str, args: &[OsString]) -> io::Result<(i32, Vec<u8>)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .unwrap()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > LIMIT {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("profile proxy output is invalid"));
    }
    Ok((child.wait()?.code().unwrap_or(125), bytes))
}

pub(super) fn run(args: &[OsString], spec: &ToolSpec) -> io::Result<i32> {
    let required = |name| {
        spec.values
            .get(name)
            .ok_or_else(|| io::Error::other("missing proxy configuration"))
    };
    let program = required("real_probe")?;
    let Some(position) = args.windows(2).position(|pair| {
        pair[0] == "-show_entries"
            && pair[1].to_str().is_some_and(|value| {
                value
                    .split([',', ':', '='])
                    .any(|field| field == "field_order")
            })
    }) else {
        return Command::new(program)
            .args(args)
            .status()
            .map(|status| status.code().unwrap_or(125));
    };
    let entries = args[position + 1]
        .to_str()
        .ok_or_else(|| io::Error::other("profile entries require UTF-8"))?;
    let mut classified = args.to_vec();
    classified[position + 1] = entries
        .replacen("stream=", "stream=codec_tag_string,", 1)
        .into();
    let (code, before) = captured(program, &classified)?;
    if code != 0 {
        io::stdout().write_all(&before)?;
        return Ok(code);
    }
    let text = std::str::from_utf8(&before).map_err(io::Error::other)?;
    let role = match (
        text.contains("\"codec_tag_string\": \"[27][0][0][0]\""),
        text.contains("\"codec_tag_string\": \"avc1\""),
    ) {
        (true, false) => "transport",
        (false, true) => "mp4",
        _ => return Err(io::Error::other("unrecognized proxy video container")),
    };
    let omit_role = required("omit_role")?;
    if !matches!(omit_role.as_str(), "transport" | "mp4") {
        return Err(io::Error::other("invalid proxy role"));
    }
    let selected = omit_role == role;
    let mut forwarded = args.to_vec();
    if selected {
        let without = entries
            .split(',')
            .filter(|field| *field != "field_order")
            .collect::<Vec<_>>()
            .join(",");
        if without == entries {
            return Err(io::Error::other("profile field was not removed"));
        }
        forwarded[position + 1] = without.into();
    }
    let (code, after) = captured(program, &forwarded)?;
    if code == 0 {
        let root = Path::new(required("audit_dir")?);
        let mut ordinal = 0_u32;
        let call = loop {
            let path = root.join(format!("{ordinal}"));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => ordinal += 1,
                Err(error) => return Err(error),
            }
        };
        fs::write(call.join("role"), role)?;
        let extension = args
            .last()
            .and_then(|input| Path::new(input).extension())
            .and_then(|value| value.to_str())
            .ok_or_else(|| io::Error::other("missing proxy input extension"))?;
        fs::write(call.join("input.extension"), extension)?;
        fs::write(call.join("selected"), if selected { "yes" } else { "no" })?;
        fs::write(call.join("before.entries"), entries)?;
        fs::write(
            call.join("after.entries"),
            forwarded[position + 1].to_str().unwrap(),
        )?;
        fs::write(call.join("before.json"), before)?;
        fs::write(call.join("after.json"), &after)?;
    }
    io::stdout().write_all(&after)?;
    Ok(code)
}
