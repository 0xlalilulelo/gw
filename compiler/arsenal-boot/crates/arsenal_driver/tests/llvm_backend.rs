//! LLVM-backend run-tests (Phase 13).
//!
//! Each program in `tests/corpus/pass/phase1/` is built via
//! `arsenal build --backend=llvm`, the resulting executable is run, and
//! its observable behaviour is matched against any sibling expectation
//! files — same protocol as `phase1_run.rs`. As of B.5, the LLVM backend
//! has full corpus parity (226 / 226 programs); iterating the directory
//! rather than a hand-curated allow-list ensures any future corpus
//! program is automatically tested through both backends.
//!
//! Skipped on Windows for the same reason as `phase1_run.rs`: the
//! driver shells out to `cc`.
//!
//! Build prerequisite: `LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18`
//! (or the Linux equivalent) must be set when compiling the workspace.

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
        .join("phase1")
        .canonicalize()
        .expect("canonicalize phase1 corpus path")
}

fn arsenal_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arsenal"))
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
    p.push(format!("arsenal-llvm-{name}-{pid}-{nanos}"));
    fs::create_dir(&p).expect("create tempdir");
    p
}

#[test]
fn llvm_backend_runs_full_corpus() {
    let dir = corpus_dir();
    let arsenal = arsenal_binary();

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

        let tmp = unique_tmp(&stem);
        let staged = tmp.join(format!("{stem}.gw"));
        fs::copy(&path, &staged).expect("copy source");

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
            "{stem}: expected exit {expected_exit}, got {actual_exit}"
        );

        let expected_stdout_path = path.with_extension("expected_stdout");
        if expected_stdout_path.is_file() {
            let expected = fs::read(&expected_stdout_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", expected_stdout_path.display()));
            assert_eq!(
                run.stdout,
                expected,
                "{stem}: stdout mismatch\n  expected: {:?}\n  actual:   {:?}",
                String::from_utf8_lossy(&expected),
                String::from_utf8_lossy(&run.stdout),
            );
        }

        tested += 1;
        let _ = fs::remove_dir_all(&tmp);
    }
    assert!(tested > 0, "phase1 corpus is empty");
}
