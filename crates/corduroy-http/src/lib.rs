//! An HTTP client for the corduroy `http` capability.
//!
//! The core checks a WebAssembly plugin's request against its grant and writes in its
//! secrets; [`UreqClient`] sends it. It follows no redirects (the host checked one host, and a
//! redirect is a response the plugin decides about), treats an error status as a response,
//! refuses a response body larger than [`MAX_BODY`], and honours `HTTPS_PROXY` and its
//! relatives.
//!
//! [`http_client`] is the plugin that registers it for `http` and `https`:
//!
//! ```no_run
//! use corduroy_core::App;
//! # fn app() -> App { App::new() }
//! let mut app = app();
//! app.plugin(corduroy_http::http_client());
//! ```

use std::io::Read as _;
use std::sync::Arc;
use std::time::Duration;

use corduroy_core::{
    Context, HTTP, HttpClient, HttpClients, HttpRequest, HttpResponse, Plugin, PluginError,
};
use ureq::Agent;
use ureq::http::{Method, Request};

/// The largest response body the client accepts: 16 MiB.
pub const MAX_BODY: u64 = 16 * 1024 * 1024;

/// Sends requests with `ureq`, one at a time, following no redirects.
#[derive(Debug, Clone)]
pub struct UreqClient {
    agent: Agent,
}

impl Default for UreqClient {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqClient {
    /// A client with its own connection pool.
    #[must_use]
    pub fn new() -> Self {
        let config = Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.into(),
        }
    }
}

impl HttpClient for UreqClient {
    fn send(&self, request: &HttpRequest, timeout: Duration) -> Result<HttpResponse, String> {
        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|error| format!("bad method `{}`: {error}", request.method))?;
        let mut builder = Request::builder().method(method).uri(&request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let body = request.body.clone().unwrap_or_default();
        let built = builder.body(body).map_err(|error| error.to_string())?;
        let configured = self
            .agent
            .configure_request(built)
            .timeout_global(Some(timeout))
            .build();
        let mut response = self
            .agent
            .run(configured)
            .map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let mut body = Vec::new();
        // One byte past the limit tells a body that fits from one that was cut.
        response
            .body_mut()
            .as_reader()
            .take(MAX_BODY + 1)
            .read_to_end(&mut body)
            .map_err(|error| error.to_string())?;
        if u64::try_from(body.len()).map_or(true, |length| length > MAX_BODY) {
            return Err(format!(
                "the response body is larger than {} MiB",
                MAX_BODY / 1024 / 1024
            ));
        }
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// The plugin that registers a [`UreqClient`] for `http` and `https` in the host's
/// [`HttpClients`] registry.
#[derive(Debug, Default)]
pub struct HttpClientPlugin {
    client: UreqClient,
}

/// A plugin registering a [`UreqClient`] for `http` and `https`.
#[must_use]
pub fn http_client() -> HttpClientPlugin {
    HttpClientPlugin::default()
}

impl Plugin for HttpClientPlugin {
    fn name(&self) -> &'static str {
        "http-client"
    }

    fn inject(&self) -> Vec<String> {
        vec![HTTP.to_owned()]
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        let clients = ctx.service::<HttpClients>(HTTP)?;
        let client: Arc<dyn HttpClient> = Arc::new(self.client.clone());
        for scheme in ["http", "https"] {
            clients.register(ctx, scheme, Arc::clone(&client))?;
        }
        Ok(())
    }
}
