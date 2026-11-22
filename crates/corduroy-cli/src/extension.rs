//! Extension kinds: the native plugins an agent file can ask for.
//!
//! The kinds are the standard extensions' ([`corduroy_std::extension`]), re-exported here so
//! that a command of one's own can name them from the command it extends:
//!
//! ```toml
//! [extensions.scratch]
//! kind = "shell-kernel"            # every other key is the extension's own configuration
//! dir = "{workspace}/scratch"
//! ```
//!
//! A command of your own adds kinds and is otherwise the same command:
//!
//! ```no_run
//! use corduroy_cli::command::main_with;
//! use corduroy_cli::extension::Extensions;
//! use corduroy_harness::core::plugin_fn;
//!
//! fn main() -> std::process::ExitCode {
//!     let extensions = Extensions::builtin().kind("greeter", |asked| {
//!         let greeting = asked.text("greeting").unwrap_or("hello").to_owned();
//!         Ok(Box::new(plugin_fn(asked.name, &[], move |ctx| {
//!             ctx.emit("greeter/ready", format!("{greeting:?}"))?;
//!             Ok(())
//!         })))
//!     });
//!     main_with(&extensions)
//! }
//! ```
//!
//! To change more than the kinds (how the log is kept, whether add-ons load), start from
//! [`corduroy_std::Standard`] and run [`main_standard`](crate::command::main_standard).

pub use corduroy_std::extension::{Asked, Built, Extensions, kernel_kind};
