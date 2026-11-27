//! The agent as a page in a browser, served on this machine.
//!
//! The `viewer` add-on writes the record as a page of files, which shows and cannot be
//! answered through: an add-on has no way to listen. This is the same page, served, so
//! that a gate's question can be answered on it. It listens on a socket, so it is an
//! extension: [`page`] is the plugin, and `[extensions.<name>] kind = "page"` in an agent
//! file turns it on.
//!
//! It drives nothing. An answer given on the page is recorded through
//! [`Outside`](corduroy_harness::Outside) and the request returns; the host, which drives
//! the runs, takes the run on. It is a [`Front`](corduroy_harness::Front): the host opens it
//! when it is about to drive runs, and shows the person its address.
//!
//! **Who is let in.** Any program on the machine can open a local port, and a page from
//! anywhere can ask a browser to fetch a local address. So:
//!
//! * it listens on `127.0.0.1` only;
//! * the address holds a token, random and new each time the page opens, and a request
//!   without it is refused. The address is shown to the person and never recorded: the
//!   record is something the agent's own tools can read;
//! * a request has to be addressed to this machine by its own address, so a name someone
//!   pointed here does not reach it;
//! * an answer has to come from the page itself, as the browser says, and as JSON;
//! * the page is told to load and reach nothing but what is served beside it.
//!
//! What it speaks (the scripts the page loads, and `POST answer`) is its own and the
//! page's, and promised to nobody else.

#![expect(
    clippy::multiple_crate_versions,
    reason = "the harness's wasmtime dependency tree pulls several versions of shared crates"
)]

mod server;

pub use server::{Config, PagePlugin, page};
