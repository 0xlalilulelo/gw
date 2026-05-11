//! Argv parsing and subcommand dispatch table for the `gw` driver.
//!
//! See `docs/architecture.md` Part H.1.
//!
//! The dispatch table is a plain `&[SubCmd]` array. Adding a future
//! subcommand is one new module with a `run` function plus one entry
//! here — no macros, no proc macros, no registration plumbing.

use std::ffi::OsString;
use std::process::ExitCode;

/// One subcommand entry.
pub struct SubCmd {
    /// Subcommand name (matches the user's `gw NAME ...`).
    pub name: &'static str,
    /// One-line summary printed by `gw --help`.
    pub summary: &'static str,
    /// Handler. Receives the argv tail *after* the subcommand name.
    pub run: fn(args: &[OsString]) -> ExitCode,
}

/// Compile-time table of all subcommands.
///
/// The order is the order printed by `--help`.
pub const SUBCMDS: &[SubCmd] = &[
    SubCmd {
        name: "new",
        summary: "scaffold a new GW project",
        run: crate::cmd_new::run,
    },
    SubCmd {
        name: "build",
        summary: "compile a single .gw file to an executable (Phase 1 increment 1)",
        run: crate::cmd_build::run,
    },
    SubCmd {
        name: "dump",
        summary: "lex, parse, and pretty-print the AST for the given path",
        run: crate::cmd_dump::run,
    },
];

/// Crate version baked at compile time, surfaced by `gw --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Top-level entry point. Parses the first argv entry, dispatches to
/// the matching subcommand, or handles `--version` / `--help` itself.
pub fn dispatch(argv: &[OsString]) -> ExitCode {
    // argv[0] is the program name; everything after is the user's input.
    let user = &argv[1..];
    if user.is_empty() {
        print_help();
        return ExitCode::from(2);
    }
    let head = &user[0];
    let head_str = head.to_string_lossy();
    match head_str.as_ref() {
        "--version" | "-V" => {
            println!("gw {VERSION}");
            ExitCode::SUCCESS
        }
        "--help" | "-h" | "help" => {
            print_help();
            ExitCode::SUCCESS
        }
        name => {
            for cmd in SUBCMDS {
                if cmd.name == name {
                    return (cmd.run)(&user[1..]);
                }
            }
            eprintln!("gw: unknown subcommand `{name}`");
            eprintln!();
            print_help_to(&mut std::io::stderr().lock());
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    print_help_to(&mut std::io::stdout().lock());
}

fn print_help_to(w: &mut dyn std::io::Write) {
    let _ = writeln!(w, "gw {VERSION} — GW bootstrap compiler driver");
    let _ = writeln!(w);
    let _ = writeln!(w, "USAGE:");
    let _ = writeln!(w, "    gw <SUBCOMMAND> [ARGS...]");
    let _ = writeln!(w, "    gw --version");
    let _ = writeln!(w, "    gw --help");
    let _ = writeln!(w);
    let _ = writeln!(w, "SUBCOMMANDS:");
    let max_name = SUBCMDS.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for cmd in SUBCMDS {
        let _ = writeln!(
            w,
            "    {name:<width$}    {summary}",
            name = cmd.name,
            width = max_name,
            summary = cmd.summary,
        );
    }
}
