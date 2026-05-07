//! LLVM-backend run-tests (Phase 13).
//!
//! Each program in this test runs through `arsenal build --backend=llvm`,
//! the resulting executable is run, and its exit code is matched against
//! the corpus's `.expected_exit` file. Stdout matching is added in B.5
//! once the LLVM backend supports the Print desugar.
//!
//! B.1: `01_exit_zero.gw` only — the tracer bullet. Subsequent
//! increments (B.2 int + control flow, B.3 float + `as`, B.4 aggregate
//! ABI, B.5 extern + Print) widen this list as the LLVM backend grows.
//!
//! Skipped on Windows for the same reason as `phase1_run.rs`: the
//! driver shells out to `cc`.
//!
//! Build prerequisite: `LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18`
//! (or the Linux equivalent) must be set when compiling the workspace.

#![cfg(not(windows))]

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Programs the LLVM backend can compile end-to-end as of B.1.
const SUPPORTED: &[&str] = &["01_exit_zero"];

fn corpus_dir() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest)
        .join("..")
        .join("..")
        .join("..")
        .join("..")
        .join("tests")
        .join("snake_eater")
        .join("pass")
        .join("phase1")
        .canonicalize()
        .expect("canonicalize phase1 corpus path")
}

fn arsenal_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arsenal"))
}

fn unique_tmp(name: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!("arsenal-llvm-{name}-{pid}-{nanos}"));
    fs::create_dir(&p).expect("create tempdir");
    p
}

#[test]
fn llvm_backend_compiles_and_runs_supported_programs() {
    let dir = corpus_dir();
    let arsenal = arsenal_binary();

    for stem in SUPPORTED {
        let src = dir.join(format!("{stem}.gw"));
        assert!(src.is_file(), "missing corpus source {}", src.display());
        let exit_path = src.with_extension("expected_exit");
        let expected_exit: i32 = fs::read_to_string(&exit_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", exit_path.display()))
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("parse {}: {e}", exit_path.display()));

        let tmp = unique_tmp(stem);
        let staged = tmp.join(format!("{stem}.gw"));
        fs::copy(&src, &staged).expect("copy source");

        let build_args: Vec<OsString> = vec![
            "build".into(),
            "--backend=llvm".into(),
            staged.as_os_str().to_owned(),
        ];
        let build = Command::new(&arsenal)
            .args(&build_args)
            .status()
            .expect("invoke arsenal build --backend=llvm");
        assert!(
            build.success(),
            "`arsenal build --backend=llvm {}` failed (status {build:?})",
            staged.display()
        );

        let exe = tmp.join(stem);
        let run = Command::new(&exe)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .unwrap_or_else(|e| panic!("invoke {}: {e}", exe.display()));
        let actual_exit = run
            .status
            .code()
            .expect("process exited via signal, not exit code");
        assert_eq!(
            actual_exit, expected_exit,
            "{stem}: expected exit {expected_exit}, got {actual_exit}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }
}
