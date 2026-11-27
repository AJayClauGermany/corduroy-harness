//! The minimal coding toolset, after Pi: `read`, `write`, `edit` and `bash`, and `tree`
//! from Goose's developer extension.
//!
//! Four tools are enough for a model to do real work, and each one the model does not have
//! to choose between is prompt space and attention saved. This plugin holds no authority of
//! its own: files are reached through the `fs-read` and `fs-write` capabilities and commands
//! run in a host kernel through the `kernel` capability, each within whatever scope the host
//! granted.
//!
//! `bash` runs in a long-lived kernel rather than a fresh process, so the working directory
//! and variables carry over from one call to the next. `tree` shows a directory's layout
//! through `fs-read` alone, so an agent without a shell can still find its way.
//!
//! `tools` in the configuration lists the ones to offer, all five when it is absent. It is
//! how a tool is left to another add-on: a name belongs to one plugin, so `bash` from
//! `lean-shell`, say, means this one is configured without it. `bash`'s contract (its input,
//! its result, the call into the kernel) is the `shell-tool` crate's, which both build on.

mod text;

use std::fmt::Write as _;
use std::sync::OnceLock;

use corduroy_guest::host::{fs_read, fs_write};
use corduroy_guest::{Keep, Plugin, PluginInfo, ToolError, ToolSpec, parse};
use serde::Deserialize;

/// Lines returned by `read` and kept from `bash` output unless the call asks otherwise.
const MAX_LINES: usize = 2000;
/// Bytes returned by `read` and kept from `bash` output.
const MAX_BYTES: usize = 50 * 1024;
/// The tools this add-on has, in the order it offers them.
const TOOLS: [&str; 5] = ["read", "write", "edit", shell_tool::NAME, "tree"];

/// Plugin configuration, from the host.
#[derive(Deserialize)]
#[serde(default)]
struct Config {
    /// Name of the host kernel `bash` runs in.
    kernel: String,
    /// The tools to offer, by name; all of them when absent.
    tools: Option<Vec<String>>,
}

