//! MCP servers as corduroy plugins.
//!
//! [`mcp_server`] is a native plugin that connects to one [Model Context
//! Protocol](https://modelcontextprotocol.io) server and registers each of its tools in the
//! [`Tools`] registry as `<server>__<tool>`, as Goose names them. Unloading the plugin removes
//! the tools and stops the server.
//!
//! A server is reached one of two ways:
//!
//! * **Over standard input and output**, as a process. The process is started through a
//!   kernel ([`Kernels::spawn`]), never directly: what a kernel does to the code it runs (a
//!   container, a sandbox) it does to the MCP servers it starts, so the choice of kernel stays
//!   the one decision about running code.
//! * **Over streamable HTTP**, at a URL, optionally with a bearer token.
//!
//! The client runs on a thread of its own with its own async runtime, because a plugin is
//! applied, and a tool called, from synchronous code that may itself be inside the loop's
//! runtime. Every wait on the server is bounded by the plugin's timeout.
//!
//! A tool's `readOnlyHint` makes it read-only for the gates, and `idempotentHint` (or being
//! read-only) makes it safe to call again after a crash.

#![expect(
    clippy::multiple_crate_versions,
    reason = "the rmcp and reqwest dependency trees pull several versions of shared crates"
)]

use std::fmt;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc as sync_mpsc;
use std::thread;
use std::time::Duration;

use corduroy_core::{
    Context, KERNELS, Kernels, Plugin, PluginError, Process, Spawn, TOOLS, Tool, ToolError,
    ToolSpec, Tools,
};
use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject};
use rmcp::service::{Peer, RoleClient};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;

/// How long a server has to start and list its tools, and a tool to answer, by default.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Where an MCP server is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// A process, started through the kernel `kernel`, speaking over its standard input and
    /// output.
    Stdio {
        /// The kernel that starts it.
        kernel: String,
        /// The program, its arguments and environment.
        spawn: Spawn,
    },
    /// A server at `url`, over streamable HTTP.
    Http {
        /// Where it is.
        url: String,
        /// Sent as `Authorization: Bearer <token>`.
        bearer_token: Option<String>,
    },
}

/// A plugin that connects to one MCP server and offers its tools. See [`mcp_server`].
pub struct McpPlugin {
    name: String,
    transport: Transport,
    timeout: Duration,
}

impl fmt::Debug for McpPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A bearer token is a secret.
        f.debug_struct("McpPlugin")
            .field("name", &self.name)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// A plugin offering the tools of the MCP server `name`, reached through `transport`.
#[must_use]
pub fn mcp_server(name: impl Into<String>, transport: Transport) -> McpPlugin {
    McpPlugin {
        name: name.into(),
        transport,
        timeout: DEFAULT_TIMEOUT,
    }
}

impl McpPlugin {
    /// Wait at most `timeout` for the server to start, and for each tool call.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl Plugin for McpPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn inject(&self) -> Vec<String> {
        match self.transport {
            Transport::Stdio { .. } => vec![TOOLS.to_owned(), KERNELS.to_owned()],
            Transport::Http { .. } => vec![TOOLS.to_owned()],
        }
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        let connection = match &self.transport {
            Transport::Stdio { kernel, spawn } => {
                let kernels = ctx.service::<Kernels>(KERNELS)?;
                let process = kernels.spawn(kernel, spawn).map_err(|error| {
                    PluginError::new(format!("MCP server `{}`: {error}", self.name))
                })?;
                Connection::Process(process)
            }
            Transport::Http { url, bearer_token } => Connection::Http {
                url: url.clone(),
                bearer_token: bearer_token.clone(),
            },
        };
        let (client, tools) = Client::start(connection, self.timeout)
            .map_err(|error| PluginError::new(format!("MCP server `{}`: {error}", self.name)))?;
        let client = Arc::new(client);
        let registry = ctx.service::<Tools>(TOOLS)?;
        for tool in tools {
            let annotations = tool.annotations.as_ref();
            let read_only = annotations
                .and_then(|annotations| annotations.read_only_hint)
                .unwrap_or(false);
            let idempotent = annotations
                .and_then(|annotations| annotations.idempotent_hint)
                .unwrap_or(false);
            let name = format!("{}__{}", self.name, tool.name);
            let spec = ToolSpec {
                name: name.clone(),
                description: tool.description.as_deref().unwrap_or_default().to_owned(),
                input_schema: serde_json::to_string(&*tool.input_schema)
                    .unwrap_or_else(|_| r#"{"type":"object"}"#.to_owned()),
                replay_safe: read_only || idempotent,
                read_only,
            };
            let remote: Arc<dyn Tool> = Arc::new(McpTool {
                client: Arc::clone(&client),
                tool: tool.name.to_string(),
                spec,
            });
            registry.register(ctx, name, remote)?;
        }
        // When the last tool and this effect let go of the client, its channel closes, the
        // client's thread ends, and the connection and the process go with it.
        ctx.effect(move || drop(client));
        Ok(())
    }
}

