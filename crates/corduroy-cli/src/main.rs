//! `corduroy`: run an agent in a terminal. See [`corduroy_cli::command`].

use std::process::ExitCode;

use corduroy_cli::command::main_standard;
use corduroy_std::Standard;

fn main() -> ExitCode {
    main_standard(&Standard::new())
}
