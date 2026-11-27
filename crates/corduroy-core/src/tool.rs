//! Tools: the things a model can ask the harness to do.
//!
//! The harness ships no tools of its own. Plugins register them in the [`Tools`] registry,
//! which the host provides as the service named [`TOOLS`].
//!
//! A tool is described by a [`ToolSpec`] and returns a [`ToolOutput`]: a list of [`Content`]
//! blocks, text or images. Both can grow without breaking the code that builds them, so a
//! spec is made with [`ToolSpec::new`] and an output from text or from blocks:
//!
//! ```
//! use corduroy_core::{Content, Tool, ToolError, ToolOutput, ToolSpec, tool_fn};
//!
//! let spec = ToolSpec::new("shout", "Upper-cases its input.", r#"{"type":"object"}"#)
//!     .read_only(true)
//!     .replay_safe(true);
//! // A closure may return text, or anything else that is a `ToolOutput`.
//! let shout = tool_fn(spec, |input| Ok(input.to_uppercase()));
//! assert_eq!(shout.call("hi")?.to_text(), "HI");
//!
//! let chart = ToolOutput::new(vec![
//!     Content::text("the chart"),
//!     Content::image("image/png", vec![0x89, b'P', b'N', b'G']),
//! ]);
//! assert_eq!(chart.to_text(), "the chart\n[image: image/png, 4 bytes]");
//! # Ok::<(), ToolError>(())
//! ```

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;

use crate::context::Context;
use crate::registry::{NotReplaced, Registry};

/// Name of the service under which the host provides the [`Tools`] registry.
pub const TOOLS: &str = "tools";

/// The registry of every tool currently available to the model.
pub type Tools = Registry<dyn Tool>;

/// How a tool presents itself to the model.
///
/// More may be said about a tool in later versions, so a spec is built with
/// [`new`](Self::new) and the methods that follow it, never as a literal.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
    /// How much of the model's context one result should take at most, and which part of a
    /// longer one is kept. `None` leaves it to whoever runs the call. See [`ResultBudget`].
    pub result: Option<ResultBudget>,
}

/// Which part of a tool's result is kept when it is too long for the model.
///
/// More ways may follow, so code that reads one needs an arm for the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Keep {
    /// The start: right for a file read from its top, or a list in order of interest.
    Head,
    /// The end: right for the output of a command, where the error comes last.
    Tail,
    /// The start and the end, with the middle left out. What is assumed of a tool that
    /// does not say.
    #[default]
    Ends,
}

impl Keep {
    /// The word for this in a tool's own description of itself (`result-keep` in the
    /// plugin world): `head`, `tail` or `ends`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Tail => "tail",
            Self::Ends => "ends",
        }
    }

    /// The way `word` names, if it names one.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "head" => Some(Self::Head),
            "tail" => Some(Self::Tail),
            "ends" => Some(Self::Ends),
            _ => None,
        }
    }
}

/// What a tool says about the size of its results: the most one should take of the model's
/// context, and which part of a longer one is kept.
///
/// The size is in tokens, since that is what a model's context is counted in, and a token
/// is taken to be [`BYTES_PER_TOKEN`] bytes of text: nobody but the model's own tokenizer
/// knows the real count, and the same bytes come to very different counts (prose near four
/// bytes to a token, a server's log nearer one and a half). Whoever runs the call cuts a
/// longer result, and may hold a call to less than its tool asked for when the model holds
/// little. A tool that would rather choose what to leave out asks what its call may return
/// ([`call::budget`](crate::call::budget)) and keeps to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResultBudget {
    /// The most one result should take, in tokens of [`BYTES_PER_TOKEN`] bytes.
    pub tokens: u32,
    /// Which part of a longer result is kept.
    pub keep: Keep,
}

/// How many bytes of text are counted as one token wherever a size is estimated.
pub const BYTES_PER_TOKEN: u32 = 3;

impl ResultBudget {
    /// At most `tokens` tokens of a result, keeping `keep` of a longer one.
    #[must_use]
    pub const fn new(tokens: u32, keep: Keep) -> Self {
        Self { tokens, keep }
    }

    /// The budget in bytes of text.
    #[must_use]
    pub const fn bytes(self) -> usize {
        (self.tokens as usize).saturating_mul(BYTES_PER_TOKEN as usize)
    }
}

