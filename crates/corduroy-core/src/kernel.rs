//! Kernels: long-lived interpreters that keep state between calls.
//!
//! A one-shot command forgets everything when it exits. A [`Kernel`] does not: variables, the
//! working directory, loaded data and background work all survive from one cell to the next,
//! so an agent can build up a computation over many steps instead of re-deriving it each time.
//!
//! Kernels are registered by name in the [`Kernels`] registry, which the host provides as the
//! service named [`KERNELS`]. [`ShellKernel`] is the reference implementation: a persistent
//! POSIX shell.

use std::error::Error;
use std::fmt;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

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

/// A persistent POSIX shell. Variables, functions and the working directory carry over from
/// one cell to the next.
///
/// The shell is started on first use and restarted, empty, after a timeout or an `exit`.
#[derive(Debug)]
pub struct ShellKernel {
    program: String,
    dir: Option<PathBuf>,
    session: Option<Session>,
    cells: u64,
}

impl Default for ShellKernel {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellKernel {
    /// A kernel backed by `sh`.
    #[must_use]
    pub fn new() -> Self {
        Self::program("sh")
    }

    /// A kernel backed by another POSIX-compatible shell, such as `bash`.
    #[must_use]
    pub fn program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            dir: None,
            session: None,
            cells: 0,
        }
    }

    /// Start the shell in `dir` instead of the current directory.
    #[must_use]
    pub fn in_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    fn start(&self) -> Result<Session, KernelError> {
        let unavailable = |error: std::io::Error| KernelError::Unavailable(error.to_string());
        let mut command = Command::new(&self.program);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(dir) = &self.dir {
            command.current_dir(dir);
        }
        own_process_group(&mut command);
        let mut child = command.spawn().map_err(unavailable)?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(KernelError::Unavailable("shell has no pipes".to_owned()));
        };
        // From here on the shell's own diagnostics arrive interleaved with cell output.
        stdin.write_all(b"exec 2>&1\n").map_err(unavailable)?;
        Ok(Session {
            child,
            stdin,
            lines: read_lines(stdout),
        })
    }
}

impl Kernel for ShellKernel {
    fn exec(&mut self, code: &str, timeout: Duration) -> Result<Cell, KernelError> {
        let mut session = match self.session.take() {
            Some(session) => session,
            None => self.start()?,
        };
        self.cells += 1;
        let marker = format!("__corduroy_{}_{}__", std::process::id(), self.cells);
        // `command eval` keeps a syntax error from ending a non-interactive shell, and the
        // quoted here-document passes the cell through untouched. Cells read no input, so
        // they cannot swallow the commands queued behind them.
        let script = format!(
            "command eval \"$(cat <<'{marker}'\n{code}\n{marker}\n)\" </dev/null\n\
             printf '\\n{marker} %s\\n' \"$?\"\n"
        );
        if session.stdin.write_all(script.as_bytes()).is_err() || session.stdin.flush().is_err() {
            return Err(KernelError::Exited {
                output: String::new(),
            });
        }
        let cell = session.collect(&marker, timeout)?;
        // Only a session that finished its cell cleanly is kept; any other is dropped, which
        // kills the shell, and the next cell starts a fresh one.
        self.session = Some(session);
        Ok(cell)
    }

    fn reset(&mut self) {
        self.session = None;
    }
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Read output until the marker line that ends the current cell.
    fn collect(&self, marker: &str, timeout: Duration) -> Result<Cell, KernelError> {
        let deadline = Instant::now() + timeout;
        let mut output = String::new();
        let mut truncated = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => return Err(KernelError::TimedOut(timeout)),
                Err(RecvTimeoutError::Disconnected) => return Err(KernelError::Exited { output }),
            };
            let Some(status) = line.strip_prefix(marker) else {
                truncated |= !keep(&mut output, &line);
                continue;
            };
            // The marker is printed on a line of its own; drop the newline that put it there
            // so the output is exactly what the cell wrote.
            let written = output.strip_suffix('\n').unwrap_or(&output);
            return Ok(Cell {
                output: written.to_owned(),
                status: status.trim().parse().unwrap_or(-1),
                truncated,
            });
        }
    }
}

/// Append `line` to `output` unless that would exceed [`MAX_OUTPUT`]. Returns whether the
/// line was kept.
fn keep(output: &mut String, line: &str) -> bool {
    let fits = output.len() + line.len() <= MAX_OUTPUT;
    if fits {
        output.push_str(line);
    }
    fits
}

