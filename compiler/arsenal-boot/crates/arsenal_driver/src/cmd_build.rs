//! `arsenal build [path]` — lex, parse, and pretty-print the AST for
//! every `.gw` file in the given project directory (or `.` if none is
//! given).
//!
//! Phase 0 satisfies the exit criterion of *Part L Phase 0* by emitting
//! the typed AST dump on stdout; type checking, MIR, and codegen are
//! Phase 1+.

use arsenal_ast::{dump, FileArena};
use arsenal_lex::{render_simple, SourceMap};
use arsenal_parse::parse;
use bumpalo::Bump;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Run `arsenal build [path]`.
pub fn run(args: &[OsString]) -> ExitCode {
    let path = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if args.len() > 1 {
        eprintln!("arsenal build: unexpected extra arguments");
        return ExitCode::from(2);
    }

    let files = match collect_gw_files(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("arsenal build: cannot read `{}`: {e}", path.display());
            return ExitCode::from(1);
        }
    };

    if files.is_empty() {
        eprintln!(
            "arsenal build: no `.gw` files found in `{}`",
            path.display()
        );
        return ExitCode::from(1);
    }

    let mut sm = SourceMap::new();
    let mut total_errors: u32 = 0;

    for src_path in &files {
        let contents = match fs::read_to_string(src_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "arsenal build: failed to read `{}`: {e}",
                    src_path.display()
                );
                total_errors = total_errors.saturating_add(1);
                continue;
            }
        };
        let display_name = src_path.display().to_string();
        let file = sm.add_file(display_name.clone(), contents);
        let bytes = sm.get(file).expect("just inserted").contents.as_bytes();

        let bump = Bump::new();
        let arena = FileArena::new(&bump, file);
        let (root, diags) = parse(file, bytes, &arena);

        println!("=== {} ===", display_name);
        let s = dump(root, &sm);
        print!("{s}");
        if !diags.is_empty() {
            println!();
            println!("--- diagnostics ({}) ---", diags.len());
            for d in diags.iter() {
                println!("  {}", render_simple(d, &sm));
            }
        }
        println!();
        total_errors = total_errors.saturating_add(diags.error_count());
    }

    if total_errors > 0 {
        eprintln!(
            "arsenal build: {total_errors} error(s) across {n} file(s)",
            n = files.len()
        );
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Collect every `*.gw` file from `path`.
///
/// If `path` is a file, returns just that file. If `path` is a
/// directory, walks its **immediate children only** (a single level —
/// recursive walk is a Phase 1 concern that wants the proper module-
/// graph builder rather than a directory recursion).
fn collect_gw_files(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let meta = fs::metadata(path)?;
    if meta.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("gw") {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}