impl ToolSpec {
    /// A tool called `name`, with what it does and the JSON Schema of its input object as
    /// JSON text. It is neither [replay-safe](Self::replay_safe()) nor
    /// [read-only](Self::read_only()) until said so.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: input_schema.into(),
            replay_safe: false,
            read_only: false,
            result: None,
        }
    }

    /// Say how much of the model's context one result should take at most, in tokens, and
    /// which part of a longer one is kept. See [`ResultBudget`].
    #[must_use]
    pub const fn result(mut self, tokens: u32, keep: Keep) -> Self {
        self.result = Some(ResultBudget::new(tokens, keep));
        self
    }

    /// Say whether calling the tool twice is the same as calling it once, so that a call
    /// nobody knows the fate of may simply be made again. See the field of the same name.
    #[must_use]
    pub const fn replay_safe(mut self, replay_safe: bool) -> Self {
        self.replay_safe = replay_safe;
        self
    }

    /// Say whether the tool only looks and never changes anything. See the field of the
    /// same name.
    #[must_use]
    pub const fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }
}

/// One block of a tool's result.
///
/// More kinds may follow, so code that takes blocks apart needs an arm for the rest. A
/// block's [`Display`](fmt::Display) form is the text that stands for it wherever only text
/// can go: text as it is, an image as a marker saying what it is and how large, such as
/// `[image: image/png, 1234 bytes]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Content {
    /// Text for the model to read.
    Text(String),
    /// An image for the model to look at.
    Image {
        /// What kind of image `data` is, such as `image/png`.
        mime_type: String,
        /// The image itself: its bytes as they would be in a file, not encoded as text.
        data: Vec<u8>,
    },
}

impl Content {
    /// A block of text.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// An image of the kind `mime_type` names, such as `image/png`, from its bytes.
    #[must_use]
    pub fn image(mime_type: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self::Image {
            mime_type: mime_type.into(),
            data: data.into(),
        }
    }
}

impl fmt::Display for Content {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => f.write_str(text),
            Self::Image { mime_type, data } => {
                write!(f, "[image: {mime_type}, {} bytes]", data.len())
            }
        }
    }
}

/// What a tool returned: [`Content`] blocks, in the order the model should see them.
///
/// Most tools return text, and text is a result as it is: a `String` or `&str` becomes a
/// result of one text block wherever a `ToolOutput` is wanted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolOutput {
    content: Vec<Content>,
}

impl ToolOutput {
    /// A result made of `content`, in that order.
    #[must_use]
    pub const fn new(content: Vec<Content>) -> Self {
        Self { content }
    }

