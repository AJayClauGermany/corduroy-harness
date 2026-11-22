//! The `corduroy` command: an agent described by a file, run in a terminal.
//!
//! [`agent`] reads the agent file and puts the harness together from it: providers, kernels,
//! secrets and WebAssembly plugins with their grants. [`terminal`] shows what the agent does
//! as it is committed to the log and asks a person what the gates ask. The binary ties them
//! to the command line.

#![expect(
    clippy::multiple_crate_versions,
    reason = "the wasmtime, oi-kit and reqwest dependency trees pull several versions of shared crates"
)]

pub mod agent;
pub mod terminal;