impl Drop for Session {
    fn drop(&mut self) {
        kill_process_group(&self.child);
        // Both fail only if the shell is already gone, which is the goal.
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

/// Forward the shell's output line by line from a thread, so reads can time out.
fn read_lines(stdout: ChildStdout) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        // Stops at end of output, on a read error, or once nobody is listening.
        while matches!(reader.read_until(b'\n', &mut line), Ok(read) if read > 0) {
            let text = String::from_utf8_lossy(&line).into_owned();
            if sender.send(text).is_err() {
                break;
            }
            line.clear();
        }
    });
    receiver
}

/// Put the shell in a process group of its own, so everything a cell started can be stopped
/// along with it.
#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_process_group(_command: &mut Command) {}

/// Stop whatever the shell left running. The standard library can only signal the shell
/// itself, so this asks `kill` to signal the group.
#[cfg(unix)]
fn kill_process_group(child: &Child) {
    let group = format!("-{}", child.id());
    let status = Command::new("kill")
        .args(["-KILL", "--", &group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // Failing to run `kill` leaves only the shell itself to be stopped by the caller.
    drop(status);
}

#[cfg(not(unix))]
fn kill_process_group(_child: &Child) {}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    const PATIENT: Duration = Duration::from_secs(10);

    fn run(kernel: &mut ShellKernel, code: &str) -> Cell {
        kernel.exec(code, PATIENT).unwrap()
    }

    #[test]
    fn state_survives_between_cells() {
        let mut kernel = ShellKernel::new();

        assert_eq!(run(&mut kernel, "total=40; cd /").output, "");
        let cell = run(&mut kernel, "echo $((total + 2)) in $(pwd)");

        assert_eq!(cell.output, "42 in /\n");
        assert_eq!(cell.status, 0);
        assert!(!cell.truncated);
    }

    #[test]
    fn output_is_exactly_what_the_cell_wrote() {
        let mut kernel = ShellKernel::new();

        assert_eq!(run(&mut kernel, "printf 'no newline'").output, "no newline");
        assert_eq!(
            run(&mut kernel, "echo out; echo err >&2").output,
            "out\nerr\n"
        );
        assert_eq!(run(&mut kernel, "printf 'a\\n\\n'").output, "a\n\n");
    }

    #[test]
    fn failures_and_syntax_errors_are_statuses_not_crashes() {
        let mut kernel = ShellKernel::new();
        run(&mut kernel, "kept=yes");

        assert_eq!(run(&mut kernel, "false").status, 1);
        let broken = run(&mut kernel, "if then");
        assert_ne!(broken.status, 0);
        assert!(broken.output.to_lowercase().contains("syntax error"));
        assert_eq!(run(&mut kernel, "echo $kept").output, "yes\n");
    }

    #[test]
    fn a_cell_cannot_read_the_commands_behind_it() {
        let mut kernel = ShellKernel::new();

        assert_eq!(run(&mut kernel, "cat").output, "");
        assert_eq!(run(&mut kernel, "echo still here").output, "still here\n");
    }

    #[test]
    fn a_timeout_restarts_the_kernel_without_its_state() {
        let mut kernel = ShellKernel::new();
        run(&mut kernel, "lost=yes");

        let error = kernel.exec("sleep 30", Duration::from_millis(200));

        assert_eq!(
            error,
            Err(KernelError::TimedOut(Duration::from_millis(200)))
        );
        assert_eq!(run(&mut kernel, "echo \"[$lost]\"").output, "[]\n");
    }

    #[test]
    fn exiting_the_shell_is_reported_and_recovered_from() {
        let mut kernel = ShellKernel::new();

        let error = kernel.exec("echo bye; exit 3", PATIENT).unwrap_err();

        assert_eq!(
            error,
            KernelError::Exited {
                output: "bye\n".into()
            }
        );
        assert_eq!(run(&mut kernel, "echo back").output, "back\n");
    }

    #[test]
    fn kernels_are_found_by_name() {
        let kernels = Kernels::new();
        kernels
            .insert("shell", Arc::new(Mutex::new(ShellKernel::new())))
            .unwrap();

        assert_eq!(
            kernels.exec("shell", "echo hi", PATIENT).unwrap().output,
            "hi\n"
        );
        assert_eq!(
            kernels.exec("python", "1", PATIENT),
            Err(KernelError::Unknown("python".into()))
        );
        assert_eq!(
            ShellKernel::program("/no/such/shell")
                .in_dir("/")
                .exec("true", PATIENT)
                .unwrap_err()
                .to_string(),
            "kernel unavailable: No such file or directory (os error 2)"
        );
    }
}
