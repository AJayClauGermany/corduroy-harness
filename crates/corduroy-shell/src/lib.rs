//! A persistent POSIX shell as a corduroy [`Kernel`].
//!
//! [`ShellKernel`] keeps one shell process alive between cells, so variables, functions and
//! the working directory carry over. [`shell_kernel`] is a plugin that registers one in the
//! [`Kernels`] registry. The core defines what a kernel is; implementations such as this one,
//! or a sandboxed kernel that runs cells in a container, live outside it.

use std::fmt;
use std::io::{BufRead as _, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How often a running cell looks whether the run it is for was asked to stop.
const STOP_POLL: Duration = Duration::from_millis(100);

use corduroy_core::{
    Cell, KERNELS, Kernel, KernelError, Kernels, MAX_OUTPUT, Plugin, Process, Spawn, plugin_fn,
};

/// A plugin that provides a persistent shell as the kernel `name`, working in `dir`.
///
/// Unloading the plugin stops the shell and removes the kernel.
pub fn shell_kernel(name: impl Into<String>, dir: impl Into<PathBuf>) -> impl Plugin + 'static {
    shell_kernel_of(name, ShellKernel::new().in_dir(dir))
}

/// A plugin that provides `shell`, as configured (its program, its directory), as the kernel
/// `name`. Each time the plugin is applied it starts from a fresh copy of that configuration.
///
/// Unloading the plugin stops the shell and removes the kernel.
pub fn shell_kernel_of(name: impl Into<String>, shell: ShellKernel) -> impl Plugin + 'static {
    let name = name.into();
    let (program, dir) = (shell.program, shell.dir);
    plugin_fn(format!("kernel-{name}"), &[KERNELS], move |ctx| {
        let kernels = ctx.service::<Kernels>(KERNELS)?;
        let mut fresh = ShellKernel::program(program.clone());
        fresh.dir.clone_from(&dir);
        let kernel: Arc<Mutex<dyn Kernel>> = Arc::new(Mutex::new(fresh));
        kernels.register(ctx, name.clone(), kernel)?;
        Ok(())
    })
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

    /// Start `spawn` in the kernel's directory, as its own process group, with its standard
    /// error discarded. It is found on the `PATH`, as the shell would find it.
    fn spawn(&mut self, spawn: &Spawn) -> Result<Box<dyn Process>, KernelError> {
        let mut command = Command::new(&spawn.program);
        command
            .args(&spawn.args)
            .envs(spawn.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(dir) = &self.dir {
            command.current_dir(dir);
        }
        own_process_group(&mut command);
        let child = command.spawn().map_err(|error| {
            KernelError::Unavailable(format!("cannot start `{}`: {error}", spawn.program))
        })?;
        Ok(Box::new(ShellProcess { child }))
    }
}

/// A process a [`ShellKernel`] started. Dropping it stops the process and its group.
struct ShellProcess {
    child: Child,
}

impl Process for ShellProcess {
    fn stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        let stdin: Box<dyn Write + Send> = Box::new(self.child.stdin.take()?);
        Some(stdin)
    }

    fn stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        let stdout: Box<dyn Read + Send> = Box::new(self.child.stdout.take()?);
        Some(stdout)
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn kill(&mut self) {
        kill_process_group(&self.child);
        // Both fail only if the process is already gone, which is the goal.
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

impl Drop for ShellProcess {
    fn drop(&mut self) {
        self.kill();
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
            // Woken now and then, to end the cell if the run it is for was asked to stop.
            let line = match self.lines.recv_timeout(remaining.min(STOP_POLL)) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) if remaining <= STOP_POLL => {
                    return Err(KernelError::TimedOut(timeout));
                }
                Err(RecvTimeoutError::Timeout) if corduroy_core::call::stopping() => {
                    return Err(KernelError::Stopped { output });
                }
                Err(RecvTimeoutError::Timeout) => continue,
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
    use std::path::Path;

    use super::*;

    const PATIENT: Duration = Duration::from_secs(10);

    fn run(kernel: &mut ShellKernel, code: &str) -> Cell {
        kernel.exec(code, PATIENT).unwrap()
    }

    #[test]
    fn a_process_runs_in_the_kernels_directory_with_pipes_until_dropped() {
        let dir = std::env::temp_dir();
        let mut kernel = ShellKernel::new().in_dir(&dir);
        let spawn = Spawn {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "pwd; echo \"$GREETING\"; while read -r line; do echo \"got $line\"; done".into(),
            ],
            env: vec![("GREETING".into(), "hello".into())],
        };
        let mut process = kernel.spawn(&spawn).unwrap();
        let mut stdin = process.stdin().unwrap();
        let mut stdout = BufReader::new(process.stdout().unwrap());
        assert!(process.stdin().is_none(), "handed out once");

        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert_eq!(
            Path::new(line.trim()).canonicalize().unwrap(),
            dir.canonicalize().unwrap()
        );
        line.clear();
        stdout.read_line(&mut line).unwrap();
        assert_eq!(line, "hello\n");
        stdin.write_all(b"ping\n").unwrap();
        stdin.flush().unwrap();
        line.clear();
        stdout.read_line(&mut line).unwrap();
        assert_eq!(line, "got ping\n");

        assert!(process.running());
        process.kill();
        assert!(!process.running());
    }

    #[test]
    fn a_program_that_cannot_start_is_unavailable() {
        let spawn = Spawn {
            program: "/no/such/program".into(),
            ..Spawn::default()
        };
        let error = ShellKernel::new().spawn(&spawn).err();
        assert!(
            matches!(error, Some(KernelError::Unavailable(reason)) if reason.contains("/no/such/program"))
        );
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
    fn a_cell_ends_when_the_run_it_is_for_is_stopped() {
        use std::sync::atomic::{AtomicBool, Ordering};

        use corduroy_core::SessionId;
        use corduroy_core::call::Serve;

        let mut kernel = ShellKernel::new();
        run(&mut kernel, "lost=yes");
        let stop = Arc::new(AtomicBool::new(false));
        let asker = Arc::clone(&stop);
        let started = Instant::now();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            asker.store(true, Ordering::SeqCst);
        });

        let serve = Serve::session(SessionId::new("main"))
            .call("1")
            .stopped_by(stop);
        let (ended, _) = serve.run(|| kernel.exec("echo begun; sleep 30", PATIENT));

        assert_eq!(
            ended,
            Err(KernelError::Stopped {
                output: "begun\n".into()
            })
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        // As after a timeout, the next cell runs in a fresh shell.
        assert_eq!(run(&mut kernel, "echo \"[$lost]\"").output, "[]\n");
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
