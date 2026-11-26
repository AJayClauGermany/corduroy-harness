//! The `corduroy` command line.
//!
//! ```text
//! corduroy [options]                 talk to the agent
//! corduroy [options] run <prompt>    one prompt, then exit
//! corduroy [options] continue        finish a run that was cut short or waits for an answer
//! corduroy [options] stop            end a run that was cut short or waits, without finishing it
//! corduroy [options] check           load the agent and say what it has, without a model
//! corduroy [options] sessions        list the sessions in the log
//! corduroy [options] sessions remove <name>
//!                                    take a session, and those it started, out of the log
//! corduroy [options] log             print the event log, or with --session one session's part
//! corduroy [options] report [N]      what the session's last run (or last N) did, with timings
//! corduroy init                      write the default agent file to edit
//!
//! options: --agent <file>  --workspace <dir>  --session <name>  --model <name>  --provider <name>
//! ```
//!
//! The command is a library function, [`main_standard`], so that a binary of your own can be
//! the same command with other [standard extensions](corduroy_std::Standard): more
//! [extension kinds](crate::extension) ([`main_with`]), another kernel, another log.

use std::error::Error;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use corduroy_harness::core::{Event, Query, SessionId};
use corduroy_harness::{Harness, MAIN, Status};

use corduroy_std::Standard;

use crate::agent::{AGENT_FILE, Agent, AgentFile, Places, assemble_from};
use crate::extension::Extensions;
use crate::report::{below, report, stamp};
use crate::terminal::{self, Out, say};

const USAGE: &str = "usage: corduroy [--agent FILE] [--workspace DIR] [--session NAME] \
                     [--model NAME] [--provider NAME] [--version] [run PROMPT | continue | stop | check | sessions | \
                     sessions remove NAME | log | report [N] | init]";

/// `corduroy` and its version, with the commit it was built from when the build said
/// (`CORDUROY_COMMIT`, set by the release workflow).
fn version() -> String {
    let release = env!("CARGO_PKG_VERSION");
    option_env!("CORDUROY_COMMIT").map_or_else(
        || format!("corduroy {release}"),
        |commit| format!("corduroy {release} ({commit})"),
    )
}

