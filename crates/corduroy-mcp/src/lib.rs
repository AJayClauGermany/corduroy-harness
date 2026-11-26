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
//! * **Over streamable HTTP**, at a URL, optionally with a bearer token: one handed over, or
//!   the secret a name stands for in the [`Secrets`] registry
//!   ([`McpPlugin::with_secret`]), revealed when the server is connected to.
//!
//! The client runs on a thread of its own with its own async runtime, because a plugin is
//! applied, and a tool called, from synchronous code that may itself be inside the loop's
//! runtime. Every wait on the server is bounded by the plugin's timeout.
//!
//! A tool's `readOnlyHint` makes it read-only for the gates, and `idempotentHint` (or being
//! read-only) makes it safe to call again after a crash.
//!
//! A tool's result keeps its text as text and its images as images ([`Content`]), in the
//! server's order. Other kinds of block (audio, resources) are passed on as text: a
//! resource's own text, or a note that the block was there.

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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use corduroy_core::{
    Content, Context, KERNELS, Kernels, Plugin, PluginError, Process, SECRETS, Secrets, Spawn,
    TOOLS, Tool, ToolError, ToolOutput, ToolSpec, Tools,
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
    /// The plugin's own name, `mcp-<server>`, which no WebAssembly plugin is given by chance.
    name: String,
    /// The server's name, which its tools are prefixed with.
    server: String,
    transport: Transport,
    /// The secret sent as the bearer token of a server over HTTP, in place of the
    /// transport's own.
    secret: Option<String>,
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

/// A plugin offering the tools of the MCP server `name`, reached through `transport`. The
/// plugin itself is called `mcp-<name>`.
#[must_use]
pub fn mcp_server(name: impl Into<String>, transport: Transport) -> McpPlugin {
    let server = name.into();
    McpPlugin {
        name: format!("mcp-{server}"),
        server,
        transport,
        secret: None,
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

    /// Send the secret registered as `secret` as the bearer token of a server over HTTP, in
    /// place of the transport's own. It is revealed once, when the server is connected to,
    /// so the secret has to be registered before this plugin is loaded.
    #[must_use]
    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = Some(secret.into());
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
            Transport::Http { .. } if self.secret.is_some() => {
                vec![TOOLS.to_owned(), SECRETS.to_owned()]
            }
            Transport::Http { .. } => vec![TOOLS.to_owned()],
        }
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        let connection = match &self.transport {
            Transport::Stdio { kernel, spawn } => {
                let kernels = ctx.service::<Kernels>(KERNELS)?;
                let process = kernels.spawn(kernel, spawn).map_err(|error| {
                    PluginError::new(format!("MCP server `{}`: {error}", self.server))
                })?;
                Connection::Process(process)
            }
            Transport::Http { url, bearer_token } => {
                let bearer_token = match &self.secret {
                    Some(secret) => Some(ctx.service::<Secrets>(SECRETS)?.reveal(secret).map_err(
                        |why| {
                            let server = &self.server;
                            PluginError::new(format!(
                                "MCP server `{server}`: secret `{secret}`: {why}"
                            ))
                        },
                    )?),
                    None => bearer_token.clone(),
                };
                Connection::Http {
                    url: url.clone(),
                    bearer_token,
                }
            }
        };
        let (client, tools) = Client::start(connection, self.timeout)
            .map_err(|error| PluginError::new(format!("MCP server `{}`: {error}", self.server)))?;
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
            let name = format!("{}__{}", self.server, tool.name);
            let input_schema = serde_json::to_string(&*tool.input_schema)
                .unwrap_or_else(|_| r#"{"type":"object"}"#.to_owned());
            let description = tool.description.as_deref().unwrap_or_default();
            let spec = ToolSpec::new(name.clone(), description, input_schema)
                .replay_safe(read_only || idempotent)
                .read_only(read_only);
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
                runtime.block_on(serve(connection, received, ready, timeout));
            })
            .map_err(|error| error.to_string())?;
        // The thread gives up at `timeout` itself; the slack lets its answer, rather than
        // this wait, say so.
        let tools = started
            .recv_timeout(timeout + Duration::from_secs(1))
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
/// Starting has `timeout` in all; a server that has not answered by then is given up, and
/// returning drops its process, which stops it.
async fn serve(
    connection: Connection,
    mut requests: mpsc::UnboundedReceiver<Request>,
    ready: sync_mpsc::Sender<Started>,
    timeout: Duration,
) {
    let started = tokio::time::timeout(timeout, async {
        // The process lives as long as this function; dropping it stops the server.
        let (service, process) = connect(connection).await?;
        let tools = service
            .peer()
            .list_all_tools()
            .await
            .map_err(|error| format!("listing tools: {error}"))?;
        Ok::<_, String>((service, process, tools))
    })
    .await
    .unwrap_or_else(|_| Err(format!("did not start within {}s", timeout.as_secs())));
    let (service, _process) = match started {
        Ok((service, process, tools)) => {
            if ready.send(Ok(tools)).is_err() {
                return;
            }
            (service, process)
        }
        Err(error) => {
            drop(ready.send(Err(error)));
            return;
        }
    };
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

    fn call(&self, input: &str) -> Result<ToolOutput, ToolError> {
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
        let output = output_of(&result);
        if result.is_error == Some(true) {
            Err(ToolError::Failed(output.to_text()))
        } else {
            Ok(output)
        }
    }
}

/// A tool's result as content blocks: its images as images, and everything else as text (a
/// text block, a resource's text, a note for any other kind of block), in the server's
/// order. Text that follows text joins it, a line each, so a result with no image is one
/// block of text; one with no blocks at all is its structured content as JSON.
fn output_of(result: &CallToolResult) -> ToolOutput {
    let blocks = serde_json::to_value(&result.content).unwrap_or(Value::Null);
    let mut content: Vec<Content> = Vec::new();
    for block in blocks.as_array().into_iter().flatten() {
        let text = |pointer: &str| block.pointer(pointer).and_then(Value::as_str);
        let part = match text("/type") {
            Some("text") => text("/text").unwrap_or_default().to_owned(),
            Some("resource") => {
                text("/resource/text").map_or_else(|| "[a resource]".to_owned(), str::to_owned)
            }
            Some("image") => {
                // An image whose data is not what MCP says it is cannot be shown.
                let data = text("/data").and_then(|data| BASE64.decode(data).ok());
                if let Some(data) = data {
                    let kind = text("/mimeType").unwrap_or("application/octet-stream");
                    content.push(Content::image(kind, data));
                    continue;
                }
                "[image content]".to_owned()
            }
            Some(other) => format!("[{other} content]"),
            None => String::new(),
        };
        if let Some(Content::Text(before)) = content.last_mut() {
            before.push('\n');
            before.push_str(&part);
        } else {
            content.push(Content::Text(part));
        }
    }
    if content.is_empty() {
        let structured = result.structured_content.as_ref();
        return ToolOutput::text(structured.map(Value::to_string).unwrap_or_default());
    }
    ToolOutput::new(content)
}
