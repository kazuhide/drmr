#![forbid(unsafe_code)]
mod control;
mod protocol;
mod server;
use protocol::{Mode, ToolSpec};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
fn config_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".fixture");
    PathBuf::from(name)
}
fn create(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}
fn install(path: &Path) -> io::Result<()> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(io::Error::other("configuration too large"));
    }
    let spec = ToolSpec::decode(bytes.as_slice())?;
    let mut destination = create(path)?;
    let mut source = File::open(env::current_exe()?)?;
    io::copy(&mut source, &mut destination)?;
    destination.sync_all()?;
    drop(destination);
    let config = config_path(path);
    spec.encode(create(&config)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let mut mode = fs::metadata(config_path(path))?.permissions();
    mode.set_readonly(true);
    fs::set_permissions(config_path(path), mode)?;
    Ok(())
}
fn main() {
    let args: Vec<std::ffi::OsString> = env::args_os().skip(1).collect();
    match control::child_entry(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(_) => std::process::exit(125),
    }
    if args.first().is_some_and(|a| a == "--install") {
        if args.len() != 2 || install(Path::new(&args[1])).is_err() {
            std::process::exit(1);
        }
        return;
    }
    let result = run(&args);
    match result {
        Ok(code) => std::process::exit(code),
        Err(_) => std::process::exit(125),
    }
}
fn run(raw_args: &[std::ffi::OsString]) -> io::Result<i32> {
    let mut spec = ToolSpec::decode(File::open(config_path(&env::current_exe()?))?)?;
    if spec.mode == Mode::Response
        && let Some(path) = spec.values.get("status_file")
    {
        let state = ToolSpec::decode(File::open(path)?)?;
        if state.mode != Mode::Response || state.values.contains_key("status_file") {
            return Err(io::Error::other("invalid response state"));
        }
        spec = state;
    }
    if spec.mode == Mode::ServerCli {
        return server::run(raw_args, &spec);
    }
    let args: Vec<String> = raw_args
        .iter()
        .map(|a| {
            a.to_str()
                .map(str::to_owned)
                .ok_or_else(|| io::Error::other("fixture arguments require UTF-8"))
        })
        .collect::<io::Result<_>>()?;
    if let Some(path) = spec.values.get("argv_file") {
        fs::write(path, args.join("\n"))?;
    }
    match spec.mode {
        Mode::Response => control::before_response(&spec)?,
        Mode::Packager => return packager(&args, &spec),
        Mode::Ffprobe | Mode::Ffmpeg | Mode::Mp4dump => return media(&args, &spec),
        Mode::ServerCli => return Err(io::Error::other("invalid fixture dispatch")),
    };
    if let Some(name) = spec.values.get("stdout_env") {
        let value =
            env::var(name).map_err(|_| io::Error::other("fixture environment unavailable"))?;
        for text in [
            spec.values
                .get("stdout_env_prefix")
                .map_or("", String::as_str),
            &value,
            spec.values
                .get("stdout_env_suffix")
                .map_or("", String::as_str),
        ] {
            io::stdout().write_all(text.as_bytes())?;
        }
    }
    io::stdout().write_all(&spec.stdout)?;
    io::stderr().write_all(&spec.stderr)?;
    if let Some(path) = spec.values.get("stdout_file") {
        io::copy(&mut File::open(path)?, &mut io::stdout())?;
    }
    for (name, stderr) in [("stdout_repeat", false), ("stderr_repeat", true)] {
        if let Some(count) = spec.values.get(name) {
            let mut count = count
                .parse::<usize>()
                .map_err(|_| io::Error::other("invalid repeat count"))?;
            let bytes = [b'x'; 8192];
            while count > 0 {
                let take = count.min(bytes.len());
                if stderr {
                    io::stderr().write_all(&bytes[..take])?;
                } else {
                    io::stdout().write_all(&bytes[..take])?;
                }
                count -= take;
            }
        }
    }
    if spec.sleep_ms > 0 {
        thread::sleep(Duration::from_millis(spec.sleep_ms));
    }
    Ok(spec.exit_code)
}

