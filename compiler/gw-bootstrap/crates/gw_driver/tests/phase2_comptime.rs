//! Phase 2 increment CT.1+: comptime corpus run-tests.
//!
//! Each `tests/corpus/pass/phase2_comptime/<name>.gw` is built via
//! `gw build`, the resulting executable is run, and its observable
//! behaviour is matched against the sibling expectation files:
//!
//! - `<name>.expected_exit` — required; integer exit code on a single
//!   line (modulo POSIX's 8-bit truncation).
//! - `<name>.expected_stdout` — optional; raw bytes the program is
//!   expected to write to stdout.
//!
//! Both the Cranelift (`fast`) and LLVM (`llvm`) backends are exercised
//! via separate cargo-test tests so any divergence is visible without
//! reading test names — same protocol as `phase1_run.rs` +
//! `llvm_backend.rs`, just over the comptime corpus.
//!
//! Skipped on Windows for the same reason as `phase1_run.rs`: the
//! driver shells out to `cc`.

#![cfg(not(windows))]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn corpus_dir() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest)
        .join("..")
        .join("..")
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("pass")
        .join("phase2_comptime")
        .canonicalize()
        .expect("canonicalize phase2_comptime corpus path")
}

fn gw_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gw"))
}

fn parse_expected_exit(path: &Path) -> i32 {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.trim()
        .parse::<i32>()
        .unwrap_or_else(|e| panic!("parse exit code in {}: {e}", path.display()))
}

fn unique_tmp(name: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!("gw-phase2-comptime-{name}-{pid}-{nanos}"));
    fs::create_dir(&p).expect("create tempdir");
    p
}

fn run_corpus(backend: &str) {
    let dir = corpus_dir();
    let gw = gw_binary();

    let mut entries: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut tested = 0;
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("gw") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("ascii stem")
            .to_string();
        let expected_exit_path = path.with_extension("expected_exit");
        let expected_exit = parse_expected_exit(&expected_exit_path);

        let tmp = unique_tmp(&format!("{backend}-{stem}"));
        let staged = tmp.join(format!("{stem}.gw"));
        fs::copy(&path, &staged).expect("copy source");

        let backend_arg: OsString = format!("--backend={backend}").into();
        let build_args: Vec<OsString> =
            vec!["build".into(), backend_arg, staged.as_os_str().to_owned()];
        let build = Command::new(&gw)
            .args(&build_args)
            .output()
            .expect("invoke gw build");
        if !build.status.success() {
            panic!(
                "`gw build --backend={backend} {}` failed\nstdout: {}\nstderr: {}",
                staged.display(),
                String::from_utf8_lossy(&build.stdout),
                String::from_utf8_lossy(&build.stderr),
            );
        }

        let exe = tmp.join(&stem);
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
            "{stem} ({backend}): expected exit {expected_exit}, got {actual_exit}"
        );

        let expected_stdout_path = path.with_extension("expected_stdout");
        if expected_stdout_path.is_file() {
            let expected = fs::read(&expected_stdout_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", expected_stdout_path.display()));
            assert_eq!(
                run.stdout,
                expected,
                "{stem} ({backend}): stdout mismatch\n  expected: {:?}\n  actual:   {:?}",
                String::from_utf8_lossy(&expected),
                String::from_utf8_lossy(&run.stdout),
            );
        }

        tested += 1;
        let _ = fs::remove_dir_all(&tmp);
    }

    assert!(tested > 0, "phase2_comptime corpus is empty");
}

#[test]
fn corpus_runs_on_fast_backend() {
    run_corpus("fast");
}

#[test]
fn corpus_runs_on_llvm_backend() {
    run_corpus("llvm");
}