impl Config {
    /// Whether the tool `name` is offered.
    fn offers(&self, name: &str) -> bool {
        self.tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|tool| tool == name))
    }

    /// The first name in `tools` that is no tool of this add-on's.
    fn unknown(&self) -> Option<&str> {
        self.tools
            .iter()
            .flatten()
            .map(String::as_str)
            .find(|tool| !TOOLS.contains(tool))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            kernel: "shell".to_owned(),
            tools: None,
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Deserialize)]
struct Read {
    path: String,
    /// First line to return, counting from 1.
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct Write {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct Edit {
    path: String,
    edits: Vec<text::Replacement>,
}

#[derive(Deserialize)]
struct Tree {
    /// The directory to show, `.` by default.
    path: Option<String>,
    /// How many levels deep to go.
    depth: Option<usize>,
}

/// Entries `tree` shows at most.
const MAX_ENTRIES: usize = 1000;
/// Directories `tree` names but does not open.
const UNOPENED: [&str; 5] = [".git/", "node_modules/", "target/", ".corduroy/", ".venv/"];

struct PiTools;

impl Plugin for PiTools {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        if let Some(unknown) = config.unknown() {
            return Err(format!(
                "bad config: `tools` names `{unknown}`, and the tools are {}",
                TOOLS.join(", ")
            ));
        }
        // A second activation in the same instance keeps the first configuration.
        let config = CONFIG.get_or_init(|| config);
        let offered: Vec<ToolSpec> = specs()
            .into_iter()
            .filter(|spec| config.offers(&spec.name))
            .collect();
        Ok(PluginInfo::new("pi-tools", env!("CARGO_PKG_VERSION")).tools(offered))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        // A tool left to another add-on is not this one's to run.
        if CONFIG.get().is_some_and(|config| !config.offers(&name)) {
            return Err(ToolError::UnknownTool(name));
        }
        match name.as_str() {
            "read" => read(&parse(&input)?),
            "write" => write(&parse(&input)?),
            "edit" => edit(&parse(&input)?),
            "bash" => bash(&parse(&input)?),
            "tree" => tree(&parse(&input)?),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }
}

corduroy_guest::export!(PiTools);

fn read(args: &Read) -> Result<String, ToolError> {
    let contents = fs_read::read(&args.path)?;
    if let Some(binary) = shell_tool::binary(&contents) {
        return Ok(format!(
            "[{}: {binary}, not text, so it is not shown.]",
            args.path
        ));
    }
    let offset = args.offset.unwrap_or(1).max(1);
    let limit = args.limit.unwrap_or(MAX_LINES).min(MAX_LINES);
    let page = text::page(&contents, offset, limit, MAX_BYTES);
    if page.shown == 0 && page.total > 0 {
        return Err(ToolError::InvalidInput(format!(
            "offset {offset} is past the end of the file ({} lines)",
            page.total
        )));
    }
    let mut output = page.text;
    let last = offset + page.shown.saturating_sub(1);
    if last < page.total {
        // Writing to a `String` cannot fail.
        let _ = write!(
            output,
            "\n\n[Showing lines {offset}-{last} of {}. Use offset={} to continue.]",
            page.total,
            last + 1
        );
    }
    Ok(output)
}

fn write(args: &Write) -> Result<String, ToolError> {
    fs_write::write(&args.path, &args.content)?;
    Ok(format!(
        "Wrote {} bytes to {}",
        args.content.len(),
        args.path
    ))
}

fn edit(args: &Edit) -> Result<String, ToolError> {
    let original = fs_read::read(&args.path)?;
    let edited = text::replace(&original, &args.edits).map_err(ToolError::InvalidInput)?;
    fs_write::write(&args.path, &edited)?;
    Ok(format!(
        "Applied {} edit(s) to {}",
        args.edits.len(),
        args.path
    ))
}

fn bash(args: &shell_tool::Call) -> Result<String, ToolError> {
    let kernel = CONFIG
        .get()
        .map_or("shell", |config| config.kernel.as_str());
    let limits = shell_tool::Limits {
        lines: MAX_LINES,
        bytes: MAX_BYTES,
    };
    shell_tool::run(kernel, &args.command, args.timeout, limits)
}

fn tree(args: &Tree) -> Result<String, ToolError> {
    let root = args.path.clone().unwrap_or_else(|| ".".to_owned());
    let depth = args.depth.unwrap_or(3).clamp(1, 10);
    let mut lines = vec![root.clone()];
    let entries = fs_read::list(&root)?;
    walk(&root, &entries, "", depth, &mut lines);
    Ok(lines.join("\n"))
}

/// Add `entries` of `dir`, and below them their own, `depth` levels down.
fn walk(dir: &str, entries: &[String], prefix: &str, depth: usize, lines: &mut Vec<String>) {
    for (index, name) in entries.iter().enumerate() {
        if lines.len() > MAX_ENTRIES {
            lines.push(format!("{prefix}… stopped at {MAX_ENTRIES} entries"));
            return;
        }
        let last = index + 1 == entries.len();
        lines.push(format!(
            "{prefix}{}{name}",
            if last { "└── " } else { "├── " }
        ));
        if !name.ends_with('/') || depth <= 1 || UNOPENED.contains(&name.as_str()) {
            continue;
        }
        let child = format!(
            "{}/{}",
            dir.trim_end_matches('/'),
            name.trim_end_matches('/')
        );
        let inner = format!("{prefix}{}", if last { "    " } else { "│   " });
        match fs_read::list(&child) {
            Ok(children) => walk(&child, &children, &inner, depth - 1, lines),
            Err(_) => lines.push(format!("{inner}(not readable)")),
        }
    }
}

/// The most one result should take of the model's context, in tokens of three bytes: what
/// [`MAX_BYTES`] comes to. The host holds a call to less for a model that holds little.
const RESULT_TOKENS: u32 = 16_384;

fn specs() -> Vec<ToolSpec> {
    // `(replay_safe, read_only)`. Reading twice, or writing the same content twice, is the
    // same as once; an edit that already happened no longer finds its old text, and a command
    // can do anything. Only `read` changes nothing.
    let spec = |name: &str,
                (replay_safe, read_only): (bool, bool),
                description: &str,
                input_schema: &str| ToolSpec {
        replay_safe,
        read_only,
        ..ToolSpec::new(name, description, input_schema)
    };
    // Which part of a result too long for the model is kept: the start of a file or a
    // listing, the end of a command's output, where the error is. `write` and `edit`
    // answer in a line.
    let kept = |spec: ToolSpec| {
        let keep = match spec.name.as_str() {
            "read" | "tree" => Keep::Head,
            // `bash` says its own, with the rest of its contract.
            _ => return spec,
        };
        spec.result(RESULT_TOKENS, keep)
    };
    [
        spec(
            "read",
            (true, true),
            "Read a text file. Returns at most 2000 lines or 50KB; use offset and limit to \
             page through larger files. A binary file is named by its kind and size.",
            r#"{"type":"object","properties":{"path":{"type":"string","description":"The file, relative to the working directory, such as src/main.rs; an absolute path is rarely needed"},"offset":{"type":"integer","description":"First line to return, counting from 1"},"limit":{"type":"integer","description":"Maximum number of lines"}},"required":["path"]}"#,
        ),
        spec(
            "write",
            (true, false),
            "Create or overwrite a file with the given content. Parent directories are \
             created as needed.",
            r#"{"type":"object","properties":{"path":{"type":"string","description":"The file, relative to the working directory, such as src/main.rs; an absolute path is rarely needed"},"content":{"type":"string"}},"required":["path","content"]}"#,
        ),
        spec(
            "edit",
            (false, false),
            "Replace exact text in a file. Each oldText must occur exactly once in the file \
             as it is now; edits must not overlap.",
            r#"{"type":"object","properties":{"path":{"type":"string","description":"The file, relative to the working directory, such as src/main.rs; an absolute path is rarely needed"},"edits":{"type":"array","items":{"type":"object","properties":{"oldText":{"type":"string"},"newText":{"type":"string"}},"required":["oldText","newText"]}}},"required":["path","edits"]}"#,
        ),
        shell_tool::spec(
            "Run a shell command. The shell is persistent: the working directory and \
             variables carry over between calls. Returns the last 2000 lines or 50KB of \
             output; binary output is named by its kind and size.",
            RESULT_TOKENS,
        ),
        spec(
            "tree",
            (true, true),
            "Show the files and directories below a path as a tree, a few levels deep. \
             .git, node_modules, target and .venv are named but not opened.",
            r#"{"type":"object","properties":{"path":{"type":"string","description":"The directory, relative to the working directory (default .)"},"depth":{"type":"integer","description":"Levels to show (default 3, at most 10)"}}}"#,
        ),
    ]
    .map(kept)
    .to_vec()
}