/// The command line, read.
#[derive(Debug, Default)]
struct Options {
    agent: Option<PathBuf>,
    workspace: Option<PathBuf>,
    session: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    version: bool,
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
            "-V" | "--version" => options.version = true,
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

/// Run the `corduroy` command on this process's arguments, with the standard extensions and
/// the extension kinds in `extensions` available to the agent file. Returns the process's exit
/// code.
pub fn main_with(extensions: &Extensions) -> ExitCode {
    main_standard(&Standard::new().with_kinds(extensions))
}

/// Run the `corduroy` command on this process's arguments, putting agents together from the
/// parts in `standard`. Returns the process's exit code.
pub fn main_standard(standard: &Standard) -> ExitCode {
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let parsed = options(std::env::args().skip(1));
    if let Ok(Options { version: true, .. }) = parsed {
        say(&out, &version());
        return ExitCode::SUCCESS;
    }
    let result = parsed.map_err(Into::into).and_then(|options| {
        tokio::runtime::Runtime::new()
            .map_err(Into::into)
            .and_then(|runtime| runtime.block_on(run(options, standard, &out)))
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

/// An agent file and the places it is read against.
struct Described {
    file: AgentFile,
    places: Places,
}

/// The agent file the options name, or the default one, and the places it is read against.
fn agent_file(options: &Options, workspace: PathBuf) -> Result<Described, Box<dyn Error>> {
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
    let places = Places::new(workspace, file_path);
    Ok(Described { file, places })
}

/// The agent the options describe, put together in `workspace`, and its agent file.
fn load(
    options: &Options,
    workspace: PathBuf,
    standard: &Standard,
    out: &Out,
) -> Result<Agent, Box<dyn Error>> {
    let Described { file, places } = agent_file(options, workspace)?;
    let agent = assemble_from(&file, &places, standard)?;
    for unstarted in &agent.failed {
        let line = format!(
            "! plugin {} did not start: {}",
            unstarted.plugin, unstarted.reason
        );
        say(out, &line);
    }
    Ok(agent)
}

async fn run(options: Options, standard: &Standard, out: &Out) -> Result<ExitCode, Box<dyn Error>> {
    let workspace = match &options.workspace {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    }
    .canonicalize()?;
    let command: Vec<&str> = options.command.iter().map(String::as_str).collect();
    if command.first() == Some(&"init") {
        return init(&workspace, out);
    }
    if let ["sessions", "remove", name] = command.as_slice() {
        // Maintenance between runs: the agent is not put together, so nothing else is written.
        let Described { file, places } = agent_file(&options, workspace)?;
        let log = file.log_path(&places);
        return remove(&log, &SessionId::new(*name), standard, out);
    }
    let agent = load(&options, workspace, standard, out)?;
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
        // Everything, unless a session was named: then that session and those it started.
        ["log"] => print_log(harness, options.session.as_ref().map(|_| &session), out),
        ["report"] => print_report(&agent, &session, "1", out),
        ["report", count] => print_report(&agent, &session, count, out),
        _ => Err(USAGE.into()),
    }
}

/// Every event in the log, read a page at a time, so that what the stream does not hold in
/// memory is read from the store.
fn all_events(harness: &Harness) -> Result<Vec<Event>, Box<dyn Error>> {
    const PAGE: usize = 1000;
    let mut events = Vec::new();
    let mut query = Query::first(PAGE);
    loop {
        let page = harness.events().read(&query)?;
        let last = page.last().map(|last| last.id);
        let full = page.len() == PAGE;
        events.extend(page);
        match last {
            Some(last) if full => query = query.after(last),
            _ => return Ok(events),
        }
    }
}

/// Print the events in the log, one per line with its time: those of `only` and the sessions
/// it started, or all of them.
fn print_log(
    harness: &Harness,
    only: Option<&SessionId>,
    out: &Out,
) -> Result<ExitCode, Box<dyn Error>> {
    for event in all_events(harness)? {
        if only.is_none_or(|session| below(&event, session)) {
            say(out, &format!("{} {event}", stamp(event.time)));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Print what there is to hand over about the last `count` runs of `session`: the build, the
/// agent as `check` describes it, then the runs with their timings.
fn print_report(
    agent: &Agent,
    session: &SessionId,
    count: &str,
    out: &Out,
) -> Result<ExitCode, Box<dyn Error>> {
    let count = count
        .parse()
        .map_err(|_| format!("`{count}` is not a number of runs\n{USAGE}"))?;
    say(out, &version());
    for line in described(agent) {
        say(out, &line);
    }
    say(out, &report(&all_events(&agent.harness)?, session, count));
    Ok(ExitCode::SUCCESS)
}

/// Take `session`, and every session started from it, out of the log at `log`.
fn remove(
    log: &Path,
    session: &SessionId,
    standard: &Standard,
    out: &Out,
) -> Result<ExitCode, Box<dyn Error>> {
    if !log.is_file() {
        return Err(format!("there is no log at {}", log.display()).into());
    }
    let removed = standard.remove_session(log, session)?;
    let started: Vec<String> = removed
        .iter()
        .filter(|id| *id != session)
        .map(|id| format!("`{id}`"))
        .collect();
    let said = if started.is_empty() {
        format!("removed `{session}` from the log")
    } else {
        format!(
            "removed `{session}` from the log, with the sessions it started: {}",
            started.join(", ")
        )
    };
    say(out, &said);
    Ok(ExitCode::SUCCESS)
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

/// What the agent has, a line each: its model, log, registries, tools and the plugins that
/// did not start.
fn described(agent: &Agent) -> Vec<String> {
    let harness = &agent.harness;
    let file = &agent.file;
    let mut lines = vec![
        format!("model     {} from {}", file.model, file.provider),
        format!("log       {}", agent.log.display()),
        format!("providers {}", harness.providers().names().join(", ")),
        format!("kernels   {}", harness.kernels().names().join(", ")),
        format!("gates     {}", harness.gates().names().join(", ")),
        format!("points    {}", harness.gates().points().join(", ")),
    ];
    // What extensions added to the loop, if any did.
    let steps = harness.operations().names();
    if !steps.is_empty() {
        lines.push(format!("steps     {}", steps.join(", ")));
    }
    for spec in harness.tools().specs() {
        let flags = if spec.read_only { " (read-only)" } else { "" };
        lines.push(format!("tool      {}{flags}", spec.name));
    }
    for unstarted in &agent.failed {
        lines.push(format!(
            "failed    {}: {}",
            unstarted.plugin, unstarted.reason
        ));
    }
    lines
}

/// Say what the agent has: its model, tools, gates and plugins.
fn check(agent: &Agent, out: &Out) -> ExitCode {
    for line in described(agent) {
        say(out, &line);
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
    fn a_stray_double_dash_is_ignored() {
        let args = ["--", "--session", "side", "run", "say", "--", "hi"];
        let read = options(args.into_iter().map(str::to_owned)).unwrap();
        assert_eq!(read.session.as_deref(), Some("side"));
        assert_eq!(read.command, ["run", "say", "--", "hi"]);
    }

    #[test]
    fn the_version_names_the_release() {
        let read = options(std::iter::once("--version".to_owned())).unwrap();
        assert!(read.version);
        assert!(version().starts_with(&format!("corduroy {}", env!("CARGO_PKG_VERSION"))));
    }
}
