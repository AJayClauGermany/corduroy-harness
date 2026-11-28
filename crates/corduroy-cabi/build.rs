//! Compiles the C extension in `examples/c/` for this crate's tests, where there is a C
//! compiler: as a static library that is linked in, and on Unix also as a shared library
//! that a test loads. Where there is none the tests that need it are left out (the cfg
//! `c_example` is not set), unless `CORDUROY_REQUIRE_C` is set (CI), which makes it an error.

use std::env;
use std::error::Error;
use std::io::Write as _;
use std::path::PathBuf;

const SOURCE: &str = "examples/c/notes.c";

fn main() -> Result<(), Box<dyn Error>> {
    let mut cargo = std::io::stdout().lock();
    for path in ["build.rs", SOURCE, "include/corduroy.h"] {
        writeln!(cargo, "cargo::rerun-if-changed={path}")?;
    }
    writeln!(cargo, "cargo::rerun-if-env-changed=CORDUROY_REQUIRE_C")?;
    writeln!(cargo, "cargo::rustc-check-cfg=cfg(c_example)")?;
    match example() {
        Ok(shared) => {
            writeln!(cargo, "cargo::rustc-cfg=c_example")?;
            if let Some(shared) = shared {
                let shared = shared.display();
                writeln!(cargo, "cargo::rustc-env=CORDUROY_NOTES_LIBRARY={shared}")?;
            }
        }
        Err(error) if env::var_os("CORDUROY_REQUIRE_C").is_some() => return Err(error),
        Err(_) => {}
    }
    Ok(())
}

/// Build the example, and return where its shared library is if one was built.
fn example() -> Result<Option<PathBuf>, Box<dyn Error>> {
    let mut build = cc::Build::new();
    build
        .file(SOURCE)
        .include("include")
        .std("c11")
        .cargo_warnings(false)
        .warnings(true)
        .extra_warnings(true)
        .warnings_into_errors(true);
    build.try_compile("corduroy_notes")?;
    if env::var_os("CARGO_CFG_UNIX").is_none() {
        return Ok(None);
    }
    let shared = PathBuf::from(env::var("OUT_DIR")?).join("libcorduroy_notes_loaded.so");
    let status = build
        .try_get_compiler()?
        .to_command()
        .args(["-shared", "-fPIC", "-std=c11", "-Iinclude", SOURCE, "-o"])
        .arg(&shared)
        .status()?;
    if !status.success() {
        return Err(format!("the C compiler could not make a shared library: {status}").into());
    }
    Ok(Some(shared))
}
