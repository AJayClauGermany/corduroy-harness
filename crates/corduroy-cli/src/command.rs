//! The `corduroy` command line.
//!
//! ```text
//! corduroy [options]                 talk to the agent
//! corduroy [options] run <prompt>    one prompt, then exit
//! corduroy [options] continue        finish a run that was cut short or waits for an answer
//! corduroy [options] stop            end a run that was cut short or waits, without finishing it
//! corduroy [options] check           load the agent and say what it has, without a model
//! corduroy [options] sessions        list the sessions in the log
//! corduroy [options] log             print the event log
//! corduroy init                      write the default agent file to edit
//!
//! options: --agent <file>  --workspace <dir>  --session <name>  --model <name>  --provider <name>
//! ```
//!
//! The command is a library function, [`main_with`], so that a binary of your own can be the
//! same command with more [extension kinds](crate::extension) built in.

use std::error::Error;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use corduroy_harness::core::SessionId;
use corduroy_harness::{Harness, MAIN, Status};

use crate::agent::{AGENT_FILE, Agent, AgentFile, Places, assemble_with};
use crate::extension::Extensions;
use crate::terminal::{self, Out, say};

const USAGE: &str = "usage: corduroy [--agent FILE] [--workspace DIR] [--session NAME] \
                     [--model NAME] [--provider NAME] [run PROMPT | continue | stop | check | sessions | log | init]";

/// The command line, read.
#[derive(Debug, Default)]
struct Options {
    agent: Option<PathBuf>,
    workspace: Option<PathBuf>,
    session: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    command: Vec<String>,
}

fn options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--agent" => options.agent = Some(value("--agent")?.into()),
            "--workspace" => options.workspace = Some(value("--workspace")?.into()),
            "--session" => options.session = Some(value("--session")?),
            "--model" => options.model = Some(value("--model")?),
            "--provider" => options.provider = Some(value("--provider")?),
            "-h" | "--help" | "help" => return Err(USAGE.to_owned()),
            // `cargo corduroy -- check` passes one on; it means nothing here.
            "--" => {}
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}"));
            }
            _ => {
                options.command.push(arg);
                options.command.extend(args.by_ref());
            }
        }
    }
    Ok(options)
}

