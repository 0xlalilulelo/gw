//! `gw` CLI entry point.
//!
//! See `docs/architecture.md` Part H.1.

fn main() -> std::process::ExitCode {
    gw_driver::run(std::env::args_os().collect())
}
