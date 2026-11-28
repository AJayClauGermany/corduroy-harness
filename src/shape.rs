//! Shaping a request: steps that change what the model is sent, and not the record.
//!
//! **A prototype.** That shaping a request is a step of its own, registered as an
//! [`Operation`](crate::Operation) is and leaving the session in the log as it was, is
//! decided. Its form is not (G19 in `docs/open-questions.md`), and this module is one form
//! to try it with: nothing here is promised, and `docs/prototypes.md` says what it is for
//! and what it found.
//!
//! What the model is sent is the session's projection, which only a `compaction` event
//! changes. Other ways of keeping a context small or cheap (dropping old tool results,
//! trimming images, adding a reminder) change what is *sent* and not what happened. Each is
//! a [`Shape`]: handed the request the loop is about to make, it gives back the one to
//! send. Shapers are registered by name in the [`Shapers`] registry, which the harness
//! provides as the service named [`SHAPERS`], so a plugin contributes one and unloading the
//! plugin takes it away. [`Run::shape`](crate::Run::shape) runs them in name order, each on
//! what the one before gave back; the [`StandardLoop`](crate::StandardLoop) does so before
//! every request, and a loop of one's own may.
//!
//! A shaper changes the messages, the system prompt and the tools offered. It does not
//! change the model: a session keeps the one it has. What it did is in the record as far
//! as a review needs: a shaped `model/request` names the shapers that changed it
//! (`shaped`) and what the messages it carried came to (`messages_hash`).
//!
//! ```
//! use std::sync::Arc;
//! use corduroy_harness::scripted::{ScriptedProvider, say};
//! use corduroy_harness::{Harness, ModelRequest, Session, Shape};
//!
//! /// Send only the last message.
//! struct Latest;
//!
//! impl Shape for Latest {
//!     fn shape(&self, _: &Session, mut request: ModelRequest) -> ModelRequest {
//!         request.messages = request.messages.split_off(request.messages.len().saturating_sub(1));
//!         request
//!     }
//! }
//!
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! let harness = Harness::new(Arc::new(ScriptedProvider::new([say("one"), say("two")])))?;
//! harness.shapers().insert("latest", Arc::new(Latest))?;
//!
//! harness.run("first").await?;
//! harness.run("second").await?;
//! // The record holds the whole conversation, whatever was sent.
//! assert_eq!(harness.journal(&corduroy_harness::MAIN.into())?.session().conversation().len(), 4);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # })?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use alloc::string::String;
use alloc::vec::Vec;

use corduroy_core::{ModelRequest, Registry};

use crate::session::Session;

/// Name of the service under which the harness provides the [`Shapers`] registry.
pub const SHAPERS: &str = "shapers";

/// The registry of every shaper, by name. They run in name order.
pub type Shapers = Registry<dyn Shape>;

/// A step that changes what the model is sent. See [the module](self).
pub trait Shape: Send + Sync {
    /// The request to send in the place of `request`, which the loop is about to make for
    /// `session`. Giving `request` back as it came is how a shaper passes.
    ///
    /// A shaper reads the session and changes nothing but the request: what it leaves out
    /// is still in the log, and is sent again by a request it does not shape. It is called
    /// on the loop's own thread before every request, so it does little.
    fn shape(&self, session: &Session, request: ModelRequest) -> ModelRequest;
}

impl<F> Shape for F
where
    F: Fn(&Session, ModelRequest) -> ModelRequest + Send + Sync,
{
    fn shape(&self, session: &Session, request: ModelRequest) -> ModelRequest {
        self(session, request)
    }
}

/// A request as the shapers left it: what to send, and which of them changed it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Shaped {
    /// The request to send.
    pub request: ModelRequest,
    /// The names of the shapers that changed the request, in the order they ran. Empty for
    /// a request that is the session's projection as the harness made it.
    pub by: Vec<String>,
}

impl Shaped {
    /// `request` as nobody shaped it.
    #[must_use]
    pub const fn plain(request: ModelRequest) -> Self {
        Self {
            request,
            by: Vec::new(),
        }
    }
}

/// `request` after each shaper of `shapers` has had it, in name order.
pub(crate) fn shape(shapers: &Shapers, session: &Session, request: ModelRequest) -> Shaped {
    let model = request.model.clone();
    let mut shaped = Shaped::plain(request);
    for name in shapers.names() {
        let Some(step) = shapers.get(&name) else {
            continue;
        };
        let before = shaped.request.clone();
        let mut after = step.shape(session, before.clone());
        // The model is the session's, and not a shaper's to choose.
        after.model.clone_from(&model);
        if after != before {
            shaped.by.push(name);
        }
        shaped.request = after;
    }
    shaped
}
