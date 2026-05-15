//! `gw build <file.gw>` — lex, parse, resolve, type-check, lower
//! to MIR, codegen via Cranelift, and link with the system C compiler
//! to produce an executable next to the source file.
//!
//! Phase 2 increment F.1: multi-file builds. The build target's
//! sibling `.gw` files in the same directory are auto-discovered and
//! folded into one resolved module / typed module / MIR program.
//! Manifest-driven (`build.gw`) builds remain a separate path.

use bumpalo::Bump;
use gw_ast::FileArena;
use gw_lex::{render_simple, SourceMap};
use gw_mir::lower;
use gw_parse::parse;
use gw_resolve::resolve_modules;
use gw_typeck::type_check;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Which codegen backend to use for `gw build`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Backend {
    /// Cranelift today; TPDE template encoder lands in Phase 7. Default.
    Fast,
    /// LLVM via inkwell. Phase 13.
    Llvm,
}

/// Run `gw build [--backend=fast|llvm] <file.gw>`.
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
                        "gw build: unknown --backend value `{other}` (expected `fast` or `llvm`)"
                    );
                    return ExitCode::from(2);
                }
            };
        } else if s.starts_with("--") {
            eprintln!("gw build: unknown flag `{s}`");
            return ExitCode::from(2);
        } else if path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            eprintln!("gw build: unexpected extra argument `{s}`");
            return ExitCode::from(2);
        }
    }
    let Some(path) = path else {
        eprintln!("gw build: missing source path");
        eprintln!("usage: gw build [--backend=fast|llvm] <file.gw>");
        return ExitCode::from(2);
    };

    if !path.is_file() {
        eprintln!(
            "gw build: `{}` is not a file (Phase 1 only supports single-file builds)",
            path.display()
        );
        return ExitCode::from(1);
    }
    if path.extension().and_then(|s| s.to_str()) != Some("gw") {
        eprintln!(
            "gw build: expected `.gw` extension, got `{}`",
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
            eprintln!("gw build: {e}");
            ExitCode::from(1)
        }
    }
}

fn build_one(src_path: &Path, backend: Backend) -> Result<PathBuf, String> {
    // Phase 2 increment F.1: auto-discover sibling `.gw` files in the
    // same directory and fold them into the build alongside the
    // primary file. Sort by filename so the def order is reproducible
    // regardless of `read_dir`'s OS-dependent traversal order.
    let dir = src_path.parent().unwrap_or_else(|| Path::new("."));
    let canon_target = src_path
        .canonicalize()
        .unwrap_or_else(|_| src_path.to_path_buf());
    let mut sibling_paths: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if p.extension().and_then(|e| e.to_str()) != Some("gw") {
                continue;
            }
            let canon = p.canonicalize().unwrap_or_else(|_| p.clone());
            if canon == canon_target {
                continue;
            }
            sibling_paths.push(p);
        }
    }
    sibling_paths.sort();

    let mut sm = SourceMap::new();
    let bump = Bump::new();

    let primary_contents = fs::read_to_string(src_path)
        .map_err(|e| format!("failed to read `{}`: {e}", src_path.display()))?;
    let primary_display = src_path.display().to_string();
    let primary_file = sm.add_file(primary_display.clone(), primary_contents);

    // Read + add each sibling to the source map up-front so the
    // contents byte-slice references stay valid for the whole build.
    let mut sibling_files = Vec::with_capacity(sibling_paths.len());
    for p in &sibling_paths {
        let contents =
            fs::read_to_string(p).map_err(|e| format!("failed to read `{}`: {e}", p.display()))?;
        let file = sm.add_file(p.display().to_string(), contents);
        sibling_files.push(file);
    }

    let primary_arena = FileArena::new(&bump, primary_file);
    let primary_bytes = sm
        .get(primary_file)
        .expect("just inserted")
        .contents
        .as_bytes();
    let (primary_root, mut diags) = parse(primary_file, primary_bytes, &primary_arena);

    let mut sibling_roots = Vec::with_capacity(sibling_files.len());
    for &fid in &sibling_files {
        let arena = FileArena::new(&bump, fid);
        let bytes = sm.get(fid).expect("just inserted").contents.as_bytes();
        let (root, sib_diags) = parse(fid, bytes, &arena);
        diags.merge(sib_diags);
        sibling_roots.push(root);
    }

    let resolved = resolve_modules(primary_root, &sibling_roots, &sm, &mut diags);
    let typed = type_check(&resolved, &sm, &mut diags);

    if diags.has_errors() {
        for d in diags.iter() {
            eprintln!("  {}", render_simple(d, &sm));
        }
        return Err(format!(
            "{n} error(s) in `{}`",
            primary_display,
            n = diags.error_count()
        ));
    }

    let mir = lower(&typed, &resolved, &sm);

    // Phase 3 increment B.3: borrow / init-state checker. Runs on
    // the converged MIR; emits `E0400` diagnostics for reads of
    // locals that aren't definitely-initialized on every incoming
    // control-flow path. If any borrow-check diag fires we abort
    // before codegen so we never lower an unsafe program.
    let borrow_diags = gw_borrow::check_program(&mir);
    if !borrow_diags.is_empty() {
        for d in &borrow_diags {
            eprintln!("  {}", render_simple(d, &sm));
        }
        return Err(format!(
            "{n} error(s) in `{}`",
            primary_display,
            n = borrow_diags.len(),
        ));
    }

    let stem = src_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("cannot derive output name from `{}`", src_path.display()))?;
    let out_dir = src_path.parent().unwrap_or_else(|| Path::new("."));
    let object_path = out_dir.join(format!("{stem}.o"));
    let exe_path = out_dir.join(executable_name(stem));

    let triple = target_lexicon::Triple::host();
    let object_bytes = match backend {
        Backend::Fast => gw_codegen_fast::compile_program(&mir, triple, stem)
            .map_err(|e| format!("codegen failed: {e}"))?,
        Backend::Llvm => gw_codegen_llvm::compile_program(&mir, triple, stem)
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
