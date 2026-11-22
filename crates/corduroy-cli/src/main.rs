//! `corduroy`: run an agent in a terminal. See [`corduroy_cli::command`].

use std::process::ExitCode;

use corduroy_cli::command::main_with;
use corduroy_cli::extension::Extensions;

fn main() -> ExitCode {
    main_with(&Extensions::builtin())
}
