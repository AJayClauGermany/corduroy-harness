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

mod text;

use std::fmt::Write as _;
use std::sync::OnceLock;

use corduroy_guest::host::{fs_read, fs_write, kernel};
use corduroy_guest::{Plugin, PluginInfo, ToolError, ToolSpec, parse};
use serde::Deserialize;

/// Lines returned by `read` and kept from `bash` output unless the call asks otherwise.
const MAX_LINES: usize = 2000;
/// Bytes returned by `read` and kept from `bash` output.
const MAX_BYTES: usize = 50 * 1024;
/// Seconds a `bash` command may run when the call does not say.
const DEFAULT_TIMEOUT: u32 = 120;

/// Plugin configuration, from the host.
#[derive(Deserialize)]
#[serde(default)]
struct Config {
    /// Name of the host kernel `bash` runs in.
    kernel: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            kernel: "shell".to_owned(),
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

#[derive(Deserialize)]
struct Bash {
    command: String,
    /// Seconds.
    timeout: Option<u32>,
}

struct PiTools;

impl Plugin for PiTools {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        // A second activation in the same instance keeps the first configuration.
        CONFIG.get_or_init(|| config);
        Ok(PluginInfo::new("pi-tools", env!("CARGO_PKG_VERSION")).tools(specs()))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
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

fn bash(args: &Bash) -> Result<String, ToolError> {
    let kernel = CONFIG
        .get()
        .map_or("shell", |config| config.kernel.as_str());
    let timeout_ms = args.timeout.unwrap_or(DEFAULT_TIMEOUT).saturating_mul(1000);
    let cell = kernel::exec(kernel, &args.command, timeout_ms)?;
    let (mut output, dropped) = text::tail(&cell.output, MAX_LINES, MAX_BYTES);
    if dropped || cell.truncated {
        output.insert_str(0, "[Earlier output was truncated.]\n");
    }
    if cell.status != 0 {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        let _ = write!(output, "[exit status {}]", cell.status);
    }
    Ok(output)
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
    vec![
        spec(
            "read",
            (true, true),
            "Read a text file. Returns at most 2000 lines or 50KB; use offset and limit to \
             page through larger files.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","description":"First line to return, counting from 1"},"limit":{"type":"integer","description":"Maximum number of lines"}},"required":["path"]}"#,
        ),
        spec(
            "write",
            (true, false),
            "Create or overwrite a file with the given content. Parent directories are \
             created as needed.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#,
        ),
        spec(
            "edit",
            (false, false),
            "Replace exact text in a file. Each oldText must occur exactly once in the file \
             as it is now; edits must not overlap.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"edits":{"type":"array","items":{"type":"object","properties":{"oldText":{"type":"string"},"newText":{"type":"string"}},"required":["oldText","newText"]}}},"required":["path","edits"]}"#,
        ),
        spec(
            "bash",
            (false, false),
            "Run a shell command. The shell is persistent: the working directory and \
             variables carry over between calls. Returns the last 2000 lines or 50KB of output.",
            r#"{"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"integer","description":"Seconds before the command is stopped (default 120)"}},"required":["command"]}"#,
        ),
        spec(
            "tree",
            (true, true),
            "Show the files and directories below a path as a tree, a few levels deep. \
             .git, node_modules, target and .venv are named but not opened.",
            r#"{"type":"object","properties":{"path":{"type":"string","description":"Directory (default .)"},"depth":{"type":"integer","description":"Levels to show (default 3, at most 10)"}}}"#,
        ),
    ]
}
