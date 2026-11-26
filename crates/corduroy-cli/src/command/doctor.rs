//! `corduroy doctor`: try the agent for real, part by part, and say what works.
//!
//! `check` loads the agent and asks nothing of the outside. This goes one step on: it sees
//! whether each credential can be had, runs one command in each kernel, and asks each model
//! the agent names one short question. A run that fails afterwards is then the agent's
//! doing, and not a key, a sign-in or a network that was never working.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use corduroy_harness::core::{Message, ModelRequest};
use corduroy_std::Standard;

use super::{Described, secrets};
use crate::agent::{Agent, Places};
use crate::terminal::{Out, say};

/// How long a kernel has for one command, and a model for one short answer.
const KERNEL_WAIT: Duration = Duration::from_secs(20);
const MODEL_WAIT: Duration = Duration::from_secs(60);

/// What a kernel is asked to print.
const WORD: &str = "corduroy-doctor";

/// What was tried, said as it goes, and how much of it failed.
struct Tried<'a> {
    out: &'a Out,
    failed: u32,
    all: u32,
}

impl Tried<'_> {
    /// Say that `what` works, with `how`, or that it does not, with why.
    fn result(&mut self, part: &str, what: &str, outcome: Result<String, String>) {
        self.all = self.all.saturating_add(1);
        let line = match outcome {
            Ok(how) => format!("✓ {part:<8} {what}: {how}"),
            Err(why) => {
                self.failed = self.failed.saturating_add(1);
                format!("✗ {part:<8} {what}: {why}")
            }
        };
        say(self.out, &line);
    }
}

/// The models the agent names outright: the loop's, and those an enabled add-on's `llm`
/// grant names without a pattern, each as `<provider>/<model>`.
fn models(agent: &Agent) -> Vec<String> {
    let file = &agent.file;
    let mut names = vec![format!("{}/{}", file.provider, file.model)];
    let granted = file
        .plugins
        .iter()
        .filter(|plugin| plugin.enabled)
        .filter_map(|plugin| plugin.grants.get("llm"))
        .flat_map(|grant| grant.allowed())
        .filter(|model| !model.contains('*') && model.contains('/'));
    for model in granted {
        if !names.iter().any(|name| name == model) {
            names.push(model.to_owned());
        }
    }
    names
}

/// Ask `model` one short question, and say how long the answer took and what it cost.
fn ask(agent: &Agent, model: &str) -> Result<String, String> {
    let request = ModelRequest::new(
        model,
        "This is a check that you can be reached. Answer with the one word: ok",
        vec![Message::user().text("Are you there? Answer with the one word: ok")],
    );
    let began = Instant::now();
    let reply = agent
        .harness
        .models()
        .complete(request, MODEL_WAIT)
        .map_err(|error| error.to_string())?;
    let took = began.elapsed().as_millis();
    let tokens =
        |count: Option<u64>| count.map_or_else(|| "?".to_owned(), |count| count.to_string());
    Ok(format!(
        "answered in {took} ms ({} tokens in, {} out)",
        tokens(reply.usage.input_tokens),
        tokens(reply.usage.output_tokens)
    ))
}

/// Run one command in the kernel `name`.
fn run_in(agent: &Agent, name: &str) -> Result<String, String> {
    let began = Instant::now();
    let cell = agent
        .harness
        .kernels()
        .exec(name, &format!("echo {WORD}"), KERNEL_WAIT)
        .map_err(|error| error.to_string())?;
    if cell.status == 0 && cell.output.contains(WORD) {
        Ok(format!(
            "ran a command in {} ms",
            began.elapsed().as_millis()
        ))
    } else {
        Err(format!(
            "a command that only prints a word ended with status {}",
            cell.status
        ))
    }
}

/// What the person can do about a model's failure, if the plan that pays for it knows.
fn advice(agent: &Agent, standard: &Standard, model: &str, failure: &str) -> Option<String> {
    let (provider, _) = model.split_once('/')?;
    let secret = agent.file.providers.get(provider)?.spec.secret.as_ref()?;
    let kind = agent.file.secrets.get(secret)?.extension();
    standard.sign_in(&kind)?.advise(failure)
}

/// See whether each credential the agent names can be had, without reading one out.
fn credentials(agent: &Agent, places: &Places, standard: &Standard, tried: &mut Tried<'_>) {
    let described = Described {
        file: agent.file.clone(),
        places: places.clone(),
    };
    match secrets::standing(&described, standard) {
        Ok(all) => {
            for standing in all {
                let state = if standing.good {
                    Ok(standing.state)
                } else {
                    Err(standing.state)
                };
                tried.result("secret", &standing.what, state);
            }
        }
        Err(error) => tried.result("secret", "the agent's secrets", Err(error.to_string())),
    }
}

/// Try each part of `agent`, saying what works. Fails if anything does not.
pub(super) fn doctor(agent: &Agent, places: &Places, standard: &Standard, out: &Out) -> ExitCode {
    let mut tried = Tried {
        out,
        failed: 0,
        all: 0,
    };
    let from = places.file.as_ref().map_or_else(
        || "the default agent (there is no corduroy.toml here)".to_owned(),
        |file| file.display().to_string(),
    );
    let started = if agent.failed.is_empty() {
        Ok("everything it lists started".to_owned())
    } else {
        Err(format!(
            "{} of its plugins did not start",
            agent.failed.len()
        ))
    };
    tried.result("agent", &from, started);
    for unstarted in &agent.failed {
        tried.result("plugin", &unstarted.plugin, Err(unstarted.reason.clone()));
    }
    credentials(agent, places, standard, &mut tried);
    for kernel in agent.harness.kernels().names() {
        tried.result("kernel", &kernel, run_in(agent, &kernel));
    }
    for model in models(agent) {
        let answer = ask(agent, &model);
        let advised = answer
            .as_ref()
            .err()
            .and_then(|failure| advice(agent, standard, &model, failure));
        tried.result("model", &model, answer);
        if let Some(advised) = advised {
            say(out, &format!("           {advised}"));
        }
    }
    if tried.failed == 0 {
        say(out, &format!("all {} parts tried are working", tried.all));
        ExitCode::SUCCESS
    } else {
        let are = if tried.failed == 1 { "is" } else { "are" };
        say(
            out,
            &format!(
                "{} of the {} parts tried {are} not working",
                tried.failed, tried.all
            ),
        );
        ExitCode::FAILURE
    }
}
