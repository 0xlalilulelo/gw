//! `gw_codegen_llvm` build script.
//!
//! `llvm-sys`'s build script reports the LLVM 18 install directory as a
//! library search path, but LLVM 18's system-libs (zstd, ffi, xml2,
//! curses) live elsewhere — typically Homebrew's `lib` prefix on macOS.
//! Without it the link step fails with `library 'zstd' not found`.
//! Adding the Homebrew lib dirs unconditionally on macOS is harmless
//! for non-Homebrew environments (the linker silently ignores unknown
//! search paths). Linux installs ship LLVM's system-libs in
//! `/usr/lib`, which the linker already searches by default.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // Apple Silicon Homebrew prefix.
        println!("cargo:rustc-link-search=native=/opt/homebrew/lib");
        // Intel macOS Homebrew prefix.
        println!("cargo:rustc-link-search=native=/usr/local/lib");
    }
}
