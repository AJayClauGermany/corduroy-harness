//! Have a second model review the changes in the workspace.
//!
//! The model in the loop marks its own work. This plugin gets the change it made, through
//! `kernel` (`git diff` against a base, and the files git does not track yet, read through
//! `fs-read`), and asks another model through `llm` (one from another provider, ideally) what
//! is wrong with it. The answer is the tool's result, for the model in the loop to act on.
//!
//! The reviewer is asked once and has no tools: it sees what it is sent. A reviewer that
//! explores the workspace on its own is a session of its own, which needs the model a session
//! runs on to be chosen per session (`docs/open-questions.md`, G12).
//!
//! Configuration: `model`, the model to ask, which the host must serve and the `llm` grant
//! must reach; `kernel`, the kernel `git` runs in (`shell` by default); `dir`, the repository
//! (the kernel's directory by default); `base`, what the change is compared with (`HEAD` by
//! default: what is not committed yet); `max_bytes`, the most text sent (200 KB by default);
//! `instructions`, more for the reviewer to look for.
//!
//! Tool: `review`.

mod request;

use std::sync::OnceLock;

use corduroy_guest::host::llm::{self, Completion, Role, Turn};
use corduroy_guest::host::{fs_read, kernel};
use corduroy_guest::{Plugin, PluginInfo, ToolError, ToolSpec, parse};
use serde::Deserialize;

use request::Changes;

/// How long one `git` command may take, in milliseconds.
const GIT_TIMEOUT_MS: u32 = 30_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    model: String,
    #[serde(default = "default_kernel")]
    kernel: String,
    dir: Option<String>,
    #[serde(default = "default_base")]
    base: String,
    #[serde(default = "default_max_bytes")]
    max_bytes: usize,
    instructions: Option<String>,
}

fn default_kernel() -> String {
    "shell".to_owned()
}

fn default_base() -> String {
    "HEAD".to_owned()
}

const fn default_max_bytes() -> usize {
    200_000
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Deserialize)]
struct Review {
    goal: Option<String>,
    base: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
}

/// The output of `git <args>` in the repository, or why it failed.
fn git(config: &Config, args: &str) -> Result<(String, bool), ToolError> {
    let git = match &config.dir {
        Some(dir) => format!("git -C {}", request::quote(dir).map_err(ToolError::Failed)?),
        None => "git".to_owned(),
    };
    let cell = kernel::exec(&config.kernel, &format!("{git} {args}"), GIT_TIMEOUT_MS)?;
    if cell.status != 0 {
        return Err(ToolError::Failed(format!(
            "`git {args}` failed (exit status {}): {}",
            cell.status,
            cell.output.trim()
        )));
    }
    Ok((cell.output, cell.truncated))
}

/// `path` as `fs-read` wants it: in the repository, unless it is absolute.
fn located(config: &Config, path: &str) -> String {
    let absolute = path.starts_with('/') || path.get(1..3) == Some(":\\");
    match &config.dir {
        Some(dir) if !absolute => format!("{}/{path}", dir.trim_end_matches(['/', '\\'])),
        _ => path.to_owned(),
    }
}

fn review(input: &Review) -> Result<String, ToolError> {
    let config = CONFIG
        .get()
        .ok_or_else(|| ToolError::Failed("review was not configured".to_owned()))?;
    let base = request::reference(input.base.as_deref().unwrap_or(&config.base))
        .map_err(ToolError::InvalidInput)?;
    let quoted = request::quote(base).map_err(ToolError::InvalidInput)?;
    let (diff, diff_cut) = git(
        config,
        &format!("--no-pager diff --no-color --no-ext-diff --no-textconv {quoted} --"),
    )?;
    let (listing, _) = git(config, "ls-files --others --exclude-standard")?;
    let new = request::untracked(&listing);
    if diff.trim().is_empty() && new.is_empty() && input.paths.is_empty() {
        return Ok(format!("There are no changes against `{base}` to review."));
    }

    let mut files = Vec::new();
    let mut unreadable = 0;
    for path in new.iter().chain(&input.paths) {
        // Files it may not read, or that are not text, are left out.
        match fs_read::read(&located(config, path)) {
            Ok(contents) => files.push((path.clone(), contents)),
            Err(_) => unreadable += 1,
        }
    }
    let (text, no_room) = request::prompt(
        &Changes {
            goal: input.goal.as_deref(),
            base,
            diff: &diff,
            files: &files,
        },
        config.max_bytes,
    );
    let request = Completion {
        system: request::system(config.instructions.as_deref()),
        messages: vec![Turn {
            role: Role::User,
            text,
        }],
    };
    let answer = llm::complete(&config.model, &request)?;

    let mut notes = Vec::new();
    if diff_cut {
        notes.push("the host cut the diff short".to_owned());
    }
    if unreadable + no_room > 0 {
        notes.push(format!(
            "{} file(s) were left out: unreadable, not text, or over the limit",
            unreadable + no_room
        ));
    }
    let notes = if notes.is_empty() {
        String::new()
    } else {
        format!("\n\n[{}]", notes.join("; "))
    };
    Ok(format!(
        "Review by {} of the changes against `{base}`:\n\n{answer}{notes}",
        config.model
    ))
}

struct Reviewer;

impl Plugin for Reviewer {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        request::reference(&config.base)?;
        if let Some(dir) = &config.dir {
            request::quote(dir)?;
        }
        CONFIG.get_or_init(|| config);
        Ok(PluginInfo::new("review", env!("CARGO_PKG_VERSION")).tool(
            ToolSpec::new(
                "review",
                "Have another model review your changes in the workspace (what `git diff` \
                 shows against a base, and new files) and say what is wrong with them. Use it \
                 when a change is made and before you say the work is done; then fix what it \
                 finds, or say why not. `goal` is what the change is meant to do; `paths` are \
                 more files the reviewer should read whole.",
                r#"{"type":"object","properties":{"goal":{"type":"string"},"base":{"type":"string","description":"commit, branch or tag to compare with; HEAD by default"},"paths":{"type":"array","items":{"type":"string"}}}}"#,
            )
            .read_only(),
        ))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "review" => review(&parse(&input)?),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }
}

corduroy_guest::export!(Reviewer);
