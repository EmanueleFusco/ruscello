//! Out-of-process browser helper (wry/tao), spawned on demand by the engine.
//!
//! Placeholder created during project bootstrap (M0). See `docs/PLAN.md`.

use std::process::ExitCode;

use ruscello_platform::{language::Language, logging};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("{}", Language::system().logging_unavailable());
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let _log = logging::init("ruscello-webview")?;
    Ok(())
}
