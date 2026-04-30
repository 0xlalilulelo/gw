//! `arsenal` driver library — subcommand dispatch lives here so the
//! binary entry point stays trivial and integration tests can exercise
//! the full pipeline in-process.
//!
//! See `docs/architecture.md` Part H.1 (`arsenal` driver).
//!
//! Public entry point: [`run`].

pub mod cli;
pub mod cmd_build;
pub mod cmd_new;

use std::ffi::OsString;
use std::process::ExitCode;

/// Top-level entry point. Parses argv and dispatches to the matching
/// subcommand. Returns the process exit code.
pub fn run(argv: Vec<OsString>) -> ExitCode {
    cli::dispatch(&argv)
}
