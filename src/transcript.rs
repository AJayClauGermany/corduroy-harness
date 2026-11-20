//! The running record of a single agent run.

use std::fmt;

use crate::model::Action;

/// One thing that happened during a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// The task the agent was asked to perform. Always the first entry.
    Task(String),
    /// An action the model chose.
    Action(Action),
    /// Successful output from a tool call.
    Output(String),
    /// A tool call that failed; shown to the model so it can recover.
    ToolError(String),
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Task(task) => write!(f, "task: {task}"),
            Self::Action(action) => write!(f, "action: {action}"),
            Self::Output(output) => write!(f, "output: {output}"),
            Self::ToolError(error) => write!(f, "tool error: {error}"),
        }
    }
}

/// Ordered history of a run, starting with the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    entries: Vec<Entry>,
}

impl Transcript {
    /// Start a transcript for `task`.
    #[must_use]
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            entries: vec![Entry::Task(task.into())],
        }
    }

    /// Every entry so far, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The most recent entry. A transcript always has at least the task.
    #[must_use]
    pub fn last(&self) -> Option<&Entry> {
        self.entries.last()
    }

    /// How many actions the model has taken so far.
    #[must_use]
    pub fn action_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, Entry::Action(_)))
            .count()
    }

    pub(crate) fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }
}

impl fmt::Display for Transcript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            write!(f, "{entry}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_the_task() {
        let transcript = Transcript::new("do it");

        assert_eq!(transcript.entries(), [Entry::Task("do it".into())]);
        assert_eq!(transcript.action_count(), 0);
    }

    #[test]
    fn counts_only_actions_and_renders_one_line_per_entry() {
        let mut transcript = Transcript::new("t");
        transcript.push(Entry::Action(Action::CallTool {
            name: "echo".into(),
            input: "x".into(),
        }));
        transcript.push(Entry::Output("x".into()));
        transcript.push(Entry::ToolError("nope".into()));

        assert_eq!(transcript.action_count(), 1);
        assert_eq!(
            transcript.to_string(),
            "task: t\naction: call echo(x)\noutput: x\ntool error: nope"
        );
    }
}
