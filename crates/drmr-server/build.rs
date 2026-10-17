//! Rebuild when the embedded UI changes: `include_dir!` reads `static/` at compile
//! time, and Cargo only re-runs the macro when it is told the directory is an input.

fn main() {
    println!("cargo:rerun-if-changed=static");
}
