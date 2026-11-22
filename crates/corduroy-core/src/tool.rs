//! Tools: the things a model can ask the harness to do.
//!
//! The harness ships no tools of its own. Plugins register them in the [`Tools`] registry,
//! which the host provides as the service named [`TOOLS`].

use std::error::Error;
use std::fmt;

use crate::registry::Registry;

/// Name of the service under which the host provides the [`Tools`] registry.
pub const TOOLS: &str = "tools";

/// The registry of every tool currently available to the model.
pub type Tools = Registry<dyn Tool>;

/// How a tool presents itself to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    /// Unique name the model uses to call the tool.
    pub name: String,
    /// What the tool does and when to use it.
    pub description: String,
    /// JSON Schema for the tool's input object, as JSON text.
    pub input_schema: String,
    /// Whether a call may simply be made again when nobody knows if it already ran.
    ///
    /// A run that is picked up after a crash finds calls it asked for and has no result for.
    /// Such a call is repeated only if its tool says here that doing it twice is the same as
    /// doing it once (a read, a write of fixed contents). Otherwise the model is told the
    /// call was interrupted and decides for itself.
    pub replay_safe: bool,
    /// Whether the tool only looks and never changes anything. Gates see this, so an
    /// approval policy can let reads through and ask about writes.
    pub read_only: bool,
}

/// Why a tool call failed. These are shown to the model, not treated as fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// No tool is registered under this name.
    UnknownTool(String),
    /// The tool rejected its input.
    InvalidInput(String),
    /// The tool accepted its input but could not complete.
    Failed(String),
    /// The plugin behind the tool lacks a capability the call needed.
    Denied(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(f, "unknown tool `{name}`"),
            Self::InvalidInput(reason) => write!(f, "invalid input: {reason}"),
            Self::Failed(reason) => write!(f, "tool failed: {reason}"),
            Self::Denied(reason) => write!(f, "permission denied: {reason}"),
        }
    }
}

impl Error for ToolError {}

/// Something the model can ask the harness to do.
pub trait Tool: Send + Sync {
    /// How the tool presents itself to the model.
    fn spec(&self) -> ToolSpec;

    /// Run the tool on `input`, a JSON object matching [`ToolSpec::input_schema`].
    ///
    /// # Errors
    ///
    /// Returns a [`ToolError`] if the input is invalid or the tool cannot complete.
    fn call(&self, input: &str) -> Result<String, ToolError>;
}

/// A [`Tool`] built from a spec and a closure. See [`tool_fn`].
pub struct FnTool<F> {
    spec: ToolSpec,
    call: F,
}

impl<F> fmt::Debug for FnTool<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnTool")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

/// Build a [`Tool`] from its spec and the function that runs it.
pub const fn tool_fn<F>(spec: ToolSpec, call: F) -> FnTool<F>
where
    F: Fn(&str) -> Result<String, ToolError> + Send + Sync,
{
    FnTool { spec, call }
}

impl<F> Tool for FnTool<F>
where
    F: Fn(&str) -> Result<String, ToolError> + Send + Sync,
{
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn call(&self, input: &str) -> Result<String, ToolError> {
        (self.call)(input)
    }
}

impl Registry<dyn Tool> {
    /// The specs of every registered tool, sorted by name.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.entries().iter().map(|tool| tool.spec()).collect()
    }

    /// Look up `name` and run it with `input`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::UnknownTool`] if `name` is not registered, or whatever error the
    /// tool itself returns.
    pub fn call(&self, name: &str, input: &str) -> Result<String, ToolError> {
        // The registry lock is released before the tool runs, so a slow tool blocks nobody.
        self.get(name)
            .ok_or_else(|| ToolError::UnknownTool(name.to_owned()))?
            .call(input)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn shout() -> impl Tool {
        let spec = ToolSpec {
            name: "shout".into(),
            description: "Upper-cases its input.".into(),
            input_schema: "{}".into(),
            replay_safe: true,
            read_only: true,
        };
        tool_fn(spec, |input| {
            if input.is_empty() {
                return Err(ToolError::InvalidInput("nothing to shout".into()));
            }
            Ok(input.to_uppercase())
        })
    }

    #[test]
    fn dispatches_by_name_and_reports_failures_as_data() {
        let tools = Tools::new();
        tools.insert("shout", Arc::new(shout())).unwrap();

        assert_eq!(tools.call("shout", "hi"), Ok("HI".to_owned()));
        assert_eq!(tools.specs().len(), 1);
        assert_eq!(
            tools.call("shout", "").unwrap_err().to_string(),
            "invalid input: nothing to shout"
        );
        assert_eq!(
            tools.call("whisper", "hi"),
            Err(ToolError::UnknownTool("whisper".to_owned()))
        );
        assert_eq!(
            ToolError::Denied("no".into()).to_string(),
            "permission denied: no"
        );
        assert_eq!(ToolError::Failed("x".into()).to_string(), "tool failed: x");
    }
}
