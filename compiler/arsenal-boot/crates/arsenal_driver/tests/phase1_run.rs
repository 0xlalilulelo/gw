//! Phase-1 run-tests: each `tests/snake_eater/pass/phase1/<name>.gw`
//! is built via `arsenal build`, the resulting executable is run, and
//! its exit code must match the contents of
//! `tests/snake_eater/pass/phase1/<name>.expected_exit`.
//!
//! Skipped on Windows for now — the driver invokes `cc`, which is not
//! present by default on Windows runners. Cross-platform linker
//! handling lands in a later increment.

#![cfg(not(windows))]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    // Cargo writes integration-test binaries under
    // `<workspace>/target/<profile>/deps/<name>-<hash>`. The driver
    // binary lives one level up at `<profile>/arsenal`. Use the
    // OUT_DIR-relative trick that cargo's `env!("CARGO_BIN_EXE_arsenal")`
    // affords us — the macro returns the absolute path to the
    // arsenal binary built for tests.
    PathBuf::from(env!("CARGO_BIN_EXE_arsenal"))
}

#[test]
fn corpus_runs_and_exits_correctly() {
    let dir = corpus_dir();
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

        // Compile into a per-test temp dir so we never collide with
        // sibling tests or the source tree.
        let tmp = unique_tmp(&stem);
        let staged_src = tmp.join(format!("{stem}.gw"));
        fs::copy(&path, &staged_src).expect("copy source");

        let arsenal = arsenal_binary();
        let build_args: Vec<OsString> = vec!["build".into(), staged_src.as_os_str().to_owned()];
        let build = Command::new(&arsenal)
            .args(&build_args)
            .status()
            .expect("invoke arsenal build");
        assert!(
            build.success(),
            "`arsenal build {}` failed (status {build:?})",
            staged_src.display()
        );

        let exe = tmp.join(&stem);
        let run = Command::new(&exe)
            .status()
            .unwrap_or_else(|e| panic!("invoke {}: {e}", exe.display()));
        let actual_exit = run
            .code()
            .expect("process exited via signal, not exit code");
        assert_eq!(
            actual_exit, expected_exit,
            "{stem}: expected exit {expected_exit}, got {actual_exit}"
        );

        tested += 1;
        let _ = fs::remove_dir_all(&tmp);
    }
    assert!(tested > 0, "phase1 corpus is empty");
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
    p.push(format!("arsenal-phase1-{name}-{pid}-{nanos}"));
    fs::create_dir(&p).expect("create tempdir");
    p
}
