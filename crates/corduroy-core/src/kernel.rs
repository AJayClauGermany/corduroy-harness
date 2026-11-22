//! Kernels: long-lived interpreters that keep state between calls.
//!
//! A one-shot command forgets everything when it exits. A [`Kernel`] does not: variables, the
//! working directory, loaded data and background work all survive from one cell to the next,
//! so an agent can build up a computation over many steps instead of re-deriving it each time.
//!
//! Kernels are registered by name in the [`Kernels`] registry, which the host provides as the
//! service named [`KERNELS`]. Implementations live outside the core: `corduroy-shell` has a
//! persistent POSIX shell.

use std::error::Error;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::registry::Registry;

/// Name of the service under which the host provides the [`Kernels`] registry.
pub const KERNELS: &str = "kernels";

/// The registry of every running kernel.
pub type Kernels = Registry<Mutex<dyn Kernel>>;

/// Most output kept from one cell, in bytes. The rest is dropped and the cell is marked
/// [`truncated`](Cell::truncated).
pub const MAX_OUTPUT: usize = 1 << 20;

/// What running one cell produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Everything the cell wrote, standard output and standard error interleaved.
    pub output: String,
    /// The cell's exit status; zero means success.
    pub status: i32,
    /// Whether output beyond [`MAX_OUTPUT`] was dropped.
    pub truncated: bool,
}

/// Why a cell could not be run to completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    /// No kernel is registered under this name.
    Unknown(String),
    /// The kernel could not be started or spoken to.
    Unavailable(String),
    /// The cell ran past its deadline. The kernel was restarted and its state is gone.
    TimedOut(Duration),
    /// The kernel exited while running the cell, for example because the cell called `exit`.
    /// It will be restarted, without its state, on the next cell.
    Exited {
        /// What the cell wrote before the kernel went away.
        output: String,
    },
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown kernel `{name}`"),
            Self::Unavailable(reason) => write!(f, "kernel unavailable: {reason}"),
            Self::TimedOut(limit) => write!(
                f,
                "cell exceeded {}ms; the kernel was restarted and its state was lost",
                limit.as_millis()
            ),
            Self::Exited { output } => write!(
                f,
                "the kernel exited and its state was lost; output so far: {output}"
            ),
        }
    }
}

impl Error for KernelError {}

/// A long-lived interpreter.
pub trait Kernel: Send {
    /// Run `code` in the kernel's existing state and wait up to `timeout` for it to finish.
    ///
    /// # Errors
    ///
    /// Returns a [`KernelError`] if the kernel is unavailable, the cell times out, or the
    /// kernel exits. A cell that merely fails reports that through [`Cell::status`].
    fn exec(&mut self, code: &str, timeout: Duration) -> Result<Cell, KernelError>;

    /// Throw the kernel's state away. The next cell starts from scratch.
    fn reset(&mut self);
}

impl Registry<Mutex<dyn Kernel>> {
    /// Run `code` in the kernel registered as `name`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unknown`] if there is no such kernel, or whatever
    /// [`Kernel::exec`] returns.
    pub fn exec(&self, name: &str, code: &str, timeout: Duration) -> Result<Cell, KernelError> {
        let kernel = self
            .get(name)
            .ok_or_else(|| KernelError::Unknown(name.to_owned()))?;
        // A kernel that panicked mid-cell is in an unknown state, but `exec` copes with a
        // dead process by restarting it, so the poisoned guard is still usable.
        let mut kernel = kernel.lock().unwrap_or_else(PoisonError::into_inner);
        kernel.exec(code, timeout)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// A kernel that remembers the last cell and echoes it.
    #[derive(Default)]
    struct Echo(String);

    impl Kernel for Echo {
        fn exec(&mut self, code: &str, _timeout: Duration) -> Result<Cell, KernelError> {
            let output = format!("{}{code}", self.0);
            self.0 = code.to_owned();
            Ok(Cell {
                output,
                status: 0,
                truncated: false,
            })
        }

        fn reset(&mut self) {
            self.0.clear();
        }
    }

    #[test]
    fn kernels_are_found_by_name() {
        let kernels = Kernels::new();
        kernels
            .insert("echo", Arc::new(Mutex::new(Echo::default())))
            .unwrap();
        let wait = Duration::from_secs(1);

        assert_eq!(kernels.exec("echo", "a", wait).unwrap().output, "a");
        assert_eq!(kernels.exec("echo", "b", wait).unwrap().output, "ab");
        assert_eq!(
            kernels.exec("python", "1", wait),
            Err(KernelError::Unknown("python".into()))
        );
    }
}
