//! Compiles the C extension in `examples/c/` when the `c-example` feature asks for it, so
//! that this crate's tests can link it in. Without the feature nothing here needs a C
//! compiler.

use std::error::Error;
use std::io::Write as _;

fn main() -> Result<(), Box<dyn Error>> {
    let mut cargo = std::io::stdout().lock();
    for path in ["build.rs", "examples/c/notes.c", "include/corduroy.h"] {
        writeln!(cargo, "cargo::rerun-if-changed={path}")?;
    }
    #[cfg(feature = "c-example")]
    cc::Build::new()
        .file("examples/c/notes.c")
        .include("include")
        .std("c11")
        .warnings(true)
        .extra_warnings(true)
        .warnings_into_errors(true)
        .try_compile("corduroy_notes")?;
    Ok(())
}
