#[cfg(feature = "desktop")]
fn main() {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let target = std::env::var("TARGET").expect("Cargo must provide TARGET");
    let suffix = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let cli = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("binaries")
        .join(format!("drmr-{target}{suffix}"));
    let mut file = std::fs::File::open(&cli)
        .expect("Run tools/build_desktop.py before enabling the desktop feature");
    let mut digest = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let size = file.read(&mut buffer).expect("Cannot read the staged CLI");
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    println!("cargo:rustc-env=DRMR_CLI_SHA256={:x}", digest.finalize());
    println!("cargo:rerun-if-changed={}", cli.display());
    tauri_build::build();
}

#[cfg(not(feature = "desktop"))]
fn main() {}
