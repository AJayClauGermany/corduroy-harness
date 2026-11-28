//! **A prototype.** Extensions for the corduroy harness written in something other than
//! Rust, through C. Nothing here is decided or promised; it exists to find out what the
//! question is (G23 in `docs/open-questions.md`; what was found is in
//! `docs/c-extensions.md`).
//!
//! An extension is a native plugin: trusted, in the process, holding the host's authority.
//! Until now that meant Rust. Here a foreign library exports one function, which describes
//! the extension in a table; [`CExtension`] wraps the table as a [`Plugin`], and hands the
//! foreign code a table of the host's functions (`include/corduroy.h`) through which it
//! registers tools, services, gates, prompt parts, secrets, listeners and effects. Each of
//! those goes through the plugin's own [`Context`], so unloading the extension takes all of
//! it back, as for any plugin. The core is not changed and does not know.
//!
//! With the `harness` feature the table also takes a model provider, which is the one
//! asynchronous thing tried: the extension is handed a request and a completion, returns,
//! and completes whenever and from whichever thread it likes.
//!
//! # Where the `unsafe` is
//!
//! In [`mod@raw`]'s private half and nowhere else. [`abi`] only declares shapes. The
//! workspace forbids `unsafe`; this crate carries its own copy of the lint table to deny it
//! instead, and lifts that for the one module.
//!
//! [`Plugin`]: corduroy_core::Plugin
//! [`Context`]: corduroy_core::Context

pub mod abi;
#[expect(
    unsafe_code,
    reason = "the one place the harness crosses into C: see the module's own account"
)]
mod raw;

pub use raw::{CExtension, LoadError};
