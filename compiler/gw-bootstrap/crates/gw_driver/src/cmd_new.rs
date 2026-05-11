//! `gw new <name>` — scaffold a fresh GW project.
//!
//! Creates a directory `<name>/` containing:
//! - `build.gw` — minimal manifest stub. Phase 0 doesn't yet read it;
//!   the contents follow `docs/spec.md` §5.8.2 ("a `build.gw` at the
//!   project root drives the `gw build` command and is itself
//!   executable GW code") so the file is recognisable when the
//!   manifest reader lands.
//! - `hello.gw` — the canonical GW one-liner (`docs/spec.md` §5.15.1).

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const BUILD_TEMPLATE: &str = "// Project manifest. See docs/spec.md §5.8.2.
// The `gw` package manager will read this once it lands;
// for now it is a placeholder.

comptime {
    package = \"%NAME%\";
    version = \"0.0.1\";
}
";

const HELLO_TEMPLATE: &str = "// docs/spec.md §5.15.1: bare string literals are sugar for Print.
\"Hello, world.\\n\";
";

/// Run `gw new <name>`.
pub fn run(args: &[OsString]) -> ExitCode {
    let Some(name) = args.first() else {
        eprintln!("gw new: missing project name");
        eprintln!("usage: gw new <name>");
        return ExitCode::from(2);
    };
    if args.len() > 1 {
        eprintln!("gw new: unexpected extra arguments");
        return ExitCode::from(2);
    }
    let name_str = name.to_string_lossy();
    if !is_valid_project_name(&name_str) {
        eprintln!("gw new: project name `{name_str}` must match [A-Za-z_][A-Za-z0-9_-]*");
        return ExitCode::from(2);
    }

    let dir = Path::new(name);
    if dir.exists() {
        eprintln!("gw new: `{}` already exists", dir.display());
        return ExitCode::from(1);
    }
    if let Err(e) = fs::create_dir(dir) {
        eprintln!(
            "gw new: failed to create directory `{}`: {e}",
            dir.display()
        );
        return ExitCode::from(1);
    }

    let build_path = dir.join("build.gw");
    let build_contents = BUILD_TEMPLATE.replace("%NAME%", &name_str);
    if let Err(e) = write_file(&build_path, build_contents.as_bytes()) {
        eprintln!("gw new: failed to write `{}`: {e}", build_path.display());
        return ExitCode::from(1);
    }

    let hello_path = dir.join("hello.gw");
    if let Err(e) = write_file(&hello_path, HELLO_TEMPLATE.as_bytes()) {
        eprintln!("gw new: failed to write `{}`: {e}", hello_path.display());
        return ExitCode::from(1);
    }

    println!("created project `{name_str}`:");
    println!("  {}", build_path.display());
    println!("  {}", hello_path.display());
    println!();
    println!("next:  gw build {name_str}");
    ExitCode::SUCCESS
}

fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    Ok(())
}

fn is_valid_project_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        assert!(is_valid_project_name("hello"));
        assert!(is_valid_project_name("my-project"));
        assert!(is_valid_project_name("_underscore"));
        assert!(!is_valid_project_name(""));
        assert!(!is_valid_project_name("1numeric"));
        assert!(!is_valid_project_name("has space"));
        assert!(!is_valid_project_name("../escape"));
    }
}
