//! Phase 2 increment F.1: multi-file build integration test.
//!
//! Each subdirectory under `tests/snake_eater/pass/phase2_multifile/`
//! is a multi-file project: a `main.gw` driver file plus zero or more
//! sibling `.gw` files in the same directory. The test stages the
//! whole subdirectory into a per-test temp dir, runs `arsenal build
//! main.gw`, executes the resulting binary, and matches its exit code
//! against `<subdir>/expected_exit`.
//!
//! Skipped on Windows for the same reason `phase1_run.rs` is — the
//! driver shells out to `cc`.
//!
//! Both the Cranelift (`fast`) and LLVM (`llvm`) backends are exercised
//! via separate cargo-test tests so a regression in either is visible
//! without reading test names.

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
        .join("phase2_multifile")
        .canonicalize()
        .expect("canonicalize phase2_multifile corpus path")
}

fn arsenal_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arsenal"))
}

fn parse_expected_exit(p: &Path) -> i32 {
    let raw = fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    raw.trim().parse::<i32>().expect("expected_exit is i32")
}

fn unique_tmp(name: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!("arsenal-phase2-multifile-{name}-{pid}-{nanos}"));
    fs::create_dir(&p).expect("create tempdir");
    p
}

fn copy_dir_contents(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).expect("read project dir") {
        let entry = entry.expect("dir entry");
        let p = entry.path();
        if p.is_file() {
            let name = p.file_name().expect("file name").to_owned();
            fs::copy(&p, dst.join(&name)).expect("copy file");
        }
    }
}

fn run_corpus(backend: &str) {
    let dir = corpus_dir();
    let mut projects: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .collect();
    projects.sort_by_key(|e| e.file_name());

    let mut tested = 0;
    for project in projects {
        let project_path = project.path();
        let project_name = project_path
            .file_name()
            .and_then(|s| s.to_str())
            .expect("ascii project name")
            .to_string();
        let expected_exit_path = project_path.join("expected_exit");
        let expected_exit = parse_expected_exit(&expected_exit_path);

        let tmp = unique_tmp(&format!("{backend}-{project_name}"));
        copy_dir_contents(&project_path, &tmp);
        let staged_main = tmp.join("main.gw");
        assert!(
            staged_main.exists(),
            "project {project_name} missing main.gw"
        );

        let arsenal = arsenal_binary();
        let backend_arg: OsString = format!("--backend={backend}").into();
        let build_args: Vec<OsString> = vec![
            "build".into(),
            backend_arg,
            staged_main.as_os_str().to_owned(),
        ];
        let build = Command::new(&arsenal)
            .args(&build_args)
            .output()
            .expect("spawn arsenal build");
        if !build.status.success() {
            panic!(
                "arsenal build failed for {project_name}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&build.stdout),
                String::from_utf8_lossy(&build.stderr),
            );
        }

        let exe = tmp.join("main");
        let run = Command::new(&exe).output().expect("spawn project binary");
        let exit = run.status.code().unwrap_or(-1);
        assert_eq!(
            exit, expected_exit,
            "project {project_name} ({backend}) expected exit {expected_exit}, got {exit}"
        );
        tested += 1;

        // Best-effort cleanup; ignore failure.
        let _ = fs::remove_dir_all(&tmp);
    }

    assert!(
        tested >= 1,
        "phase2_multifile corpus produced no test cases"
    );
}

#[test]
fn corpus_runs_on_fast_backend() {
    run_corpus("fast");
}

#[test]
fn corpus_runs_on_llvm_backend() {
    run_corpus("llvm");
}