/// How the client reaches the server.
enum Connection {
    Process(Box<dyn Process>),
    Http {
        url: String,
        bearer_token: Option<String>,
    },
}

/// What the plugin asks of the client's thread.
enum Request {
    Call {
        tool: String,
        arguments: Option<JsonObject>,
        reply: sync_mpsc::Sender<Result<CallToolResult, String>>,
    },
}

/// A connection to a server, served by a thread with a runtime of its own. Dropping it ends
/// the thread, which closes the connection and stops the process.
struct Client {
    requests: mpsc::UnboundedSender<Request>,
    timeout: Duration,
}

type Started = Result<Vec<rmcp::model::Tool>, String>;

impl Client {
    /// Connect, and list the server's tools, within `timeout`.
    fn start(
        connection: Connection,
        timeout: Duration,
    ) -> Result<(Self, Vec<rmcp::model::Tool>), String> {
        let (requests, received) = mpsc::unbounded_channel();
        let (ready, started) = sync_mpsc::channel::<Started>();
        thread::Builder::new()
            .name("mcp-client".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        drop(ready.send(Err(error.to_string())));
                        return;
                    }
                };
                runtime.block_on(serve(connection, received, ready));
            })
            .map_err(|error| error.to_string())?;
        let tools = started
            .recv_timeout(timeout)
            .map_err(|_| format!("did not start within {}s", timeout.as_secs()))??;
        Ok((Self { requests, timeout }, tools))
    }

    fn call(&self, tool: &str, arguments: Option<JsonObject>) -> Result<CallToolResult, ToolError> {
        let (reply, answered) = sync_mpsc::channel();
        self.requests
            .send(Request::Call {
                tool: tool.to_owned(),
                arguments,
                reply,
            })
            .map_err(|_| ToolError::Failed("the MCP server is gone".to_owned()))?;
        answered
            .recv_timeout(self.timeout)
            .map_err(|_| {
                ToolError::Failed(format!(
                    "the MCP server did not answer within {}s",
                    self.timeout.as_secs()
                ))
            })?
            .map_err(ToolError::Failed)
    }
}

/// The client's thread: connect, report the tools, then answer calls until the plugin goes.
async fn serve(
    connection: Connection,
    mut requests: mpsc::UnboundedReceiver<Request>,
    ready: sync_mpsc::Sender<Started>,
) {
    // The process lives as long as this function; dropping it stops the server.
    let (service, _process) = match connect(connection).await {
        Ok(connected) => connected,
        Err(error) => {
            drop(ready.send(Err(error)));
            return;
        }
    };
    let tools = service
        .peer()
        .list_all_tools()
        .await
        .map_err(|error| format!("listing tools: {error}"));
    if ready.send(tools).is_err() {
        return;
    }
    let peer: Peer<RoleClient> = service.peer().clone();
    while let Some(request) = requests.recv().await {
        match request {
            Request::Call {
                tool,
                arguments,
                reply,
            } => {
                let peer = peer.clone();
                tokio::spawn(async move {
                    drop(reply.send(call(&peer, tool, arguments).await));
                });
            }
        }
    }
    drop(service.cancel().await);
}

