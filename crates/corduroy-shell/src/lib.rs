//! A persistent shell as a corduroy [`Kernel`]: a POSIX shell, or PowerShell.
//!
//! [`ShellKernel`] keeps one shell process alive between cells, so variables, functions and
//! the working directory carry over. [`shell_kernel`] is a plugin that registers one in the
//! [`Kernels`] registry. A POSIX shell (`sh`, `bash`) is the kernel of macOS and Linux;
//! PowerShell ([`ShellKernel::powershell`]) is there on every Windows, and on the others too
//! where `pwsh` is installed. The core defines what a kernel is; implementations such as this one,
//! or a sandboxed kernel that runs cells in a container, live outside it.
//!
//! The shell, and every process the kernel starts, begins from an empty environment and not
//! from the program's own, which is where a model provider's key is. It is given the plain
//! variables a process needs to run at all ([`PLAIN`]), each as the program itself has it,
//! and beyond those only what it was told to: [`ShellKernel::with_env`] sets a variable, and
//! [`ShellKernel::passing`] hands one of the program's own through by name.

use std::fmt;
use std::io::{BufRead as _, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// How often a running cell looks whether the run it is for was asked to stop.
const STOP_POLL: Duration = Duration::from_millis(100);

use corduroy_core::{
    Cell, KERNELS, Kernel, KernelError, Kernels, MAX_OUTPUT, Plugin, Process, Spawn, plugin_fn,
};

/// The environment variables every process a kernel starts is given.
///
/// Each has the value the program itself has for it, if it has one: where programs are
/// found, whose account this is, the language and time zone, and what a process on Windows
/// cannot start without. Nothing else of the program's environment reaches a shell, so a key
/// kept there does not.
pub const PLAIN: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TMPDIR",
    "TERM",
    // Windows.
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "USERNAME",
    "USERDOMAIN",
    "COMPUTERNAME",
    "APPDATA",
    "LOCALAPPDATA",
    "ALLUSERSPROFILE",
    "PUBLIC",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "PSModulePath",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "OS",
];

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
    let (program, dir, dialect) = (shell.program, shell.dir, shell.dialect);
    let (env, passed, prefix) = (shell.env, shell.passed, shell.prefix);
    plugin_fn(format!("kernel-{name}"), &[KERNELS], move |ctx| {
        let kernels = ctx.service::<Kernels>(KERNELS)?;
        let mut fresh = ShellKernel::program(program.clone());
        fresh.dir.clone_from(&dir);
        fresh.dialect = dialect;
        fresh.env.clone_from(&env);
        fresh.passed.clone_from(&passed);
        fresh.prefix.clone_from(&prefix);
        let kernel: Arc<Mutex<dyn Kernel>> = Arc::new(Mutex::new(fresh));
        kernels.register(ctx, name.clone(), kernel)?;
        Ok(())
    })
}

/// A persistent shell. Variables, functions and the working directory carry over from one
/// cell to the next.
///
/// The shell is started on first use and restarted, empty, after a timeout or an `exit`.
#[derive(Debug)]
pub struct ShellKernel {
    program: String,
    dialect: Dialect,
    dir: Option<PathBuf>,
    prefix: Vec<String>,
    /// Variables set for the shell and for what the kernel starts, beyond the plain ones.
    env: Vec<(String, String)>,
    /// Variables of the program's own environment handed through, by name.
    passed: Vec<String>,
    session: Option<Session>,
    cells: u64,
}

