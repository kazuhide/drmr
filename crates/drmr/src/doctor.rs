//! Discover external dependencies without running programs or loading configuration.

use serde_json::json;
use std::path::Path;
use std::process::ExitCode;

const TOOLS: [(&str, &str); 4] = [
    ("ffmpeg", "media processing (FFmpeg)"),
    ("ffprobe", "media inspection (FFmpeg)"),
    ("mp4dump", "MP4 structure inspection (Bento4)"),
    (
        "packager",
        "media encryption and packaging (Shaka Packager)",
    ),
];

pub fn run(as_json: bool) -> ExitCode {
    let search_path = std::env::var_os("PATH");
    let directories: Vec<_> = search_path
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    let tools: Vec<_> = TOOLS
        .iter()
        .map(|(name, purpose)| {
            let executable_name = if cfg!(windows) {
                format!("{name}.exe")
            } else {
                (*name).to_string()
            };
            let available = directories
                .iter()
                .any(|directory| is_executable(&directory.join(&executable_name)));
            (*name, *purpose, available)
        })
        .collect();
    let ready = tools.iter().all(|(_, _, available)| *available);
    if as_json {
        let tools: Vec<_> = tools
            .iter()
            .map(|(name, purpose, available)| {
                json!({"name": name, "purpose": purpose, "available": available, "required": true})
            })
            .collect();
        println!("{}", json!({"ready": ready, "tools": tools}));
    } else {
        println!("External media tools (searched on PATH):");
        for (name, purpose, available) in &tools {
            let status = if *available { "OK" } else { "MISSING" };
            println!("  {status:7} {name:8} {purpose}");
        }
        if ready {
            println!(
                "\nAll required tools are available. Versions and media compatibility were not checked."
            );
        } else {
            println!(
                "\nInstall the missing tools from FFmpeg, Bento4, or Shaka Packager and add their executable directories to PATH."
            );
            println!("See the README installation instructions, then run 'drmr doctor' again.");
        }
    }
    ExitCode::from(u8::from(!ready))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}