/// Run the `corduroy` command on this process's arguments, with the extension kinds in
/// `extensions` available to the agent file. Returns the process's exit code.
pub fn main_with(extensions: &Extensions) -> ExitCode {
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let result = options(std::env::args().skip(1))
        .map_err(Into::into)
        .and_then(|options| {
            tokio::runtime::Runtime::new()
                .map_err(Into::into)
                .and_then(|runtime| runtime.block_on(run(options, extensions, &out)))
        });
    match result {
        Ok(code) => code,
        Err(error) => {
            drop(writeln!(io::stderr(), "corduroy: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Write the default agent file into `workspace`.
fn init(workspace: &Path, out: &Out) -> Result<ExitCode, Box<dyn Error>> {
    let path = workspace.join(AGENT_FILE);
    if path.exists() {
        return Err(format!("{} already exists", path.display()).into());
    }
    std::fs::write(&path, AgentFile::default_text())?;
    let wrote = format!(
        "wrote {}: edit it to change what the agent can do",
        path.display()
    );
    say(out, &wrote);
    Ok(ExitCode::SUCCESS)
}

/// The agent the options describe, put together in `workspace`, and its agent file.
fn load(
    options: &Options,
    workspace: PathBuf,
    extensions: &Extensions,
    out: &Out,
) -> Result<Agent, Box<dyn Error>> {
    let file_path = options
        .agent
        .clone()
        .or_else(|| Some(workspace.join(AGENT_FILE)).filter(|path| path.is_file()));
    let mut file = match &file_path {
        Some(path) => AgentFile::read(path)?,
        None => AgentFile::parse(&AgentFile::default_text())?,
    };
    if let Some(model) = &options.model {
        file.model.clone_from(model);
    }
    if let Some(provider) = &options.provider {
        file.provider.clone_from(provider);
    }
    let file_path = file_path.map(|path| path.canonicalize()).transpose()?;
    let agent = assemble_with(&file, &Places::new(workspace, file_path), extensions)?;
    for unstarted in &agent.failed {
        let line = format!(
            "! plugin {} did not start: {}",
            unstarted.plugin, unstarted.reason
        );
        say(out, &line);
    }
    Ok(agent)
}

async fn run(
    options: Options,
    extensions: &Extensions,
    out: &Out,
) -> Result<ExitCode, Box<dyn Error>> {
    let workspace = match &options.workspace {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    }
    .canonicalize()?;
    let command: Vec<&str> = options.command.iter().map(String::as_str).collect();
    if command.first() == Some(&"init") {
        return init(&workspace, out);
    }
    let agent = load(&options, workspace, extensions, out)?;
    let harness = &agent.harness;
    let session = SessionId::new(options.session.as_deref().unwrap_or(MAIN));
    if !harness.session_list().iter().any(|info| info.id == session) {
        harness.start_session(session.clone(), None)?;
    }
    let mut input = io::BufReader::new(io::stdin());
    match command.as_slice() {
        [] => chat(harness, session, &mut input, out).await,
        ["run", words @ ..] if !words.is_empty() => {
            let _shown = terminal::watch(harness, &session, out);
            let prompt = words.join(" ");
            let outcome = terminal::prompt(harness, &session, &prompt, &mut input, out).await?;
            Ok(finish(&outcome.status, out))
        }
        ["continue"] => {
            let _shown = terminal::watch(harness, &session, out);
            let outcome = terminal::resume(harness, &session, &mut input, out).await?;
            Ok(finish(&outcome.status, out))
        }
        ["stop"] => {
            let stopped = harness.stop(&session)?;
            let said = if stopped {
                format!("stopped the run of `{session}`")
            } else {
                format!("`{session}` has no run to stop")
            };
            say(out, &said);
            Ok(ExitCode::SUCCESS)
        }
        ["check"] => Ok(check(&agent, out)),
        ["sessions"] => {
            for info in harness.session_list() {
                let parent = info.parent.map(|parent| format!(" (from {parent})"));
                say(out, &format!("{}{}", info.id, parent.unwrap_or_default()));
            }
            Ok(ExitCode::SUCCESS)
        }
        ["log"] => {
            for event in harness.events().events() {
                say(out, &format!("{} {event}", stamp(event.time)));
            }
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(USAGE.into()),
    }
}

/// `time`, in milliseconds since the Unix epoch, as `2026-11-22T17:31:05Z`.
fn stamp(time: u64) -> String {
    let seconds = time / 1000;
    let (days, of_day) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970-01-01 to a date in the proleptic Gregorian calendar, by counting
    // from 0000-03-01 so that a leap day is the last day of its year.
    let shifted = days + 719_468;
    let (era, day_of_era) = (shifted / 146_097, shifted % 146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

fn finish(status: &Status, out: &Out) -> ExitCode {
    if let Some(line) = terminal::ending(status) {
        say(out, &line);
    }
    match status {
        Status::Finished { .. } | Status::Yielded => ExitCode::SUCCESS,
        // As a program interrupted by Ctrl-C does.
        Status::Stopped => ExitCode::from(130),
        _ => ExitCode::FAILURE,
    }
}

/// Say what the agent has: its model, tools, gates and plugins.
fn check(agent: &Agent, out: &Out) -> ExitCode {
    let harness = &agent.harness;
    let file = &agent.file;
    let lines = [
        format!("model     {} from {}", file.model, file.provider),
        format!("log       {}", agent.log.display()),
        format!("providers {}", harness.providers().names().join(", ")),
        format!("kernels   {}", harness.kernels().names().join(", ")),
        format!("gates     {}", harness.gates().names().join(", ")),
        format!("points    {}", harness.gates().points().join(", ")),
    ];
    for line in lines {
        say(out, &line);
    }
    // What extensions added to the loop, if any did.
    let steps = harness.operations().names();
    if !steps.is_empty() {
        say(out, &format!("steps     {}", steps.join(", ")));
    }
    for spec in harness.tools().specs() {
        let flags = if spec.read_only { " (read-only)" } else { "" };
        say(out, &format!("tool      {}{flags}", spec.name));
    }
    for unstarted in &agent.failed {
        say(
            out,
            &format!("failed    {}: {}", unstarted.plugin, unstarted.reason),
        );
    }
    if agent.failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

const HELP: &str = "/sessions  list sessions\n/session NAME  switch to a session, starting it \
                    if new\n/tools  list the tools\n/exit  leave";

/// What a line typed at the prompt asks for.
enum Typed<'a> {
    Nothing,
    Exit,
    Switch(&'a str),
    Prompt(&'a str),
}

/// Carry out a `/command` that needs no model, or say what the line asks for.
fn typed<'a>(line: &'a str, harness: &Harness, session: &SessionId, out: &Out) -> Typed<'a> {
    let (command, rest) = line
        .split_once(' ')
        .map_or((line, ""), |(command, rest)| (command, rest.trim()));
    match command {
        "" => Typed::Nothing,
        "/exit" | "/quit" => Typed::Exit,
        "/session" if !rest.is_empty() => Typed::Switch(rest),
        "/help" => {
            say(out, HELP);
            Typed::Nothing
        }
        "/sessions" => {
            let lines: Vec<String> = harness
                .session_list()
                .into_iter()
                .map(|info| {
                    let marker = if info.id == *session { "*" } else { " " };
                    format!("{marker} {}", info.id)
                })
                .collect();
            say(out, &lines.join("\n"));
            Typed::Nothing
        }
        "/tools" => {
            let lines: Vec<String> = harness
                .tools()
                .specs()
                .into_iter()
                .map(|spec| {
                    let summary = spec
                        .description
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    format!("{}: {summary}", spec.name)
                })
                .collect();
            say(out, &lines.join("\n"));
            Typed::Nothing
        }
        command if command.starts_with('/') => {
            say(out, &format!("unknown command {command}; /help lists them"));
            Typed::Nothing
        }
        _ => Typed::Prompt(line),
    }
}

/// Talk to the agent until the input ends or `/exit`.
async fn chat(
    harness: &Harness,
    mut session: SessionId,
    input: &mut (dyn BufRead + Send),
    out: &Out,
) -> Result<ExitCode, Box<dyn Error>> {
    say(
        out,
        "corduroy: type a prompt, /help for commands, /exit or ctrl-d to leave",
    );
    let mut shown = terminal::watch(harness, &session, out);
    let outcome = terminal::resume(harness, &session, input, out).await?;
    let started = !outcome.session.conversation().is_empty();
    if let Some(line) = terminal::ending(&outcome.status).filter(|_| started) {
        say(out, &line);
    }
    loop {
        say_prompt(out);
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(ExitCode::SUCCESS);
        }
        match typed(line.trim(), harness, &session, out) {
            Typed::Nothing => {}
            Typed::Exit => return Ok(ExitCode::SUCCESS),
            Typed::Switch(name) => {
                let next = SessionId::new(name);
                if !harness.session_list().iter().any(|info| info.id == next) {
                    harness.start_session(next.clone(), Some(&session))?;
                }
                harness.events().unsubscribe(shown);
                session = next;
                shown = terminal::watch(harness, &session, out);
            }
            Typed::Prompt(prompt) => {
                let outcome = terminal::prompt(harness, &session, prompt, input, out).await?;
                if let Some(line) = terminal::ending(&outcome.status) {
                    say(out, &line);
                }
            }
        }
    }
}

fn say_prompt(out: &Out) {
    let mut out = out
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    drop(write!(out, "› "));
    drop(out.flush());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_shown_as_utc_dates() {
        assert_eq!(stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(stamp(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(stamp(1_795_368_645_804), "2026-11-22T17:30:45Z");
        assert_eq!(stamp(4_102_444_799_999), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn a_stray_double_dash_is_ignored() {
        let args = ["--", "--session", "side", "run", "say", "--", "hi"];
        let read = options(args.into_iter().map(str::to_owned)).unwrap();
        assert_eq!(read.session.as_deref(), Some("side"));
        assert_eq!(read.command, ["run", "say", "--", "hi"]);
    }
}
