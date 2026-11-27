//! `bash`, in place of `pi-tools`' own: the same tool, with each command put through a
//! program that shortens what it prints before a model reads it. The program is `rtk` unless
//! the configuration names another.
//!
//! It is a drop-in replacement, and is here partly to show one. The tool's name, its input
//! and the shape of its result are the `shell-tool` crate's, which `pi-tools` builds on as
//! well, so the model, the gates and the log cannot tell which add-on answered: only what a
//! command printed differs. A name belongs to one plugin, so an agent that loads this one
//! configures `pi-tools` without `bash` (`tools = ["read", "write", "edit", "tree"]`).
//!
//! # How a command is run
//!
//! `rtk` shortens the output of the commands it knows (`rtk git status`), and a model writes
//! more than single commands: `cd app && cargo test`, a pipe, several lines. Putting the
//! program in front of all that would hand it `cd`, which then happens in a child process
//! and is lost. So by default the script is given to `rtk rewrite`, which answers with the
//! same script with its own name put where it helps (and leaves alone what it does not
//! know), and the answer is run in the shell itself, where `cd` and variables carry over as
//! they do for `pi-tools`. A script `rewrite` has nothing for runs as it was written.
//!
//! `run` in the configuration is that recipe, as a template, so that another program, or
//! another version of this one, is a change to an agent file and not a rebuild. It may hold
//! `{prefix}` (the program), `{quoted}` (the model's script as one shell word) and
//! `{command}` (the script as written). `"{prefix} {command}"` is the plain form: the
//! program in front of whatever was written.
//!
//! The program is looked for on the kernel's `PATH`, as the shell finds any command; a
//! kernel starts from the plain variables and this add-on adds none. Where it is not
//! installed the call fails and says so, rather than run the command another way.
//!
//! The template is for a POSIX shell.

use std::sync::OnceLock;

use corduroy_guest::{Plugin, PluginInfo, ToolError, parse};
use serde::Deserialize;
use shell_tool::{Call, Limits, quoted};

/// The program commands go through unless the configuration names another.
const PREFIX: &str = "rtk";

/// How a script is run unless the configuration says: asked of `{prefix} rewrite`, whose
/// answer (status 0, or 3 where its own rules would ask a person, which is the gates' to do
/// here) is run in this shell; with any other status the script runs as written.
const RUN: &str = "__lean=$({prefix} rewrite {quoted} 2>/dev/null); \
                   case $? in 0|3) eval \"$__lean\" ;; *) eval {quoted} ;; esac";

/// The most one result should take of the model's context, in tokens of three bytes, unless
/// the configuration says: a quarter of what `pi-tools` asks for its own.
const RESULT_TOKENS: u32 = 4096;

/// Lines kept from a command's output.
const MAX_LINES: usize = 2000;

/// What the script prints, with [`ABSENT_STATUS`], when the program is not installed.
const ABSENT: &str = "lean-shell: the program is not on the PATH";
/// The status a shell gives for a command it cannot find.
const ABSENT_STATUS: i32 = 127;

/// Plugin configuration, from the host.
#[derive(Deserialize)]
#[serde(default)]
struct Config {
    /// Name of the host kernel `bash` runs in.
    kernel: String,
    /// The program each command goes through, with any arguments it always takes.
    prefix: String,
    /// How a script is run: a template holding `{prefix}`, `{quoted}` or `{command}`.
    run: String,
    /// The most one result should take of the model's context, in tokens.
    result_tokens: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            kernel: "shell".to_owned(),
            prefix: PREFIX.to_owned(),
            run: RUN.to_owned(),
            result_tokens: RESULT_TOKENS,
        }
    }
}

impl Config {
    /// What is wrong with the configuration, if anything.
    fn fault(&self) -> Option<String> {
        if self.program().is_none() {
            return Some("`prefix` names no program".to_owned());
        }
        if !["{quoted}", "{command}"]
            .iter()
            .any(|part| self.run.contains(part))
        {
            return Some(
                "`run` holds neither `{quoted}` nor `{command}`, so it would run no command"
                    .to_owned(),
            );
        }
        (self.result_tokens == 0).then(|| "`result_tokens` is 0".to_owned())
    }

    /// The program itself: the first word of `prefix`.
    fn program(&self) -> Option<&str> {
        self.prefix.split_whitespace().next()
    }

    /// The script the kernel is given for the model's `command`: the template filled in,
    /// run only where the program is installed. Otherwise it prints [`ABSENT`] and ends
    /// with [`ABSENT_STATUS`], which [`absent`] reads back.
    fn script(&self, command: &str) -> String {
        format!(
            "if command -v {program} >/dev/null 2>&1; then\n{run}\nelse\nprintf '%s\\n' \
             {absent}; (exit {ABSENT_STATUS})\nfi",
            program = quoted(self.program().unwrap_or(PREFIX)),
            run = fill(&self.run, &self.prefix, command),
            absent = quoted(ABSENT),
        )
    }

    /// How much of a command's output this add-on keeps: what its declared result comes to
    /// in bytes.
    fn limits(&self) -> Limits {
        let bytes = usize::try_from(self.result_tokens)
            .unwrap_or(usize::MAX)
            .saturating_mul(3);
        Limits {
            lines: MAX_LINES,
            bytes,
        }
    }
}

