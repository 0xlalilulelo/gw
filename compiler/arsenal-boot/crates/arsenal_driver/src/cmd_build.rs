//! `arsenal build <file.gw>` — lex, parse, resolve, type-check, lower
//! to MIR, codegen via Cranelift, and link with the system C compiler
//! to produce an executable next to the source file.
//!
//! Phase 1 increment 1: single-file builds only. Cross-file projects
//! land once we have a frequency-graph builder.

use arsenal_ast::FileArena;
use arsenal_lex::{render_simple, SourceMap};
use arsenal_mir::lower;
use arsenal_parse::parse;
use arsenal_resolve::resolve_module;
use arsenal_typeck::type_check;
use bumpalo::Bump;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Which codegen backend to use for `arsenal build`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Backend {
    /// Cranelift today; TPDE template encoder lands in Phase 7. Default.
    Fast,
    /// LLVM via inkwell. Phase 13.
    Llvm,
}

/// Run `arsenal build [--backend=fast|llvm] <file.gw>`.
pub fn run(args: &[OsString]) -> ExitCode {
    let mut path: Option<PathBuf> = None;
    let mut backend = Backend::Fast;
    for arg in args {
        let s = arg.to_string_lossy();
        if let Some(value) = s.strip_prefix("--backend=") {
            backend = match value {
                "fast" => Backend::Fast,
                "llvm" => Backend::Llvm,
                other => {
                    eprintln!(
                        "arsenal build: unknown --backend value `{other}` (expected `fast` or `llvm`)"
                    );
                    return ExitCode::from(2);
                }
            };
        } else if s.starts_with("--") {
            eprintln!("arsenal build: unknown flag `{s}`");
            return ExitCode::from(2);
        } else if path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            eprintln!("arsenal build: unexpected extra argument `{s}`");
            return ExitCode::from(2);
        }
    }
    let Some(path) = path else {
        eprintln!("arsenal build: missing source path");
        eprintln!("usage: arsenal build [--backend=fast|llvm] <file.gw>");
        return ExitCode::from(2);
    };

    if !path.is_file() {
        eprintln!(
            "arsenal build: `{}` is not a file (Phase 1 only supports single-file builds)",
            path.display()
        );
        return ExitCode::from(1);
    }
    if path.extension().and_then(|s| s.to_str()) != Some("gw") {
        eprintln!(
            "arsenal build: expected `.gw` extension, got `{}`",
            path.display()
        );
        return ExitCode::from(1);
    }

    match build_one(&path, backend) {
        Ok(out) => {
            println!("built `{}`", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("arsenal build: {e}");
            ExitCode::from(1)
        }
    }
}

fn build_one(src_path: &Path, backend: Backend) -> Result<PathBuf, String> {
    let contents = fs::read_to_string(src_path)
        .map_err(|e| format!("failed to read `{}`: {e}", src_path.display()))?;
    let mut sm = SourceMap::new();
    let display_name = src_path.display().to_string();
    let file = sm.add_file(display_name.clone(), contents);
    let bytes = sm.get(file).expect("just inserted").contents.as_bytes();

    let bump = Bump::new();
    let arena = FileArena::new(&bump, file);
    let (root, mut diags) = parse(file, bytes, &arena);
    let resolved = resolve_module(root, &sm, &mut diags);
    let typed = type_check(&resolved, &sm, &mut diags);

    if diags.has_errors() {
        for d in diags.iter() {
            eprintln!("  {}", render_simple(d, &sm));
        }
        return Err(format!(
            "{n} error(s) in `{}`",
            display_name,
            n = diags.error_count()
        ));
    }

    let mir = lower(&typed, &resolved, &sm);

    let stem = src_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("cannot derive output name from `{}`", src_path.display()))?;
    let out_dir = src_path.parent().unwrap_or_else(|| Path::new("."));
    let object_path = out_dir.join(format!("{stem}.o"));
    let exe_path = out_dir.join(executable_name(stem));

    let triple = target_lexicon::Triple::host();
    let object_bytes = match backend {
        Backend::Fast => arsenal_codegen_fast::compile_program(&mir, triple, stem)
            .map_err(|e| format!("codegen failed: {e}"))?,
        Backend::Llvm => arsenal_codegen_llvm::compile_program(&mir, triple, stem)
            .map_err(|e| format!("codegen failed: {e}"))?,
    };
    fs::write(&object_path, &object_bytes)
        .map_err(|e| format!("failed to write `{}`: {e}", object_path.display()))?;

    link_executable(&object_path, &exe_path)?;
    // Clean up the intermediate object — the user only asked for the
    // executable. Best-effort; ignore failure.
    let _ = fs::remove_file(&object_path);

    Ok(exe_path)
}

fn executable_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn link_executable(object_path: &Path, exe_path: &Path) -> Result<(), String> {
    // Phase 1 strategy: shell out to the system C compiler (`cc`),
    // which exists on every supported host (clang on macOS, gcc on
    // Linux, optional on Windows). The architecture's eventual
    // bundled-lld pipeline (Part J.3) lands later.
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let status = Command::new(&cc)
        .arg(object_path)
        .arg("-o")
        .arg(exe_path)
        .status()
        .map_err(|e| format!("failed to invoke `{cc}`: {e}"))?;
    if !status.success() {
        return Err(format!("`{cc}` exited with status {status}"));
    }
    Ok(())
}
