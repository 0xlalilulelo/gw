//! Argv parsing and subcommand dispatch table for the `arsenal` driver.
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
    /// Subcommand name (matches the user's `arsenal NAME ...`).
    pub name: &'static str,
    /// One-line summary printed by `arsenal --help`.
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
        summary: "lex, parse, and dump the AST for the given project",
        run: crate::cmd_build::run,
    },
];

/// Crate version baked at compile time, surfaced by `arsenal --version`.
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
            println!("arsenal {VERSION}");
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
            eprintln!("arsenal: unknown subcommand `{name}`");
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
    let _ = writeln!(w, "arsenal {VERSION} — GW bootstrap compiler driver");
    let _ = writeln!(w);
    let _ = writeln!(w, "USAGE:");
    let _ = writeln!(w, "    arsenal <SUBCOMMAND> [ARGS...]");
    let _ = writeln!(w, "    arsenal --version");
    let _ = writeln!(w, "    arsenal --help");
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