fn write_capture(path: &Path, args: &[String]) -> io::Result<()> {
    fs::write(path, args.join("\n"))
}
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}
fn packager(args: &[String], spec: &ToolSpec) -> io::Result<i32> {
    let values = &spec.values;
    if let Some(path) = values.get("STUB_PACKAGER_ARGV_FILE") {
        write_capture(Path::new(path), args)?;
    }
    if let Some(path) = values.get("STUB_PACKAGER_ARGV_DIR") {
        let path = Path::new(path);
        fs::create_dir_all(path)?;
        let count = fs::read_dir(path)?
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.len() == 8
                    && name.ends_with(".txt")
                    && name.as_bytes()[..4].iter().all(u8::is_ascii_digit)
            })
            .count();
        write_capture(&path.join(format!("{count:04}.txt")), args)?;
    }
    if let Some(mode) = values.get("STUB_PACKAGER_STDERR") {
        if mode == "echo_argv" {
            writeln!(
                io::stderr(),
                "shaka packager error; args were: {}",
                args.join(" ")
            )?;
        } else if !mode.is_empty() {
            writeln!(io::stderr(), "{mode}")?;
        }
    }
    let count = values
        .get("STUB_PACKAGER_SEGMENT_COUNT")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            if flag(args, "--segment_duration")
                .and_then(|s| s.parse::<f64>().ok())
                .is_some_and(|d| d >= 10_000.0)
            {
                1
            } else {
                3
            }
        });
    for descriptor in args.iter().filter(|a| a.contains('=') && a.contains(',')) {
        let fields: std::collections::BTreeMap<_, _> = descriptor
            .split(',')
            .filter_map(|f| f.split_once('='))
            .collect();
        for name in ["output", "init_segment"] {
            if let Some(value) = fields.get(name) {
                empty_output(Path::new(value))?;
            }
        }
        if let Some(template) = fields.get("segment_template") {
            for number in 1..=count {
                empty_output(Path::new(
                    &template.replace("$Number$", &number.to_string()),
                ))?;
            }
        }
    }
    if values.get("STUB_PACKAGER_HANG").is_some_and(|v| v == "1") {
        thread::sleep(Duration::from_secs(60));
    }
    Ok(values
        .get("STUB_PACKAGER_EXIT")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}
fn empty_output(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, [])
}
fn emit_file(root: &Path, name: &str) -> io::Result<()> {
    io::copy(&mut File::open(root.join(name))?, &mut io::stdout())?;
    Ok(())
}
fn media(args: &[String], spec: &ToolSpec) -> io::Result<i32> {
    let root = Path::new(
        spec.values
            .get("root")
            .ok_or_else(|| io::Error::other("missing media fixture root"))?,
    );
    let joined = args.join(" ");
    match spec.mode {
        Mode::Ffprobe => {
            if joined.contains("missing") {
                writeln!(io::stderr(), "missing: No such file or directory")?;
                return Ok(1);
            }
            let name = if joined.contains("-show_streams") {
                "probe.json"
            } else if joined.contains("-show_data_hash") {
                "hash.json"
            } else if joined.contains("-show_data") {
                "data.json"
            } else if joined.contains("format=duration") {
                "timing.json"
            } else {
                return Ok(3);
            };
            emit_file(root, name)?;
        }
        Mode::Ffmpeg => {
            if joined.contains("broken") {
                io::stderr().write_all(b"broken: Invalid data found when processing input\r\n")?;
                return Ok(183);
            }
            let last = args
                .last()
                .ok_or_else(|| io::Error::other("missing output argument"))?;
            if last == "-" && args.windows(2).any(|pair| pair == ["-f", "framemd5"]) {
                emit_file(root, "framemd5.txt")?;
            } else if joined.contains("-f s32le") {
                emit_file(root, "pcm.bin")?;
            } else if joined.contains("-f framemd5") {
                fs::copy(root.join("framemd5.txt"), last)?;
            } else {
                fs::write(last, [])?;
            }
        }
        Mode::Mp4dump => {
            if joined.contains("missing") {
                writeln!(
                    io::stderr(),
                    "ERROR: cannot open input file ({})",
                    spec.values.get("iv_spaced").map_or("", String::as_str)
                )?;
                return Ok(1);
            }
            emit_file(root, "tree.json")?;
        }
        _ => return Err(io::Error::other("not a media fixture mode")),
    }
    Ok(0)
}
