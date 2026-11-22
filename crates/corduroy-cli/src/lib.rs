//! The `corduroy` command: an agent described by a file, run in a terminal.
//!
//! [`agent`] reads the agent file and puts the harness together from it: providers, kernels,
//! secrets, extensions (native plugins) and add-ons (WebAssembly plugins) with their grants.
//! [`extension`] holds the kinds of extension a command was built with. [`terminal`] shows what
//! the agent does as it is committed to the log and asks a person what the gates ask.
//! [`command`] is the command line; the binary only calls it, so a binary of your own can be
//! the same command with more extension kinds.

#![expect(
    clippy::multiple_crate_versions,
    reason = "the wasmtime, oi-kit and reqwest dependency trees pull several versions of shared crates"
)]

pub mod agent;
pub mod command;
pub mod extension;
pub mod terminal;
