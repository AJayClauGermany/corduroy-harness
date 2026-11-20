//! The acting half of the loop: tools and the registry that dispatches to them.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

/// Why a tool call failed. These are shown to the model, not treated as fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// No tool is registered under this name.
    UnknownTool(String),
    /// The tool rejected its input.
    InvalidInput(String),
    /// The tool accepted its input but could not complete.
    Failed(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(f, "unknown tool `{name}`"),
            Self::InvalidInput(reason) => write!(f, "invalid input: {reason}"),
            Self::Failed(reason) => write!(f, "tool failed: {reason}"),
        }
    }
}

impl Error for ToolError {}

/// Something the model can ask the harness to do.
pub trait Tool: fmt::Debug {
    /// Unique name the model uses to call this tool.
    fn name(&self) -> &str;

    /// One-line description of what the tool does and what input it expects.
    fn description(&self) -> &str;

    /// Run the tool.
    ///
    /// # Errors
    ///
    /// Returns a [`ToolError`] if the input is invalid or the tool cannot complete.
    fn call(&self, input: &str) -> Result<String, ToolError>;
}

/// The set of tools available to an agent, looked up by name.
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a tool, replacing any existing tool with the same name.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.name().to_owned(), Box::new(tool));
    }

    /// Builder-style [`register`](Self::register).
    #[must_use]
    pub fn with(mut self, tool: impl Tool + 'static) -> Self {
        self.register(tool);
        self
    }

    /// Names of all registered tools, in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    /// Look up `name` and run it with `input`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::UnknownTool`] if `name` is not registered, or whatever
    /// error the tool itself returns.
    pub fn call(&self, name: &str, input: &str) -> Result<String, ToolError> {
        self.tools
            .get(name)
            .ok_or_else(|| ToolError::UnknownTool(name.to_owned()))?
            .call(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::{Add, Echo};

    #[test]
    fn dispatches_by_name() {
        let registry = ToolRegistry::new().with(Echo).with(Add);

        assert_eq!(registry.call("echo", "hi"), Ok("hi".to_owned()));
        assert_eq!(registry.call("add", "1 2"), Ok("3".to_owned()));
        assert_eq!(registry.names().collect::<Vec<_>>(), ["add", "echo"]);
    }

    #[test]
    fn unknown_tool_is_an_error_not_a_panic() {
        let registry = ToolRegistry::new();

        assert_eq!(
            registry.call("nope", ""),
            Err(ToolError::UnknownTool("nope".to_owned()))
        );
    }

    #[test]
    fn registering_the_same_name_replaces_the_tool() {
        let registry = ToolRegistry::new().with(Echo).with(Echo);

        assert_eq!(registry.names().count(), 1);
    }
}
