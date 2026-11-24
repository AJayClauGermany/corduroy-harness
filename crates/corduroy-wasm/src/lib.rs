//! Add-ons, the WebAssembly plugins of the corduroy harness.
//!
//! An add-on is a WebAssembly component built against the `corduroy:plugin/plugin` world in
//! `wit/corduroy.wit`. Any language that compiles to a component can implement one. This crate
//! turns such a component into a [`corduroy_core::Plugin`], so it joins the same context tree
//! as extensions (native plugins): its tools, listeners, prompt part, gate and services are effects that
//! vanish when it is unloaded.
//!
//! # Capabilities
//!
//! A component can only reach the outside world through its imports, and it has no imports
//! but the host interfaces of the plugin world. Each of those interfaces is one *capability*:
//! `fs-read`, `fs-write`, `kernel`, `events`, `log`, `clock`, `random`, `tools`, `state`,
//! `service`, `plugins`, `llm`, `secret`, `http`, `log-read`. There is no WASI, so there is no ambient authority to fall back on.
//!
//! * **At load time** the host links only the interfaces the plugin was granted. A component
//!   that imports an interface it was not granted is never instantiated.
//! * **At call time** every host function checks the resource it was asked for (a path, a
//!   kernel name, a topic) against the grant's [`Scope`](corduroy_core::Scope) and answers
//!   `denied` when it falls outside.
//!
//! Plugins are also metered: each call into a plugin gets a fuel budget and the plugin's
//! memory is capped, see [`Limits`]. A call that traps (out of fuel, out of memory, a panic)
//! kills the instance; the next call starts a new one, up to a limit of restarts.
//!
//! ```no_run
//! use corduroy_core::{App, Scope};
//! use corduroy_wasm::WasmRuntime;
//!
//! let runtime = WasmRuntime::new()?;
//! let plugin = runtime
//!     .load("plugins/target/wasm32-unknown-unknown/release/pi_tools.wasm")?
//!     .grant("fs-read", Scope::subtree("/work"))
//!     .grant("fs-write", Scope::subtree("/work/out"))
//!     .grant("kernel", Scope::none().allow("shell"));
//!
//! let mut app = App::new();
//! app.plugin(plugin);
//! # Ok::<(), corduroy_wasm::WasmError>(())
//! ```

#![expect(
    clippy::multiple_crate_versions,
    reason = "wasmtime's own dependency tree pulls two versions of hashbrown and embedded-io"
)]

pub mod plugin;
pub mod runtime;

pub use plugin::{CAPABILITIES, WORLD, WasmPlugin};
pub use runtime::{Limits, WasmError, WasmRuntime};