/// `template` with `{prefix}`, `{quoted}` and `{command}` filled in, in one pass: a brace
/// in the model's command is text, not a place to fill.
fn fill(template: &str, prefix: &str, command: &str) -> String {
    let quoted = quoted(command);
    let parts = [
        ("{prefix}", prefix),
        ("{quoted}", quoted.as_str()),
        ("{command}", command),
    ];
    let mut filled = String::with_capacity(template.len() + command.len());
    let mut rest = template;
    while let Some(at) = rest.find('{') {
        let (before, from) = rest.split_at(at);
        filled.push_str(before);
        if let Some((name, value)) = parts.iter().find(|(name, _)| from.starts_with(name)) {
            filled.push_str(value);
            rest = from.strip_prefix(name).unwrap_or_default();
        } else {
            filled.push('{');
            rest = from.strip_prefix('{').unwrap_or_default();
        }
    }
    filled.push_str(rest);
    filled
}

/// Whether a result is the script saying the program is not installed.
fn absent(result: &str) -> bool {
    result
        .strip_prefix(ABSENT)
        .is_some_and(|rest| rest.trim() == format!("[exit status {ABSENT_STATUS}]"))
}

static CONFIG: OnceLock<Config> = OnceLock::new();

struct LeanShell;

impl Plugin for LeanShell {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        if let Some(fault) = config.fault() {
            return Err(format!("bad config: {fault}"));
        }
        // A second activation in the same instance keeps the first configuration.
        let config = CONFIG.get_or_init(|| config);
        let description = format!(
            "Run a shell command. The shell is persistent: the working directory and \
             variables carry over between calls. Output is shortened by `{}` where it knows \
             the command; returns the last {MAX_LINES} lines or {}KB of it, and binary \
             output is named by its kind and size.",
            config.program().unwrap_or(PREFIX),
            config.limits().bytes / 1024,
        );
        Ok(PluginInfo::new("lean-shell", env!("CARGO_PKG_VERSION"))
            .tool(shell_tool::spec(&description, config.result_tokens)))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        if name != shell_tool::NAME {
            return Err(ToolError::UnknownTool(name));
        }
        let call: Call = parse(&input)?;
        let fallback = Config::default();
        let config = CONFIG.get().unwrap_or(&fallback);
        let result = shell_tool::run(
            &config.kernel,
            &config.script(&call.command),
            call.timeout,
            config.limits(),
        )?;
        if absent(&result) {
            return Err(ToolError::Failed(format!(
                "`{}` is not installed where the shell finds commands (its PATH), so the \
                 command was not run. This tool puts every command through it; it is set as \
                 `prefix` in this add-on's configuration.",
                config.program().unwrap_or(PREFIX)
            )));
        }
        Ok(result)
    }
}

corduroy_guest::export!(LeanShell);

#[cfg(test)]
mod tests {
    use super::*;

    fn config(prefix: &str, run: &str) -> Config {
        Config {
            prefix: prefix.to_owned(),
            run: run.to_owned(),
            ..Config::default()
        }
    }

    #[test]
    fn a_template_is_filled_once_and_a_brace_in_the_command_is_text() {
        assert_eq!(
            fill("{prefix} {command}", "rtk", "git status"),
            "rtk git status"
        );
        assert_eq!(
            fill("{prefix} sh -c {quoted}", "env", "echo 'a b'"),
            r"env sh -c 'echo '\''a b'\'''"
        );
        // The command names a place itself: it is not filled a second time.
        assert_eq!(
            fill("{prefix} {command}", "rtk", "echo {prefix} ${HOME}"),
            "rtk echo {prefix} ${HOME}"
        );
        assert_eq!(fill("{other} {command}", "rtk", "ls"), "{other} ls");
    }

    #[test]
    fn by_default_a_script_is_rewritten_and_run_in_the_shell_itself() {
        let script = Config::default().script("cd app && cargo test");
        assert_eq!(
            script,
            "if command -v 'rtk' >/dev/null 2>&1; then\n\
             __lean=$(rtk rewrite 'cd app && cargo test' 2>/dev/null); \
             case $? in 0|3) eval \"$__lean\" ;; *) eval 'cd app && cargo test' ;; esac\n\
             else\nprintf '%s\\n' 'lean-shell: the program is not on the PATH'; (exit 127)\nfi"
        );
    }

    #[test]
    fn the_program_is_the_first_word_of_the_prefix() {
        let config = config("rtk bypass", "{prefix} {command}");
        assert_eq!(config.program(), Some("rtk"));
        assert!(config.script("ls").contains("\nrtk bypass ls\n"));
    }

    #[test]
    fn a_configuration_that_would_run_nothing_is_refused() {
        assert_eq!(Config::default().fault(), None);
        assert_eq!(
            config(" ", RUN).fault().unwrap(),
            "`prefix` names no program"
        );
        assert!(
            config("rtk", "{prefix} --version")
                .fault()
                .unwrap()
                .starts_with("`run` holds neither")
        );
        let none = Config {
            result_tokens: 0,
            ..Config::default()
        };
        assert_eq!(none.fault().unwrap(), "`result_tokens` is 0");
    }

    #[test]
    fn only_the_scripts_own_line_says_the_program_is_absent() {
        let limits = Config::default().limits();
        let said = shell_tool::shown(&format!("{ABSENT}\n"), ABSENT_STATUS, false, limits);
        assert!(absent(&said));
        // A command of the model's that was not found is a result like any other.
        let other = shell_tool::shown("sh: nope: not found\n", ABSENT_STATUS, false, limits);
        assert!(!absent(&other));
        assert!(!absent("ok\n"));
    }

    #[test]
    fn what_is_kept_is_what_the_declared_result_comes_to() {
        assert_eq!(
            Config::default().limits(),
            Limits {
                lines: 2000,
                bytes: 12_288
            }
        );
    }
}