/// The language the shell speaks, which decides how a cell is sent and its end recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Posix,
    PowerShell,
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
            dialect: Dialect::Posix,
            dir: None,
            prefix: Vec::new(),
            env: Vec::new(),
            passed: Vec::new(),
            session: None,
            cells: 0,
        }
    }

    /// A kernel backed by PowerShell: `powershell` (Windows PowerShell, on every Windows) on
    /// Windows, `pwsh` (PowerShell 7) elsewhere.
    ///
    /// A cell is a PowerShell script, dot-sourced so that what it defines stays for the next.
    /// Everything it writes, errors and `Write-Host` included, is its output. Its status is
    /// 1 if it threw a terminating error, the exit code of the last native program it ran if
    /// that was not 0, and 0 otherwise; an error that does not stop the script is output, not
    /// a status. A native program that reads its standard input waits for input that never
    /// comes, until the cell times out.
    #[must_use]
    pub fn powershell() -> Self {
        Self::powershell_program(if cfg!(windows) { "powershell" } else { "pwsh" })
    }

    /// A kernel backed by the PowerShell `program`, such as `pwsh` on Windows when
    /// PowerShell 7 is installed. See [`powershell`](Self::powershell).
    #[must_use]
    pub fn powershell_program(program: impl Into<String>) -> Self {
        Self {
            dialect: Dialect::PowerShell,
            ..Self::program(program)
        }
    }

    /// Start the shell in `dir` instead of the current directory.
    #[must_use]
    pub fn in_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    /// Prefix both the persistent shell and kernel-spawned processes with a trusted command
    /// and arguments, for example a sandbox launcher. The prefix is not interpreted as shell code.
    #[must_use]
    pub fn with_prefix(mut self, prefix: Vec<String>) -> Self {
        self.prefix = prefix;
        self
    }

    fn command(&self, program: &str) -> Command {
        let mut command = if let Some((executable, args)) = self.prefix.split_first() {
            let mut wrapper = Command::new(executable);
            wrapper.args(args).arg(program);
            wrapper
        } else {
            Command::new(program)
        };
        self.environment(&mut command);
        command
    }

    /// Set the environment variable `name` to `value` for the shell and for every process
    /// the kernel starts. The value is in the open: whatever runs there can print it.
    #[must_use]
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    /// Hand the program's own environment variable `name` through to the shell and to every
    /// process the kernel starts, if the program has it. Passing a credential this way puts
    /// its value within reach of whatever runs there.
    #[must_use]
    pub fn passing(mut self, name: impl Into<String>) -> Self {
        self.passed.push(name.into());
        self
    }

    /// Give `command` the environment a process of this kernel starts from: none of the
    /// program's own, then the [`PLAIN`] variables and those passed through, as the program
    /// has them, then the variables set for the kernel.
    fn environment(&self, command: &mut Command) {
        command.env_clear();
        let handed = PLAIN
            .iter()
            .copied()
            .chain(self.passed.iter().map(String::as_str));
        for name in handed {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.envs(self.env.iter().map(|(name, value)| (name, value)));
    }

    fn start(&self) -> Result<Session, KernelError> {
        let unavailable = |error: std::io::Error| KernelError::Unavailable(error.to_string());
        let mut command = self.command(&self.program);
        if self.dialect == Dialect::PowerShell {
            // Commands from standard input, one line at a time, with nothing asked of a person.
            // A terminal of no capabilities, or .NET on Unix writes the codes that set one up
            // into the output.
            command
                .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "-"])
                .env("TERM", "dumb");
        }
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
        let preamble: &[u8] = match self.dialect {
            // From here on the shell's own diagnostics arrive interleaved with cell output.
            Dialect::Posix => b"exec 2>&1\n",
            // Output as UTF-8, with no progress bars and (PowerShell 7) no colours in it.
            Dialect::PowerShell => {
                b"[Console]::OutputEncoding = New-Object Text.UTF8Encoding $false; \
                  $ProgressPreference = 'SilentlyContinue'; \
                  if ($PSStyle) { $PSStyle.OutputRendering = 'PlainText' }\n"
            }
        };
        stdin.write_all(preamble).map_err(unavailable)?;
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
        let script = match self.dialect {
            Dialect::Posix => posix_cell(code, &marker),
            Dialect::PowerShell => powershell_cell(code, &marker),
        };
        if session.stdin.write_all(script.as_bytes()).is_err() || session.stdin.flush().is_err() {
            return Err(KernelError::Exited {
                output: String::new(),
            });
        }
        let cell = session.collect(&marker, timeout, self.dialect)?;
        // Only a session that finished its cell cleanly is kept; any other is dropped, which
        // kills the shell, and the next cell starts a fresh one.
        self.session = Some(session);
        Ok(cell)
    }

    fn reset(&mut self) {
        self.session = None;
    }

    /// Start `spawn` in the kernel's directory, as its own process group, with its standard
    /// error discarded. It is found on the `PATH`, as the shell would find it, and its
    /// environment is the kernel's with the variables `spawn` names: nothing of the
    /// program's own beyond that.
    fn spawn(&mut self, spawn: &Spawn) -> Result<Box<dyn Process>, KernelError> {
        let mut command = self.command(&spawn.program);
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

/// The script that runs `code` in a POSIX shell and then prints `marker` and its status.
///
/// `command eval` keeps a syntax error from ending a non-interactive shell, and the quoted
/// here-document passes the cell through untouched. Cells read no input, so they cannot
/// swallow the commands queued behind them.
fn posix_cell(code: &str, marker: &str) -> String {
    format!(
        "command eval \"$(cat <<'{marker}'\n{code}\n{marker}\n)\" </dev/null\n\
         printf '\\n{marker} %s\\n' \"$?\"\n"
    )
}

/// The line that runs `code` in PowerShell and then prints `marker` and its status.
///
/// The cell travels as base64 on that one line, since PowerShell reading commands from its
/// input would otherwise run a multi-line cell piece by piece. It is parsed whole (a syntax
/// error is a terminating error like any other) and dot-sourced, so what it defines stays.
/// Every stream it writes to is merged into its output, in order, and the marker follows by
/// the same way out, so it cannot overtake what the cell wrote.
fn powershell_cell(code: &str, marker: &str) -> String {
    let encoded = BASE64.encode(code);
    format!(
        "$global:LASTEXITCODE = 0; $__corduroy_failed = $false; \
         try {{ . ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString(\
         [Convert]::FromBase64String('{encoded}')))) *>&1 | Out-String -Stream -Width 4096 }} \
         catch {{ $__corduroy_failed = $true; $_ | Out-String -Stream -Width 4096 }}; \
         $__corduroy_status = if ($__corduroy_failed) {{ 1 }} \
         elseif ($LASTEXITCODE) {{ $LASTEXITCODE }} else {{ 0 }}; \
         Write-Output (\"`n{marker} \" + $__corduroy_status)\n"
    )
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
    /// Read output until the marker line that ends the current cell. PowerShell's line
    /// endings (CRLF on Windows) are read as `\n`, as a POSIX shell's are.
    fn collect(
        &self,
        marker: &str,
        timeout: Duration,
        dialect: Dialect,
    ) -> Result<Cell, KernelError> {
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
            let line = match dialect {
                Dialect::PowerShell => powershell_line(&line),
                Dialect::Posix => line,
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

/// A line of PowerShell's output as the cell wrote it: `\n` for Windows' CRLF, and without
/// the codes that put a terminal's keypad in and out of application mode, which .NET writes
/// when it takes its output for a terminal.
fn powershell_line(line: &str) -> String {
    line.replace("\r\n", "\n")
        .replace("\u{1b}[?1h", "")
        .replace("\u{1b}[?1l", "")
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
const fn own_process_group(_command: &mut Command) {}

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

/// Stop whatever the shell left running: Windows has no process groups to signal, so this
/// asks `taskkill` to end the shell's whole tree of processes.
#[cfg(windows)]
fn kill_process_group(child: &Child) {
    let status = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // Failing to run `taskkill` leaves only the shell itself to be stopped by the caller.
    drop(status);
}

#[cfg(not(any(unix, windows)))]
fn kill_process_group(_child: &Child) {}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const PATIENT: Duration = Duration::from_secs(10);

    fn run(kernel: &mut ShellKernel, code: &str) -> Cell {
        kernel.exec(code, PATIENT).unwrap()
    }

    /// A variable the test program has and no kernel is given: cargo sets it for every test.
    const OURS: &str = "CARGO_MANIFEST_DIR";

    #[test]
    fn a_shell_starts_from_an_empty_environment_with_the_plain_variables() {
        assert!(std::env::var_os(OURS).is_some(), "cargo sets {OURS}");
        let mut kernel = ShellKernel::new();
        let shown = format!("printf '%s' \"${{{OURS}-unset}}\"");

        // Nothing of the program's own environment: a provider's key is not there to print.
        assert_eq!(run(&mut kernel, &shown).output, "unset");
        // Where programs are found is, or no command would run.
        assert_eq!(
            run(&mut kernel, "printf '%s' \"$PATH\"").output,
            std::env::var("PATH").unwrap()
        );
    }

    #[test]
    fn a_shell_is_given_only_the_variables_it_was_told_to() {
        let mut kernel = ShellKernel::new()
            .with_env("GREETING", "hello")
            .passing(OURS)
            .passing("CORDUROY_NOT_SET_ANYWHERE");
        let shown = format!(
            "printf '%s %s %s' \"$GREETING\" \"${{{OURS}-unset}}\" \
             \"${{CORDUROY_NOT_SET_ANYWHERE-unset}}\""
        );

        let ours = std::env::var(OURS).unwrap();
        assert_eq!(
            run(&mut kernel, &shown).output,
            format!("hello {ours} unset")
        );
    }

    #[test]
    fn a_process_the_kernel_starts_gets_the_same_environment_and_what_it_names() {
        let mut kernel = ShellKernel::new().with_env("GREETING", "hello");
        let spawn = Spawn {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                format!("echo \"$GREETING $OWN ${{{OURS}-unset}}\""),
            ],
            env: vec![("OWN".into(), "named".into())],
        };

        let mut process = kernel.spawn(&spawn).unwrap();
        let mut line = String::new();
        BufReader::new(process.stdout().unwrap())
            .read_line(&mut line)
            .unwrap();

        assert_eq!(line.trim(), "hello named unset");
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
    fn the_prefix_wraps_shell_cells_and_spawned_programs() {
        let mut kernel = ShellKernel::new().with_prefix(vec![
            "sh".into(),
            "-c".into(),
            "export PREFIXED=yes; exec \"$@\"".into(),
            "wrapper".into(),
        ]);
        let cell = kernel
            .exec("printf '%s' \"$PREFIXED\"", Duration::from_secs(5))
            .unwrap();
        assert_eq!(cell.output, "yes");

        let spawn = Spawn {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "echo \"$PREFIXED $1\"".into(),
                "sh".into(),
                "one argument".into(),
            ],
            ..Spawn::default()
        };
        let mut process = kernel.spawn(&spawn).unwrap();
        let mut line = String::new();
        BufReader::new(process.stdout().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "yes one argument\n");
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
    fn a_powershell_cell_is_one_line_whatever_it_holds() {
        let code = "foreach ($i in 1..2) {\n  \"it's $i\"\n}";
        let line = powershell_cell(code, "__mark__");

        assert_eq!(line.matches('\n').count(), 1, "{line}");
        assert!(line.ends_with('\n'));
        assert!(line.contains(&BASE64.encode(code)));
    }

    #[test]
    fn powershell_output_is_read_as_the_cell_wrote_it() {
        assert_eq!(powershell_line("\u{1b}[?1h\u{1b}[?1lready\r\n"), "ready\n");
        assert_eq!(powershell_line("plain\n"), "plain\n");
    }

    /// PowerShell, started, if it is on this machine. Its tests pass without it, unless
    /// `CORDUROY_REQUIRE_POWERSHELL` says it must be there (CI).
    fn powershell() -> Option<ShellKernel> {
        let mut kernel = ShellKernel::powershell();
        match kernel.exec("'ready'", Duration::from_secs(60)) {
            Ok(cell) => {
                assert_eq!(cell.output, "ready\n");
                Some(kernel)
            }
            Err(KernelError::Unavailable(_))
                if std::env::var_os("CORDUROY_REQUIRE_POWERSHELL").is_none() =>
            {
                None
            }
            Err(error) => panic!("PowerShell did not start: {error}"),
        }
    }

    #[test]
    fn powershell_keeps_state_between_cells() {
        let Some(mut kernel) = powershell() else {
            return;
        };

        let set = run(&mut kernel, "$total = 40; function Add2($n) { $n + 2 }");
        assert_eq!((set.output.as_str(), set.status), ("", 0));
        let cell = run(
            &mut kernel,
            "foreach ($i in 1..2) {\n  \"line $i\"\n}\nAdd2 $total",
        );

        assert_eq!(cell.output, "line 1\nline 2\n42\n");
        assert_eq!(cell.status, 0);
    }

    #[test]
    fn powershell_output_is_every_stream_in_order() {
        let Some(mut kernel) = powershell() else {
            return;
        };

        let cell = run(
            &mut kernel,
            "Write-Output 'one'; Write-Host 'two'; Write-Error 'three'; 'four'",
        );

        let lines: Vec<&str> = cell
            .output
            .lines()
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(lines.first(), Some(&"one"));
        assert_eq!(lines.get(1), Some(&"two"));
        assert!(lines.iter().any(|line| line.contains("three")), "{lines:?}");
        assert_eq!(lines.last(), Some(&"four"));
        // An error that does not stop the script is output, not a status.
        assert_eq!(cell.status, 0);
    }

    #[test]
    fn powershell_failures_and_syntax_errors_are_statuses_not_crashes() {
        let Some(mut kernel) = powershell() else {
            return;
        };
        run(&mut kernel, "$kept = 'yes'");

        let thrown = run(&mut kernel, "throw 'boom'");
        assert_eq!(thrown.status, 1);
        assert!(thrown.output.contains("boom"), "{}", thrown.output);
        let broken = run(&mut kernel, "if (");
        assert_eq!(broken.status, 1);
        let native = run(
            &mut kernel,
            "& (Get-Process -Id $PID).Path -NoProfile -Command 'exit 3'",
        );
        assert_eq!(native.status, 3);
        assert_eq!(run(&mut kernel, "$kept").output, "yes\n");
    }

    #[test]
    fn powershell_exits_and_timeouts_cost_the_kernel_its_state() {
        let Some(mut kernel) = powershell() else {
            return;
        };
        run(&mut kernel, "$lost = 'yes'");

        let error = kernel.exec("Start-Sleep -Seconds 30", Duration::from_secs(2));
        assert_eq!(error, Err(KernelError::TimedOut(Duration::from_secs(2))));
        assert_eq!(run(&mut kernel, "\"[$lost]\"").output, "[]\n");

        let exited = kernel.exec("'bye'; exit 3", PATIENT).unwrap_err();
        assert_eq!(
            exited,
            KernelError::Exited {
                output: "bye\n".into()
            }
        );
        assert_eq!(run(&mut kernel, "'back'").output, "back\n");
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
