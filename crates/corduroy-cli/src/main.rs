//! `corduroy`: run an agent in a terminal.
//!
//! ```text
//! corduroy [options]                 talk to the agent
//! corduroy [options] run <prompt>    one prompt, then exit
//! corduroy [options] continue        finish a run that was cut short or waits for an answer
//! corduroy [options] check           load the agent and say what it has, without a model
//! corduroy [options] sessions        list the sessions in the log
//! corduroy [options] log             print the event log
//! corduroy init                      write the default agent file to edit
//!
//! options: --agent <file>  --workspace <dir>  --session <name>  --model <name>  --provider <name>
//! ```

use std::error::Error;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use corduroy_cli::agent::{AGENT_FILE, Agent, AgentFile, Places, assemble};
use corduroy_cli::terminal::{self, Out, say};
use corduroy_harness::core::SessionId;
use corduroy_harness::{Harness, MAIN, Status};

const USAGE: &str = "usage: corduroy [--agent FILE] [--workspace DIR] [--session NAME] \
                     [--model NAME] [--provider NAME] [run PROMPT | continue | check | sessions | log | init]";

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

fn main() -> ExitCode {
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let result = options(std::env::args().skip(1))
        .map_err(Into::into)
        .and_then(|options| {
            tokio::runtime::Runtime::new()
                .map_err(Into::into)
                .and_then(|runtime| runtime.block_on(run(options, &out)))
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
fn load(options: &Options, workspace: PathBuf, out: &Out) -> Result<Agent, Box<dyn Error>> {
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
    let agent = assemble(&file, &Places::new(workspace, file_path))?;
    for unstarted in &agent.failed {
        let line = format!(
            "! plugin {} did not start: {}",
            unstarted.plugin, unstarted.reason
        );
        say(out, &line);
    }
    Ok(agent)
}

async fn run(options: Options, out: &Out) -> Result<ExitCode, Box<dyn Error>> {
    let workspace = match &options.workspace {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    }
    .canonicalize()?;
    let command: Vec<&str> = options.command.iter().map(String::as_str).collect();
    if command.first() == Some(&"init") {
        return init(&workspace, out);
    }
    let agent = load(&options, workspace, out)?;
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
                say(out, &event.to_string());
            }
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(USAGE.into()),
    }
}

fn finish(status: &Status, out: &Out) -> ExitCode {
    if let Some(line) = terminal::ending(status) {
        say(out, &line);
    }
    match status {
        Status::Finished { .. } | Status::Yielded => ExitCode::SUCCESS,
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
    ];
    for line in lines {
        say(out, &line);
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
    let started = !outcome.session.conversation().messages().is_empty();
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
