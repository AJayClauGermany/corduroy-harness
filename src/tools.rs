//! Running the tool calls the model asked for.

#[cfg(feature = "std")]
use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use anyhow::Result;
use corduroy_core::call::Serve;
use corduroy_core::{Message, ToolCall, ToolError, ToolOutput, ToolResult, Tools};

use crate::session::Journal;
use crate::stop::Stop;

/// What a call that was never run because the run was stopped is answered with.
pub(crate) const NOT_RUN: &str = "not run: the run was stopped before this call";

/// Runs tool calls from the [`Tools`] registry, each with a trail of its own in the log.
///
/// The registry is read when each call is made, so loading or unloading a plugin changes
/// what the model can do from the next step on. Each call is stored as starting
/// (`tool/start`) before the tool runs, and its result (`tool/result`) as soon as it returns,
/// so that after a crash the log tells a call that never began from one that was running
/// and from one that finished.
#[derive(Debug)]
pub struct PluginTools {
    tools: Arc<Tools>,
}

impl PluginTools {
    /// Run tools from `tools`.
    #[must_use]
    pub const fn new(tools: Arc<Tools>) -> Self {
        Self { tools }
    }

    /// Run `calls` of `journal`'s session one after another and return the message that
    /// carries their results to the model. Once `stop` is asked, the calls not yet begun are
    /// answered as not run.
    ///
    /// A call whose tool says it waits for a signal (see [`corduroy_core::call`]) is
    /// recorded as waiting and answered with a placeholder until the signal comes.
    pub(crate) async fn run(
        &self,
        journal: &Arc<Journal>,
        calls: Vec<ToolCall>,
        stop: &Stop,
    ) -> Result<Message> {
        let mut results = Message::user();
        for call in calls {
            if stop.asked() {
                results = results.result(ToolResult::failed(call.id, NOT_RUN));
                continue;
            }
            // Intent first: if this cannot be stored, the tool must not run.
            journal.start_call(&call.id)?;
            let tools = Arc::clone(&self.tools);
            let serve = Serve::session(journal.id().clone())
                .call(call.id.clone())
                .stopped_by(stop.flag());
            let (name, input) = (call.name.clone(), call.input.clone());
            // Tools are synchronous and may run for a while (a shell command, say). The call
            // is served in its scope, so what it calls knows its session, can make it wait,
            // and can tell when the run is asked to stop.
            let (outcome, waits) = blocking(move || serve.run(|| tools.call(&name, &input)))
                .await
                .unwrap_or_else(|panic| {
                    let panicked = ToolError::Failed(format!("the tool panicked: {panic}"));
                    (Err(panicked), Vec::new())
                });
            // A call that waits gets its result from a signal; until then a placeholder stands
            // in, which the model never sees, since the run stops before asking it again.
            if let Some(signal) = waits.first() {
                let placeholder = journal.wait_call(&call.id, signal)?;
                results = results.result(ToolResult::new(call.id, true, placeholder));
                continue;
            }
            // A failed tool is data for the model, not an error for the run.
            let (ok, output) = match outcome {
                Ok(output) => (true, output),
                Err(error) => (false, ToolOutput::text(error.to_string())),
            };
            journal.finish_call(&call.id, ok, &output)?;
            results = results.result(ToolResult::new(call.id, ok, output));
        }
        Ok(results)
    }
}

/// Run `work`, which may block for a while (a tool, a service that asks a model), without
/// holding up the executor the loop runs on. Returns why, if it panicked and the panic could
/// be caught.
///
/// On a tokio runtime, with the `tokio` feature, the work goes to tokio's blocking pool.
/// Anywhere else it runs here, where the loop runs, as it must on a device with one thread.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    #[cfg(feature = "tokio")]
    if tokio::runtime::Handle::try_current().is_ok() {
        return tokio::task::spawn_blocking(work)
            .await
            .map_err(|error| error.to_string());
    }
    #[cfg(feature = "std")]
    {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).map_err(|panic| {
            panic
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "it gave no reason".to_owned())
        })
    }
    #[cfg(not(feature = "std"))]
    {
        Ok(work())
    }
}
