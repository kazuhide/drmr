//! External reference tools for short synthetic test media, never product processing.
use drmr_core::toolrun::{Completed, RunOptions, ToolError, ToolRunner};
use std::time::Duration;

pub fn run(runner: &ToolRunner, argv: &[&str], log_stdout: bool) -> Result<Completed, ToolError> {
    let argv: Vec<_> = argv.iter().map(|arg| (*arg).to_owned()).collect();
    runner.run_tool(
        &argv,
        "test-reference",
        &RunOptions {
            timeout: Some(Duration::from_secs(20)),
            log_stdout,
            max_output_chars: 64 * 1024,
            ..RunOptions::default()
        },
    )
}