async fn call(
    peer: &Peer<RoleClient>,
    tool: String,
    arguments: Option<JsonObject>,
) -> Result<CallToolResult, String> {
    let params = CallToolRequestParams::new(tool);
    let params = match arguments {
        Some(arguments) => params.with_arguments(arguments),
        None => params,
    };
    peer.call_tool(params)
        .await
        .map_err(|error| error.to_string())
}

type Service = rmcp::service::RunningService<RoleClient, ()>;

async fn connect(connection: Connection) -> Result<(Service, Option<Box<dyn Process>>), String> {
    match connection {
        Connection::Process(mut process) => {
            let (Some(stdin), Some(stdout)) = (process.stdin(), process.stdout()) else {
                return Err("the process has no pipes".to_owned());
            };
            let (ours, theirs) = tokio::io::duplex(64 * 1024);
            pipe(stdin, stdout, theirs);
            let service =
                ().serve(tokio::io::split(ours))
                    .await
                    .map_err(|error| format!("handshake: {error}"))?;
            Ok((service, Some(process)))
        }
        Connection::Http { url, bearer_token } => {
            let mut config = StreamableHttpClientTransportConfig::with_uri(url);
            if let Some(token) = bearer_token {
                config = config.auth_header(token);
            }
            let service =
                ().serve(StreamableHttpClientTransport::from_config(config))
                    .await
                    .map_err(|error| format!("handshake: {error}"))?;
            Ok((service, None))
        }
    }
}

/// Join a process's blocking pipes to the async end `theirs`, a thread each way.
fn pipe(
    mut stdin: Box<dyn Write + Send>,
    mut stdout: Box<dyn Read + Send>,
    theirs: tokio::io::DuplexStream,
) {
    let (mut from_client, mut to_client) = tokio::io::split(theirs);
    let handle = tokio::runtime::Handle::current();
    let outward = handle.clone();
    thread::spawn(move || {
        let mut buffer = vec![0; 16 * 1024];
        while let Ok(read) = stdout.read(&mut buffer) {
            let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
                break;
            };
            if handle.block_on(to_client.write_all(chunk)).is_err() {
                break;
            }
        }
    });
    thread::spawn(move || {
        let mut buffer = vec![0; 16 * 1024];
        while let Ok(read) = outward.block_on(from_client.read(&mut buffer)) {
            let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
                break;
            };
            if stdin.write_all(chunk).and_then(|()| stdin.flush()).is_err() {
                break;
            }
        }
    });
}

/// One tool of a server.
struct McpTool {
    client: Arc<Client>,
    tool: String,
    spec: ToolSpec,
}

impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn call(&self, input: &str) -> Result<String, ToolError> {
        let arguments = match serde_json::from_str::<Value>(input) {
            Ok(Value::Object(arguments)) => Some(arguments),
            Ok(Value::Null) => None,
            Ok(_) => {
                return Err(ToolError::InvalidInput(
                    "the input must be an object".to_owned(),
                ));
            }
            Err(error) => return Err(ToolError::InvalidInput(error.to_string())),
        };
        let result = self.client.call(&self.tool, arguments)?;
        let text = text_of(&result);
        if result.is_error == Some(true) {
            Err(ToolError::Failed(text))
        } else {
            Ok(text)
        }
    }
}

/// The text of a tool's result: its text blocks, a note for anything else, or its structured
/// content as JSON when there is no text.
fn text_of(result: &CallToolResult) -> String {
    let blocks = serde_json::to_value(&result.content).unwrap_or(Value::Null);
    let parts: Vec<String> = blocks
        .as_array()
        .into_iter()
        .flatten()
        .map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            Some("resource") => block
                .pointer("/resource/text")
                .and_then(Value::as_str)
                .map_or_else(|| "[a resource]".to_owned(), str::to_owned),
            Some(other) => format!("[{other} content]"),
            None => String::new(),
        })
        .collect();
    if parts.is_empty() {
        return result
            .structured_content
            .as_ref()
            .map(Value::to_string)
            .unwrap_or_default();
    }
    parts.join("\n")
}
