//! HTTP clients: what the `http` capability sends requests through.
//!
//! The core checks a plugin's request (the host it goes to, the secrets it carries) and
//! records it; sending it is a client's job. Clients are registered in the [`HttpClients`]
//! registry, which the host provides as the service named [`HTTP`], under the URL scheme they
//! serve (`http`, `https`). A client is a plugin, so a proxying, caching or recording client
//! replaces the plain one without the core changing.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::time::Duration;

use crate::registry::Registry;

/// Name of the service under which the host provides the [`HttpClients`] registry.
pub const HTTP: &str = "http";

/// The registry of HTTP clients, by the URL scheme each serves.
pub type HttpClients = Registry<dyn HttpClient>;

/// A request, with every secret already written in.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// `GET`, `POST` and so on.
    pub method: String,
    /// The absolute URL.
    pub url: String,
    /// Header names and values, in order.
    pub headers: Vec<(String, String)>,
    /// The body, if any.
    pub body: Option<Vec<u8>>,
}

impl fmt::Debug for HttpRequest {
    // Header values may hold secrets, so they are left out.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &names)
            .finish_non_exhaustive()
    }
}

/// A response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// Header names and values, in order.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

/// Something that sends HTTP requests.
///
/// A client must not follow redirects: the host checked the request's host against the
/// plugin's grant, and a redirect to another host would go around that check. A redirect is
/// a response like any other, and the plugin decides whether to make the next request.
pub trait HttpClient: Send + Sync {
    /// Send `request` and wait at most `timeout` for the whole response.
    ///
    /// # Errors
    ///
    /// Returns why no response was had: the host could not be reached, the connection
    /// failed, the time ran out. A response with an error status is a response, not an error.
    fn send(&self, request: &HttpRequest, timeout: Duration) -> Result<HttpResponse, String>;
}

impl<F> HttpClient for F
where
    F: Fn(&HttpRequest, Duration) -> Result<HttpResponse, String> + Send + Sync,
{
    fn send(&self, request: &HttpRequest, timeout: Duration) -> Result<HttpResponse, String> {
        self(request, timeout)
    }
}
