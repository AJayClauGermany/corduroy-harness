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
//! corduroy [options] doctor          try the agent for real: its secrets, kernels and models
//! corduroy init [vendor] [--secrets keychain|env]
//!                                    write the default agent file to edit
//! corduroy [options] secrets         list the agent's secrets and how each stands
//! corduroy [options] secrets set [name]
//!                                    put a secret's value in the keychain, typed without showing
//! corduroy [options] secrets remove [name]
//!                                    take it out again
//! corduroy [options] login [name]    sign in for a secret of the agent that needs it (a ChatGPT plan)
//! corduroy [options] logout [name]   sign out again: revoke and forget what was kept
//!
//! options: --agent <file>  --workspace <dir>  --session <name>  --model <name>  --provider <name>
//! ```
//!
//! The command is a library function, [`main_standard`], so that a binary of your own can be
//! the same command with other [standard extensions](corduroy_std::Standard): more
//! [extension kinds](crate::extension) ([`main_with`]), another kernel, another log, another
//! keychain. [`run_on`] is the same with the arguments, what is typed and where it writes
//! handed over, which is how the tests drive it.

mod doctor;
mod init;
mod secrets;

use std::error::Error;
use std::io::{self, BufRead, IsTerminal as _, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use corduroy_harness::core::{Event, Query, SessionId};
use corduroy_harness::{Harness, MAIN, Status};

use corduroy_std::{SignIn, Standard};

use crate::agent::{AGENT_FILE, Agent, AgentFile, Places, assemble_from};
use crate::extension::Extensions;
use crate::report::{below, report, stamp};
use crate::terminal::{self, Lines, Out, say};

const USAGE: &str = "usage: corduroy [--agent FILE] [--workspace DIR] [--session NAME] \
                     [--model NAME] [--provider NAME] [--version] [run PROMPT | continue | stop | check | sessions | \
                     sessions remove NAME | log | report [N] | doctor | \
                     init [anthropic|openai|chatgpt|ollama] [--secrets keychain|env] | \
                     secrets | secrets set [SECRET] | secrets remove [SECRET] | \
                     login [SECRET] | logout [SECRET]]";

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
    let console = Console {
        input: Box::new(io::BufReader::new(io::stdin())),
        out: Arc::new(Mutex::new(io::stdout())),
        hidden: io::stdin().is_terminal(),
    };
    match run_on(standard, std::env::args().skip(1), console) {
        Ok(code) => code,
        Err(error) => {
            drop(writeln!(io::stderr(), "corduroy: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Where the command reads what a person types and writes what it says.
pub struct Console {
    /// What the person types: prompts, answers to a gate, and a secret's value.
    pub input: Box<dyn BufRead + Send>,
    /// Where the command writes.
    pub out: Out,
    /// Whether a person is typing at a terminal, so that a secret's value is read from it
    /// without being shown. Otherwise the value is a line of `input`.
    pub hidden: bool,
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console")
            .field("hidden", &self.hidden)
            .finish_non_exhaustive()
    }
}

impl Console {
    /// A value the person types after `prompt`, not shown as it is typed at a terminal. The
    /// spaces and the line end around it are dropped.
    fn secret(&mut self, prompt: &str) -> Result<String, Box<dyn Error>> {
        let typed = if self.hidden {
            rpassword::prompt_password(prompt)?
        } else {
            let mut line = String::new();
            self.input.read_line(&mut line)?;
            line
        };
        let value = typed.trim();
        if value.is_empty() {
            return Err("no value was given, so nothing was changed".into());
        }
        Ok(value.to_owned())
    }
}

/// Run the `corduroy` command on `args` (without the program's name), putting agents
/// together from the parts in `standard`, reading and writing through `console`.
///
/// # Errors
///
/// What went wrong, as the command would say it after `corduroy: `. A run that failed is no
/// error: it is the exit code.
pub fn run_on(
    standard: &Standard,
    args: impl Iterator<Item = String>,
    mut console: Console,
) -> Result<ExitCode, String> {
    let options = options(args)?;
    if options.version {
        say(&console.out, &version());
        return Ok(ExitCode::SUCCESS);
    }
    let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
    runtime
        .block_on(run(options, standard, &mut console))
        .map_err(|error| error.to_string())
}

/// An agent file and the places it is read against.
struct Described {
    file: AgentFile,
    places: Places,
}

/// One `[secrets.<name>]` entry as the function that builds, keeps or signs in to its kind is
/// handed it.
struct SecretAsked {
    /// The extension kind the entry is built from.
    kind: String,
    /// The entry's other keys, with the places filled in.
    config: serde_json::Value,
}

impl Described {
    /// The secret `name` of the file.
    fn secret(&self, name: &str) -> Result<SecretAsked, Box<dyn Error>> {
        let entry = self
            .file
            .secrets
            .get(name)
            .ok_or_else(|| format!("the agent has no [secrets.{name}]"))?;
        let config = self
            .places
            .expand_json(serde_json::to_value(&entry.config)?);
        Ok(SecretAsked {
            kind: entry.extension(),
            config,
        })
    }
}

/// An agent put together, and the places it was put together in.
struct Loaded {
    agent: Agent,
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
        // Said now, as a provider the file itself names wrongly is, and not by the first
        // request that finds nobody to ask.
        if !file.providers.contains_key(provider) {
            let has: Vec<&str> = file.providers.keys().map(String::as_str).collect();
            return Err(format!(
                "`--provider {provider}` names no [providers.{provider}] entry: this agent \
                 has {}",
                has.join(", ")
            )
            .into());
        }
        file.provider.clone_from(provider);
    }
    let file_path = file_path.map(|path| path.canonicalize()).transpose()?;
    let places = Places::new(workspace, file_path);
    Ok(Described { file, places })
}

/// The agent the options describe, put together in `workspace`, and the places it was put
/// together in.
fn load(
    options: &Options,
    workspace: PathBuf,
    standard: &Standard,
    out: &Out,
) -> Result<Loaded, Box<dyn Error>> {
    let Described { file, places } = agent_file(options, workspace)?;
    let agent = assemble_from(&file, &places, standard)?;
    for unstarted in &agent.failed {
        let line = format!(
            "! plugin {} did not start: {}",
            unstarted.plugin, unstarted.reason
        );
        say(out, &line);
    }
    Ok(Loaded { agent, places })
}

/// How a command ended: the process's exit code, or what went wrong.
type Done = Result<ExitCode, Box<dyn Error>>;

/// Carry out `command` if it is one that does not put the agent together: `init`, a
/// secret's value or sign-in, and maintenance of the log. Nothing else is written by these.
fn between_runs(
    command: &[&str],
    options: &Options,
    workspace: &Path,
    standard: &Standard,
    console: &mut Console,
) -> Option<Done> {
    let described = || agent_file(options, workspace.to_owned());
    let out = Arc::clone(&console.out);
    match command {
        ["init", words @ ..] => Some(init::init(words, workspace, standard, &out)),
        ["secrets"] => {
            Some(described().and_then(|described| secrets::list(&described, standard, &out)))
        }
        ["secrets", action @ ("set" | "remove"), named @ ..] if named.len() <= 1 => {
            Some(described().and_then(|described| {
                let named = named.first().copied();
                secrets::change(*action == "set", named, &described, standard, console)
            }))
        }
        [action @ ("login" | "logout"), named @ ..] if named.len() <= 1 => {
            Some(described().and_then(|described| {
                let named = named.first().copied();
                secrets::sign(*action == "login", named, &described, standard, console)
            }))
        }
        ["sessions", "remove", name] => Some(described().and_then(|Described { file, places }| {
            let log = file.log_path(&places);
            remove(&log, &SessionId::new(*name), standard, &out)
        })),
        _ => None,
    }
}

async fn run(
    options: Options,
    standard: &Standard,
    console: &mut Console,
) -> Result<ExitCode, Box<dyn Error>> {
    let workspace = match &options.workspace {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    }
    .canonicalize()?;
    let command: Vec<&str> = options.command.iter().map(String::as_str).collect();
    if let Some(done) = between_runs(&command, &options, &workspace, standard, console) {
        return done;
    }
    let out = &Arc::clone(&console.out);
    let Loaded { agent, places } = load(&options, workspace, standard, out)?;
    let harness = &agent.harness;
    let session = SessionId::new(options.session.as_deref().unwrap_or(MAIN));
    if !harness.session_list().iter().any(|info| info.id == session) {
        harness.start_session(session.clone(), None)?;
    }
    let paid = paid_by(&agent.file, standard);
    let unfiled = places.file.is_none();
    // What is typed from here on is prompts and answers, read so that a question put here
    // can be answered somewhere else first.
    let typed = std::mem::replace(&mut console.input, Box::new(io::empty()));
    let input = &mut Lines::new(typed);
    if let [] | ["run" | "continue", ..] = command.as_slice() {
        announce(&agent.file, unfiled, paid.as_deref(), out);
        open_fronts(harness, out);
    }
    let paid = paid.as_deref();
    let shown = Shown { out, paid, unfiled };
    match command.as_slice() {
        [] => chat(harness, session, input, &shown).await,
        ["run", words @ ..] if !words.is_empty() => {
            let _shown = terminal::watch(harness, &session, out);
            let prompt = words.join(" ");
            let outcome = terminal::prompt(harness, &session, &prompt, input, out).await?;
            Ok(finish(&outcome.status, &shown))
        }
        ["continue"] => {
            let _shown = terminal::watch(harness, &session, out);
            let outcome = terminal::resume(harness, &session, input, out).await?;
            Ok(finish(&outcome.status, &shown))
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
        ["doctor"] => Ok(doctor::doctor(&agent, &places, standard, out)),
        ["sessions"] => Ok(list_sessions(harness, out)),
        // Everything, unless a session was named: then that session and those it started.
        ["log"] => print_log(harness, options.session.as_ref().map(|_| &session), out),
        ["report"] => print_report(&agent, &session, "1", out),
        ["report", count] => print_report(&agent, &session, count, out),
        _ => Err(USAGE.into()),
    }
}

/// Print the sessions in the log, each with the session it was started from.
fn list_sessions(harness: &Harness, out: &Out) -> ExitCode {
    for info in harness.session_list() {
        let parent = info.parent.map(|parent| format!(" (from {parent})"));
        say(out, &format!("{}{}", info.id, parent.unwrap_or_default()));
    }
    ExitCode::SUCCESS
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

/// How the secret of the provider the loop asks is signed in to, if its kind is.
fn paid_by(file: &AgentFile, standard: &Standard) -> Option<Arc<dyn SignIn>> {
    let secret = file.providers.get(&file.provider)?.spec.secret.as_ref()?;
    let kind = file.secrets.get(secret)?.extension();
    standard.sign_in(&kind).cloned()
}

/// Say what a person should know before a run: which agent this is, if no file says, and
/// whose account its model is on, if it is paid by a sign-in.
fn announce(file: &AgentFile, unfiled: bool, paid: Option<&dyn SignIn>, out: &Out) {
    if unfiled {
        say(out, &default_agent(file));
    }
    if let Some(line) = paid.and_then(SignIn::using) {
        say(out, &line);
    }
}

/// What is said before a run of the agent used when there is no agent file: what it asks,
/// and the command that writes an agent for something else.
fn default_agent(file: &AgentFile) -> String {
    format!(
        "no {AGENT_FILE} here: this is the default agent, which asks Ollama on this machine \
         for `{}` and reads no key. {INIT}",
        file.model
    )
}

/// What writes an agent file, for whoever met the default agent and wanted another.
const INIT: &str = "`corduroy init` writes an agent for Anthropic, OpenAI, a ChatGPT plan or \
                    Ollama, to edit";

/// Where a run's end is said, what its model is paid through, if that is signed in to, and
/// whether the agent is the default one, used for want of an agent file.
struct Shown<'a> {
    out: &'a Out,
    paid: Option<&'a dyn SignIn>,
    unfiled: bool,
}

impl Shown<'_> {
    /// Say how a run ended, if there is something to say, and what the person can do about
    /// a failure that is the account's.
    fn end(&self, status: &Status) {
        if let Some(line) = terminal::ending(status) {
            say(self.out, &line);
        }
        if let (Status::Failed { reason }, Some(paid)) = (status, self.paid)
            && let Some(advice) = paid.advise(reason)
        {
            say(self.out, &advice);
        }
        // The default agent's model is one this machine may not serve.
        if let (Status::Failed { .. }, true) = (status, self.unfiled) {
            say(self.out, INIT);
        }
    }
}

fn finish(status: &Status, shown: &Shown<'_>) -> ExitCode {
    shown.end(status);
    match status {
        Status::Finished { .. } | Status::Yielded => ExitCode::SUCCESS,
        // As a program interrupted by Ctrl-C does.
        Status::Stopped => ExitCode::from(130),
        _ => ExitCode::FAILURE,
    }
}

/// What the agent has, a line each: the file it was read from, its model, log, registries,
/// tools and the plugins that did not start.
fn described(agent: &Agent) -> Vec<String> {
    let harness = &agent.harness;
    let file = &agent.file;
    // Which file, since a person may keep several and edit the one that is not read.
    let source = agent.source.as_ref().map_or_else(
        || format!("the default agent: no {AGENT_FILE} here"),
        |source| source.display().to_string(),
    );
    // What a person reaches it through besides this terminal: a page, if one is turned on.
    let fronts = harness.fronts().names();
    let fronts = if fronts.is_empty() {
        "none (the terminal alone)".to_owned()
    } else {
        fronts.join(", ")
    };
    let mut lines = vec![
        format!("agent     {source}"),
        format!("model     {} from {}", file.model, file.provider),
        format!("log       {}", agent.log.display()),
        format!("providers {}", harness.providers().names().join(", ")),
        format!("kernels   {}", harness.kernels().names().join(", ")),
        format!("gates     {}", harness.gates().names().join(", ")),
        format!("points    {}", harness.gates().points().join(", ")),
        format!("fronts    {fronts}"),
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

/// Open what a person reaches the agent through besides this terminal, now that runs are
/// about to be driven, and say where each is. What is said is not recorded: an address may
/// be what lets someone in.
fn open_fronts(harness: &Harness, out: &Out) {
    let fronts = harness.fronts();
    for front in fronts.names().iter().filter_map(|name| fronts.get(name)) {
        let (Ok(line) | Err(line)) = front.open();
        say(out, &line);
    }
}

/// Talk to the agent until the input ends or `/exit`.
async fn chat(
    harness: &Harness,
    mut session: SessionId,
    input: &mut Lines,
    ends: &Shown<'_>,
) -> Result<ExitCode, Box<dyn Error>> {
    let out = ends.out;
    say(
        out,
        "corduroy: type a prompt, /help for commands, /exit or ctrl-d to leave",
    );
    let mut shown = terminal::watch(harness, &session, out);
    let outcome = terminal::resume(harness, &session, input, out).await?;
    let started = !outcome.session.conversation().is_empty();
    if started {
        ends.end(&outcome.status);
    }
    // What is typed on the agent's page, say, begins a run as what is typed here does.
    let said = terminal::Said::listen(harness);
    loop {
        say_prompt(out);
        let line = tokio::select! {
            line = input.next() => Ok(line),
            message = said.next() => Err(message),
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(message) => {
                let within = if message.session == session {
                    String::new()
                } else {
                    format!("[{}] ", message.session)
                };
                say(
                    out,
                    &format!("{within}{} (typed on {})", message.text, message.via),
                );
                let outcome = terminal::taken_on(harness, &message.session, input, out).await?;
                ends.end(&outcome.status);
                continue;
            }
        };
        match typed(line.trim(), harness, &session, out) {
            Typed::Nothing => {}
            Typed::Exit => break,
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
                ends.end(&outcome.status);
            }
        }
    }
    said.close(harness);
    Ok(ExitCode::SUCCESS)
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
    fn a_failed_run_of_the_default_agent_says_what_writes_another() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let out: Out = Arc::<Mutex<Vec<u8>>>::clone(&written);
        let failed = Status::Failed {
            reason: "connection refused".to_owned(),
        };
        let said = |unfiled: bool, status: &Status| {
            written.lock().unwrap().clear();
            Shown {
                out: &out,
                paid: None,
                unfiled,
            }
            .end(status);
            String::from_utf8(written.lock().unwrap().clone()).unwrap()
        };
        // Only the agent no file describes, and only when its run failed.
        assert!(said(true, &failed).ends_with(&format!("{INIT}\n")));
        assert!(!said(false, &failed).contains("corduroy init"));
        assert!(!said(true, &Status::Stopped).contains("corduroy init"));

        let file = AgentFile::parse(&AgentFile::default_text()).unwrap();
        let line = default_agent(&file);
        assert!(
            line.starts_with("no corduroy.toml here: this is the default agent"),
            "{line}"
        );
        assert!(line.contains("for `qwen3` and reads no key") && line.contains("corduroy init"));
    }

    #[test]
    fn the_version_names_the_release() {
        let read = options(std::iter::once("--version".to_owned())).unwrap();
        assert!(read.version);
        assert!(version().starts_with(&format!("corduroy {}", env!("CARGO_PKG_VERSION"))));
    }
}
