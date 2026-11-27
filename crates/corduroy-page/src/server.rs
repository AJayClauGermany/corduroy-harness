//! The page's server: the plugin, the front it registers, and the threads that answer.

mod http;
mod served;

use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use corduroy_harness::core::{Context, EventStream, Plugin, PluginError};
use corduroy_harness::{FRONTS, Front, Fronts, OUTSIDE, Outside};

use http::{Response, Unread};
use served::Served;

/// How long a connection may take to send its request, or to take its answer.
const PATIENCE: Duration = Duration::from_secs(10);
/// Connections served at once: a browser opens a handful.
const MAX_CONNECTIONS: usize = 32;

/// How a page is set up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
    /// The port to listen on, or 0 for any that is free.
    pub port: u16,
}

impl Config {
    /// A page on any free port.
    #[must_use]
    pub const fn new() -> Self {
        Self { port: 0 }
    }

    /// On `port`, or on any free one if it is 0.
    #[must_use]
    pub const fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

/// A page being served: where, and what stops it.
struct Serving {
    address: SocketAddr,
    token: String,
    stop: Arc<AtomicBool>,
}

impl Serving {
    fn line(&self) -> String {
        format!(
            "the agent's page is at http://{}/{}/ (a gate's question can be answered there \
             as well as here; the address is new each time, and lets in whoever has it)",
            self.address, self.token
        )
    }
}

/// One page: closed until the host opens it.
struct Page {
    name: String,
    config: Config,
    events: Arc<EventStream>,
    outside: Arc<Outside>,
    serving: Mutex<Option<Serving>>,
}

/// A token for the address: 32 characters nobody can guess.
fn token() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| format!("no random bytes: {error}"))?;
    // Each byte as its two hexadecimal digits.
    Ok(bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .filter_map(|digit| char::from_digit(u32::from(digit), 16))
        .collect())
}

/// Answer one connection.
fn handle(mut stream: TcpStream, served: &Served) -> io::Result<()> {
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;
    let response = match http::read(&mut stream) {
        Ok(request) => served.answer_to(&request),
        Err(Unread::Bad(status, why)) => Response::plain(status, why),
        Err(Unread::Gone(error)) => return Err(error),
    };
    response.write(&mut stream)
}

/// Take connections until told to stop, each on a thread of its own.
fn accept(listener: &TcpListener, served: &Arc<Served>, stop: &AtomicBool) {
    let live = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else {
            continue;
        };
        if live.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
            // Dropped: the browser tries again.
            continue;
        }
        live.fetch_add(1, Ordering::SeqCst);
        let (served, done) = (Arc::clone(served), Arc::clone(&live));
        let spawned = thread::Builder::new()
            .name("corduroy-page-connection".to_owned())
            .spawn(move || {
                // A connection that fails has nobody to tell but the browser, which knows.
                drop(handle(stream, &served));
                done.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            live.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Page {
    /// Stop serving, if serving.
    fn close(&self) {
        let serving = self
            .serving
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(serving) = serving {
            serving.stop.store(true, Ordering::SeqCst);
            // The thread waits for a connection: this one tells it to look.
            drop(TcpStream::connect_timeout(&serving.address, PATIENCE));
        }
    }
}

impl Front for Page {
    fn open(&self) -> Result<String, String> {
        let mut serving = self.serving.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(serving) = serving.as_ref() {
            return Ok(serving.line());
        }
        let wanted = (Ipv4Addr::LOCALHOST, self.config.port);
        let listener = TcpListener::bind(wanted)
            .and_then(|listener| Ok((listener.local_addr()?, listener)))
            .map_err(|error| {
                format!(
                    "the page `{}` is not served: 127.0.0.1:{}: {error}",
                    self.name, self.config.port
                )
            });
        let (address, listener) = listener?;
        let opened = Serving {
            address,
            token: token()?,
            stop: Arc::new(AtomicBool::new(false)),
        };
        let served = Arc::new(Served {
            name: self.name.clone(),
            port: address.port(),
            token: opened.token.clone(),
            events: Arc::clone(&self.events),
            outside: Arc::clone(&self.outside),
            last: Mutex::new(0),
        });
        let stop = Arc::clone(&opened.stop);
        thread::Builder::new()
            .name("corduroy-page".to_owned())
            .spawn(move || accept(&listener, &served, &stop))
            .map_err(|error| format!("the page `{}` is not served: {error}", self.name))?;
        let line = opened.line();
        *serving = Some(opened);
        drop(serving);
        Ok(line)
    }
}

/// The plugin that serves a page. See [`page`].
#[derive(Debug)]
pub struct PagePlugin {
    name: String,
    config: Config,
}

/// A plugin named `name` that serves the agent's page as `config` says, once the host opens
/// it: it registers a [`Front`] in [`Fronts`] and waits. Unloading it stops the page.
#[must_use]
pub fn page(name: impl Into<String>, config: Config) -> PagePlugin {
    PagePlugin {
        name: name.into(),
        config,
    }
}

impl Plugin for PagePlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn inject(&self) -> Vec<String> {
        vec![OUTSIDE.to_owned(), FRONTS.to_owned()]
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        let page = Arc::new(Page {
            name: self.name.clone(),
            config: self.config,
            events: ctx.events(),
            outside: ctx.service::<Outside>(OUTSIDE)?,
            serving: Mutex::new(None),
        });
        let front: Arc<dyn Front> = Arc::<Page>::clone(&page);
        ctx.service::<Fronts>(FRONTS)?
            .register(ctx, self.name.clone(), front)?;
        ctx.effect(move || page.close());
        Ok(())
    }
}
