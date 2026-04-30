//! `arsenal new <name>` — scaffold a fresh GW project.
//!
//! Creates a directory `<name>/` containing:
//! - `MotherBase.gw` — minimal manifest stub. Phase 0 doesn't yet read
//!   it; the contents follow `docs/spec.md` §5.8.2 ("a `MotherBase.gw`
//!   at the project root drives the `arsenal build` command and is
//!   itself executable GW code") so the file is recognisable when the
//!   manifest reader lands.
//! - `hello.gw` — the canonical GW one-liner (`docs/spec.md` §5.15.1).

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const MOTHERBASE_TEMPLATE: &str = "// Project manifest. See docs/spec.md §5.8.2.
// The `arsenal cipher` package manager will read this once it lands;
// for now it is a placeholder.

#virtuous {
    package = \"%NAME%\";
    version = \"0.0.1\";
}
";

const HELLO_TEMPLATE: &str = "// docs/spec.md §5.15.1: bare string literals are sugar for Print.
\"Behold the Outer Heaven.\\n\";
";

/// Run `arsenal new <name>`.
pub fn run(args: &[OsString]) -> ExitCode {
    let Some(name) = args.first() else {
        eprintln!("arsenal new: missing project name");
        eprintln!("usage: arsenal new <name>");
        return ExitCode::from(2);
    };
    if args.len() > 1 {
        eprintln!("arsenal new: unexpected extra arguments");
        return ExitCode::from(2);
    }
    let name_str = name.to_string_lossy();
    if !is_valid_project_name(&name_str) {
        eprintln!("arsenal new: project name `{name_str}` must match [A-Za-z_][A-Za-z0-9_-]*");
        return ExitCode::from(2);
    }

    let dir = Path::new(name);
    if dir.exists() {
        eprintln!("arsenal new: `{}` already exists", dir.display());
        return ExitCode::from(1);
    }
    if let Err(e) = fs::create_dir(dir) {
        eprintln!(
            "arsenal new: failed to create directory `{}`: {e}",
            dir.display()
        );
        return ExitCode::from(1);
    }

    let mb_path = dir.join("MotherBase.gw");
    let mb_contents = MOTHERBASE_TEMPLATE.replace("%NAME%", &name_str);
    if let Err(e) = write_file(&mb_path, mb_contents.as_bytes()) {
        eprintln!("arsenal new: failed to write `{}`: {e}", mb_path.display());
        return ExitCode::from(1);
    }

    let hello_path = dir.join("hello.gw");
    if let Err(e) = write_file(&hello_path, HELLO_TEMPLATE.as_bytes()) {
        eprintln!(
            "arsenal new: failed to write `{}`: {e}",
            hello_path.display()
        );
        return ExitCode::from(1);
    }

    println!("created project `{name_str}`:");
    println!("  {}", mb_path.display());
    println!("  {}", hello_path.display());
    println!();
    println!("next:  arsenal build {name_str}");
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
        assert!(is_valid_project_name("snake-eater"));
        assert!(is_valid_project_name("_underscore"));
        assert!(!is_valid_project_name(""));
        assert!(!is_valid_project_name("1numeric"));
        assert!(!is_valid_project_name("has space"));
        assert!(!is_valid_project_name("../escape"));
    }
}
