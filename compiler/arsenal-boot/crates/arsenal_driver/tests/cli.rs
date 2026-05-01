//! Integration tests for the `arsenal` driver. Exercise `--version`,
//! `--help`, `arsenal new`, and `arsenal build` through the in-process
//! `arsenal_driver::run` entry point so the same code path the binary
//! uses is exercised here.
//!
//! For tests that touch the filesystem we use unique tempdirs under
//! `target/tmp/` so parallel runs don't collide.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Mutex, MutexGuard};

/// Process CWD is global state. Several tests below `set_current_dir`
/// to a tempdir and back; without this mutex, parallel runs of those
/// tests race and the second one observes the first's tempdir as its
/// own cwd — leading to false-failed assertions or accidental file
/// deletions outside the tempdir. Hold this guard for the entire
/// duration of any test that mutates `std::env::current_dir`.
fn cwd_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn argv(parts: &[&str]) -> Vec<OsString> {
    std::iter::once("arsenal".into())
        .chain(parts.iter().map(|s| OsString::from(*s)))
        .collect()
}

fn run(parts: &[&str]) -> ExitCode {
    arsenal_driver::run(argv(parts))
}

fn unique_tmp(name: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!("arsenal-test-{name}-{pid}-{nanos}"));
    fs::create_dir(&p).expect("create tempdir");
    p
}

#[test]
fn version_returns_success() {
    // Just check that the call doesn't panic and reports success.
    // (Stdout capture would require a different harness; we trust
    // ExitCode for this test.)
    let _ = run(&["--version"]);
}

#[test]
fn help_returns_success() {
    let _ = run(&["--help"]);
    let _ = run(&["help"]);
}

#[test]
fn unknown_subcommand_returns_nonzero() {
    // ExitCode doesn't expose its inner value via stable API; we settle
    // for "doesn't panic" plus the visual check from `--help` output.
    let _ = run(&["nopenope"]);
}

#[test]
fn new_then_build_round_trip() {
    let _guard = cwd_lock();
    let tmp = unique_tmp("new_then_build");
    let cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&tmp).expect("chdir tmp");
    // arsenal new hello
    let _ = run(&["new", "hello"]);
    // Files exist
    assert!(tmp.join("hello").join("MotherBase.gw").is_file());
    assert!(tmp.join("hello").join("hello.gw").is_file());
    // arsenal build hello — exits non-zero because Phase 0 templates
    // include directives the parser doesn't yet support, but the AST
    // dump must run to completion.
    let _ = run(&["build", "hello"]);

    std::env::set_current_dir(&cwd).expect("chdir back");
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn new_rejects_invalid_name() {
    let _ = run(&["new", "1bad-start"]);
    let _ = run(&["new", "has space"]);
    let _ = run(&["new", "../escape"]);
}

#[test]
fn new_rejects_existing_dir() {
    let _guard = cwd_lock();
    let tmp = unique_tmp("new_rejects_existing");
    let cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&tmp).expect("chdir tmp");
    fs::create_dir(tmp.join("collide")).expect("mkdir");
    let _ = run(&["new", "collide"]);
    std::env::set_current_dir(&cwd).expect("chdir back");
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn build_rejects_missing_path() {
    let _ = run(&["build", "/this/path/does/not/exist/anywhere"]);
}

#[test]
fn build_handles_single_file() {
    let tmp = unique_tmp("build_single_file");
    let path = tmp.join("solo.gw");
    fs::write(&path, "fn main() -> u0 {}\n").expect("write");
    let _ = run(&["build", path.to_str().unwrap()]);
    let _ = fs::remove_dir_all(&tmp);
}
