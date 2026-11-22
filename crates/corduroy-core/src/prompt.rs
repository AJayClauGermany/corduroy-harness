//! Prompt parts: text that plugins contribute to the system prompt.
//!
//! A part is rendered afresh for every model request, so a plugin can describe its current
//! state (the skills it knows, the kernels that are running) rather than a snapshot.

use crate::registry::Registry;
use alloc::string::String;
use alloc::vec::Vec;

/// Name of the service under which the host provides the [`Prompts`] registry.
pub const PROMPTS: &str = "prompts";

/// The registry of every prompt part, rendered in name order.
pub type Prompts = Registry<dyn PromptPart>;

/// A contribution to the system prompt.
pub trait PromptPart: Send + Sync {
    /// The text to include in the next request, or `None` to contribute nothing this time.
    fn render(&self) -> Option<String>;
}

impl<F> PromptPart for F
where
    F: Fn() -> Option<String> + Send + Sync,
{
    fn render(&self) -> Option<String> {
        self()
    }
}

impl Registry<dyn PromptPart> {
    /// Render every part that has something to say, as `(name, text)` in name order.
    #[must_use]
    pub fn render(&self) -> Vec<(String, String)> {
        self.names()
            .into_iter()
            .filter_map(|name| {
                let text = self.get(&name)?.render()?;
                Some((name, text))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::sync::Arc;

    use super::*;

    #[test]
    fn renders_non_empty_parts_in_name_order() {
        let prompts = Prompts::new();
        prompts
            .insert("b", Arc::new(|| Some("second".to_owned())))
            .unwrap();
        prompts.insert("quiet", Arc::new(|| None)).unwrap();
        prompts
            .insert("a", Arc::new(|| Some("first".to_owned())))
            .unwrap();

        assert_eq!(
            prompts.render(),
            [
                ("a".to_owned(), "first".to_owned()),
                ("b".to_owned(), "second".to_owned())
            ]
        );
    }
}
