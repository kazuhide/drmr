use std::{env, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=src/tool.rs");
    println!("cargo:rerun-if-changed=src/broker_bootstrap.rs");
    println!("cargo:rerun-if-changed=src/protocol.rs");
    println!("cargo:rerun-if-changed=src/server.rs");
    println!("cargo:rerun-if-changed=src/control.rs");
    println!("cargo:rerun-if-changed=src/probe_proxy.rs");
    println!("cargo:rerun-if-changed=src/packager_proxy.rs");
    let target = env::var("TARGET").unwrap();
    let native = env::var("HOST").unwrap() == target;
    let suffix = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let output =
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join(format!("drmr-native-fixture{suffix}"));
    let mut command = Command::new(env::var_os("RUSTC").unwrap());
    command.args([
        "--edition=2024",
        "-Dwarnings",
        "--crate-name",
        "drmr_native_fixture",
        "--target",
        &target,
    ]);
    command.arg("src/tool.rs");
    if native {
        command.arg("-o").arg(&output);
    } else {
        command
            .arg("--emit=obj")
            .arg("-o")
            .arg(output.with_extension("obj"));
    }
    let status = command.status().expect("native fixture compiler must run");
    assert!(status.success(), "native fixture must compile");
    println!("cargo:rustc-env=DRMR_NATIVE_FIXTURE={}", output.display());
}
