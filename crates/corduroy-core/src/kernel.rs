//! Kernels: long-lived interpreters that keep state between calls.
//!
//! A one-shot command forgets everything when it exits. A [`Kernel`] does not: variables, the
//! working directory, loaded data and background work all survive from one cell to the next,
//! so an agent can build up a computation over many steps instead of re-deriving it each time.
//!
//! Kernels are registered by name in the [`Kernels`] registry, which the host provides as the
//! service named [`KERNELS`]. Implementations live outside the core: `corduroy-shell` has a
//! persistent POSIX shell.
//!
//! A kernel is also the only way to start a long-lived process, such as an MCP server that
//! speaks over its standard input and output: [`Kernel::spawn`]. Whatever a kernel does to the
//! cells it runs (a container, a sandbox, a timeout) it can do to its processes, so choosing
//! a kernel stays the one security decision about running code.

use std::error::Error;
use std::fmt;
use std::io::{Read, Write};
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
///
/// More reasons may be added, so match with a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
    /// The run the cell was for was asked to stop ([`crate::call::stopping`]), so the cell
    /// was ended. Like a timeout, that cost the kernel its state.
    Stopped {
        /// What the cell wrote before it was ended.
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
            Self::Stopped { output } => write!(
                f,
                "the run was stopped, so the cell was ended and the kernel's state was lost; \
                 output so far: {output}"
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

    /// Start `spawn` as a long-lived process, with pipes to its standard input and output,
    /// in the kernel's working directory and under whatever confinement the kernel applies
    /// to its cells. A kernel that cannot start processes refuses, which is the default.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unavailable`] if the kernel starts no processes or this one
    /// could not be started.
    fn spawn(&mut self, spawn: &Spawn) -> Result<Box<dyn Process>, KernelError> {
        Err(KernelError::Unavailable(format!(
            "this kernel does not start processes such as `{}`",
            spawn.program
        )))
    }
}

/// A process to start through [`Kernel::spawn`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spawn {
    /// The program, found as the kernel finds programs.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Environment variables to set for it, on top of what the kernel gives every process.
    pub env: Vec<(String, String)>,
}

/// A process a kernel started. Dropping it stops the process.
pub trait Process: Send {
    /// Its standard input. There is one, handed out once.
    fn stdin(&mut self) -> Option<Box<dyn Write + Send>>;

    /// Its standard output. There is one, handed out once.
    fn stdout(&mut self) -> Option<Box<dyn Read + Send>>;

    /// Whether it is still running.
    fn running(&mut self) -> bool;

    /// Stop it now.
    fn kill(&mut self);
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

    /// Start a process through the kernel registered as `name`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unknown`] if there is no such kernel, or whatever
    /// [`Kernel::spawn`] returns.
    pub fn spawn(&self, name: &str, spawn: &Spawn) -> Result<Box<dyn Process>, KernelError> {
        let kernel = self
            .get(name)
            .ok_or_else(|| KernelError::Unknown(name.to_owned()))?;
        let mut kernel = kernel.lock().unwrap_or_else(PoisonError::into_inner);
        kernel.spawn(spawn)
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

    #[test]
    fn a_kernel_starts_no_processes_unless_it_says_so() {
        let kernels = Kernels::new();
        kernels
            .insert("echo", Arc::new(Mutex::new(Echo::default())))
            .unwrap();
        let spawn = Spawn {
            program: "server".into(),
            ..Spawn::default()
        };
        let refused = kernels.spawn("echo", &spawn).err();
        assert!(
            matches!(refused, Some(KernelError::Unavailable(reason)) if reason.contains("`server`"))
        );
        assert!(matches!(
            kernels.spawn("none", &spawn).err(),
            Some(KernelError::Unknown(_))
        ));
    }
}
