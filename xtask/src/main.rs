//! Minimal dev-task runner: `cargo run -p xtask -- <task>`.
//!
//! Wraps common cargo invocations behind one entry point. Extend with packaging
//! tasks (AppImage, .deb, .rpm) as the project matures.

use std::process::Command;

use anyhow::{bail, Context};

fn main() -> anyhow::Result<()> {
    let task = std::env::args().nth(1).unwrap_or_else(|| "help".to_string());
    match task.as_str() {
        "build" => cargo(&["build", "--workspace"]),
        "build-release" => cargo(&["build", "--workspace", "--release"]),
        "test" => cargo(&["test", "--workspace"]),
        "fmt" => cargo(&["fmt", "--all"]),
        "lint" => cargo(&["clippy", "--workspace", "--all-targets"]),
        "run-cli" => cargo(&["run", "-p", "rrs-cli", "--", "check"]),
        _ => {
            println!("tasks: build | build-release | test | fmt | lint | run-cli");
            Ok(())
        }
    }
}

fn cargo(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(env!("CARGO"))
        .args(args)
        .status()
        .context("failed to launch cargo")?;
    if !status.success() {
        bail!("cargo {:?} failed with {status}", args);
    }
    Ok(())
}