    /// A result that is one block of text.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![Content::text(text)])
    }

    /// The blocks of the result, in order.
    #[must_use]
    pub fn content(&self) -> &[Content] {
        &self.content
    }

    /// The blocks of the result, in order, by value.
    #[must_use]
    pub fn into_content(self) -> Vec<Content> {
        self.content
    }

    /// The result as text, for wherever only text can go (a log line, a plugin that reads
    /// text): its blocks joined by newlines, an image standing as a marker such as
    /// `[image: image/png, 1234 bytes]`. The same as its [`Display`](fmt::Display) form.
    #[must_use]
    pub fn to_text(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for ToolOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, block) in self.content.iter().enumerate() {
            if position > 0 {
                f.write_str("\n")?;
            }
            block.fmt(f)?;
        }
        Ok(())
    }
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for ToolOutput {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

impl From<Content> for ToolOutput {
    fn from(block: Content) -> Self {
        Self::new(vec![block])
    }
}

impl From<Vec<Content>> for ToolOutput {
    fn from(content: Vec<Content>) -> Self {
        Self::new(content)
    }
}

impl FromIterator<Content> for ToolOutput {
    fn from_iter<I: IntoIterator<Item = Content>>(iter: I) -> Self {
        Self::new(iter.into_iter().collect())
    }
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
    fn call(&self, input: &str) -> Result<ToolOutput, ToolError>;
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
///
/// The function returns anything that is a [`ToolOutput`]: a `String` or `&str` for a result
/// of text, a [`Content`] block, a list of them, or a `ToolOutput` itself.
pub const fn tool_fn<F, O>(spec: ToolSpec, call: F) -> FnTool<F>
where
    F: Fn(&str) -> Result<O, ToolError> + Send + Sync,
    O: Into<ToolOutput>,
{
    FnTool { spec, call }
}

impl<F, O> Tool for FnTool<F>
where
    F: Fn(&str) -> Result<O, ToolError> + Send + Sync,
    O: Into<ToolOutput>,
{
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn call(&self, input: &str) -> Result<ToolOutput, ToolError> {
        (self.call)(input).map(Into::into)
    }
}

/// `schema`, a JSON Schema as JSON text, without the spaces and line ends between its
/// parts, so that two writings of one schema compare equal. What is inside a string is
/// kept as it is.
fn packed(schema: &str) -> String {
    let mut packed = String::with_capacity(schema.len());
    let (mut quoted, mut escaped) = (false, false);
    for character in schema.chars() {
        if quoted {
            quoted = escaped || character != '"';
            escaped = !escaped && character == '\\';
        } else if character.is_ascii_whitespace() {
            continue;
        } else {
            quoted = character == '"';
        }
        packed.push(character);
    }
    packed
}

/// How a tool declared as `new` differs from the one declared as `used`, for a caller that
/// took one for the other: in what it takes, or in whether a call may be made again. `Ok`
/// if in neither.
///
/// What a tool says of itself to the model, whether it only reads, and how much of a
/// result it keeps are its own, and may differ. So may what it returns, which no tool
/// declares.
fn differs(new: &ToolSpec, used: &ToolSpec) -> Result<(), String> {
    if packed(&new.input_schema) != packed(&used.input_schema) {
        return Err("its input schema is not the same as that of the tool it replaces".to_owned());
    }
    match (new.replay_safe, used.replay_safe) {
        (true, false) => Err(
            "it says a call may be made again after a crash (replay-safe), \
                              and the tool it replaces does not"
                .to_owned(),
        ),
        (false, true) => Err(
            "the tool it replaces says a call may be made again after a \
                              crash (replay-safe), and it does not"
                .to_owned(),
        ),
        _ => Ok(()),
    }
}

impl Registry<dyn Tool> {
    /// Put `tool` in the place of the tool registered under its name, on behalf of the
    /// plugin behind `ctx`: from now on a call of that name is a call of `tool`.
    ///
    /// It is done only for a tool that can be taken for the one it replaces: the same
    /// name, the same input schema (as JSON text, spaces between its parts aside) and the
    /// same word on whether a call may be made again after a crash. The tool it replaces
    /// stays registered beneath it, and is called again once the plugin behind `ctx` is
    /// unloaded.
    ///
    /// # Errors
    ///
    /// Returns [`NotReplaced::Missing`] if no tool of that name is registered, and
    /// [`NotReplaced::Differs`], saying how, if the two differ.
    pub fn stand_in(
        self: &Arc<Self>,
        ctx: &mut Context<'_>,
        tool: Arc<dyn Tool>,
    ) -> Result<(), NotReplaced> {
        let spec = tool.spec();
        self.replace(ctx, spec.name.clone(), tool, |used| {
            differs(&spec, &used.spec())
        })
    }

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
    pub fn call(&self, name: &str, input: &str) -> Result<ToolOutput, ToolError> {
        // The registry lock is released before the tool runs, so a slow tool blocks nobody.
        self.get(name)
            .ok_or_else(|| ToolError::UnknownTool(name.to_owned()))?
            .call(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shout() -> impl Tool {
        let spec = ToolSpec::new("shout", "Upper-cases its input.", "{}")
            .replay_safe(true)
            .read_only(true);
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

        assert_eq!(tools.call("shout", "hi"), Ok(ToolOutput::text("HI")));
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

    #[test]
    fn a_new_spec_claims_nothing_until_told() {
        let spec = ToolSpec::new("read", "Reads a file.", r#"{"type":"object"}"#);
        assert_eq!(spec.name, "read");
        assert_eq!(spec.description, "Reads a file.");
        assert_eq!(spec.input_schema, r#"{"type":"object"}"#);
        assert!(!spec.replay_safe && !spec.read_only);

        let read = spec.clone().read_only(true);
        assert!(read.read_only && !read.replay_safe);
        let both = read.replay_safe(true);
        assert!(both.read_only && both.replay_safe);
        // Each flag can be taken back, and owned strings are taken as they are.
        assert_eq!(both.replay_safe(false).read_only(false), spec);
        let owned = ToolSpec::new(
            String::from("read"),
            String::from("Reads a file."),
            spec.input_schema.clone(),
        );
        assert_eq!(owned, spec);
    }

    #[test]
    fn text_is_a_result_of_one_text_block() {
        let expected = [Content::Text("hi".to_owned())];
        assert_eq!(ToolOutput::text("hi").content(), expected);
        assert_eq!(ToolOutput::from("hi").content(), expected);
        assert_eq!(ToolOutput::from(String::from("hi")).content(), expected);
        assert_eq!(ToolOutput::from(Content::text("hi")).content(), expected);
        assert_eq!(ToolOutput::text("hi").to_text(), "hi");
        // No blocks is a result too, and not the same as an empty text.
        assert_eq!(ToolOutput::default().content(), []);
        assert_eq!(ToolOutput::default().to_text(), "");
        assert_ne!(ToolOutput::default(), ToolOutput::text(""));
    }

    #[test]
    fn blocks_keep_their_order_and_an_image_stands_as_a_marker_in_text() {
        let png = vec![0x89, b'P', b'N', b'G'];
        let output = ToolOutput::new(vec![
            Content::text("before"),
            Content::image("image/png", png.clone()),
            Content::text("after"),
        ]);
        assert_eq!(
            output.content().get(1),
            Some(&Content::Image {
                mime_type: "image/png".to_owned(),
                data: png.clone(),
            })
        );
        assert_eq!(
            output.to_text(),
            "before\n[image: image/png, 4 bytes]\nafter"
        );
        assert_eq!(output.to_string(), output.to_text());
        assert_eq!(
            Content::image("image/png", png).to_string(),
            "[image: image/png, 4 bytes]"
        );

        let collected: ToolOutput = output.content().iter().cloned().collect();
        assert_eq!(collected, output);
        assert_eq!(ToolOutput::from(output.clone().into_content()), output);
    }

    #[test]
    fn a_tool_built_from_a_closure_may_return_text_or_blocks() {
        let spec = |name: &str| ToolSpec::new(name, "", "{}");
        let text = tool_fn(spec("text"), |_| Ok("plain"));
        let block = tool_fn(spec("block"), |_| {
            Ok(Content::image("image/gif", *b"GIF89a"))
        });
        let blocks = tool_fn(spec("blocks"), |input| {
            Ok(vec![Content::text(input), Content::text("more")])
        });
        let output = tool_fn(spec("output"), |_| Ok(ToolOutput::default()));

        assert_eq!(text.call("{}"), Ok(ToolOutput::text("plain")));
        assert_eq!(
            block.call("{}").unwrap().to_text(),
            "[image: image/gif, 6 bytes]"
        );
        assert_eq!(blocks.call("some").unwrap().to_text(), "some\nmore");
        assert_eq!(output.call("{}").unwrap().content(), []);
    }

    #[test]
    fn a_tool_differs_from_another_in_what_it_takes_or_whether_it_is_repeated() {
        let schema = r#"{"type":"object","properties":{"command":{"type":"string"}}}"#;
        let used = ToolSpec::new("bash", "Runs a command.", schema);
        // Its own words for itself, its budget and how its schema is laid out are its own.
        let spaced =
            "{ \"type\": \"object\",\n \"properties\": {\"command\": {\"type\": \"string\"}} }";
        let same = ToolSpec::new("bash", "Runs a command, briefly.", spaced)
            .read_only(true)
            .result(100, Keep::Tail);
        assert_eq!(differs(&same, &used), Ok(()));

        let other = ToolSpec::new("bash", "Runs a command.", r#"{"type":"object"}"#);
        assert!(differs(&other, &used).unwrap_err().contains("input schema"));
        // A space inside a string is part of the schema.
        let renamed = ToolSpec::new("bash", "", schema.replace("command", "com mand"));
        assert!(differs(&renamed, &used).is_err());
        let repeated = ToolSpec::new("bash", "", schema).replay_safe(true);
        assert!(
            differs(&repeated, &used)
                .unwrap_err()
                .contains("replay-safe")
        );
        assert!(
            differs(&used, &repeated)
                .unwrap_err()
                .contains("replay-safe")
        );
        assert_eq!(
            packed(r#"{ "a b" : "c \" d" , "e":1 }"#),
            r#"{"a b":"c \" d","e":1}"#
        );
    }
}
