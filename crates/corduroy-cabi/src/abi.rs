//! The interface as C lays it out: the mirror of `include/corduroy.h`, field for field.
//!
//! Nothing here acts. These are the shapes that cross; `raw` is what reads and calls them.

use core::ffi::c_void;
use core::ptr;

/// The version of the interface this crate speaks.
pub const ABI: u32 = 0;

/// The call went well; what was written is the answer.
pub const OK: i32 = 0;
/// The input was refused.
pub const INVALID_INPUT: i32 = 1;
/// The call could not complete.
pub const FAILED: i32 = 2;
/// The caller may not make this call.
pub const DENIED: i32 = 3;
/// Nothing answers to that name.
pub const UNKNOWN: i32 = 4;
/// Nothing to say this time.
pub const NONE: i32 = 5;

/// A gate has no opinion.
pub const NO_OPINION: i32 = 0;
/// A gate allows.
pub const ALLOW: i32 = 1;
/// A gate wants a person asked.
pub const ASK: i32 = 2;
/// A gate denies.
pub const DENY: i32 = 3;

/// [`Tool::flags`]: calling twice is the same as calling once.
pub const TOOL_REPLAY_SAFE: u32 = 1;
/// [`Tool::flags`]: the tool only looks.
pub const TOOL_READ_ONLY: u32 = 2;

/// Text: UTF-8, borrowed for the length of the call it is passed to.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Str {
    /// The first byte; may be null when `len` is zero.
    pub ptr: *const u8,
    /// How many bytes.
    pub len: usize,
}

impl Str {
    /// No text.
    pub const EMPTY: Self = Self {
        ptr: ptr::null(),
        len: 0,
    };

    /// `text`, for as long as it is borrowed.
    #[must_use]
    pub const fn of(text: &str) -> Self {
        Self {
            ptr: text.as_ptr(),
            len: text.len(),
        }
    }
}

/// Where a callback writes its answer. Opaque to C.
#[repr(C)]
#[derive(Debug)]
pub struct Out {
    opaque: [u8; 0],
}

/// The extension's context inside `apply`. Opaque to C.
#[repr(C)]
#[derive(Debug)]
pub struct Ctx {
    opaque: [u8; 0],
}

/// The extension's hold on the harness after `apply`. Opaque to C.
#[repr(C)]
#[derive(Debug)]
pub struct Handle {
    opaque: [u8; 0],
}

/// An answer still owed. Opaque to C.
#[repr(C)]
#[derive(Debug)]
pub struct Completion {
    opaque: [u8; 0],
}

/// A tool's call.
pub type ToolFn = unsafe extern "C" fn(user: *mut c_void, input: Str, out: *mut Out) -> i32;
/// A service's call.
pub type ServiceFn = unsafe extern "C" fn(
    user: *mut c_void,
    caller: Str,
    method: Str,
    payload: Str,
    out: *mut Out,
) -> i32;
/// A gate's check.
pub type GateFn =
    unsafe extern "C" fn(user: *mut c_void, point: Str, request: Str, reason: *mut Out) -> i32;
/// A prompt part's text, or a secret's value.
pub type TextFn = unsafe extern "C" fn(user: *mut c_void, out: *mut Out) -> i32;
/// A listener.
pub type ListenFn = unsafe extern "C" fn(user: *mut c_void, topic: Str, source: Str, payload: Str);
/// What undoes an effect, and what lets go of an extension.
pub type DisposeFn = unsafe extern "C" fn(user: *mut c_void);
/// How an extension is handed text the host produced.
pub type ReceiveFn = unsafe extern "C" fn(into: *mut c_void, text: Str);
/// A provider's start of an answer.
pub type CompleteFn = unsafe extern "C" fn(user: *mut c_void, request: Str, done: *mut Completion);
/// An extension's `apply`.
pub type ApplyFn = unsafe extern "C" fn(user: *mut c_void, host: *const Host, ctx: *mut Ctx) -> i32;
/// The one function an extension exports.
pub type Entry = unsafe extern "C" fn(extension: *mut Extension) -> i32;

/// A tool, as an extension describes it.
#[repr(C)]
#[derive(Debug)]
pub struct Tool {
    /// The size of this table as the extension knows it.
    pub size: u32,
    /// [`TOOL_REPLAY_SAFE`] and [`TOOL_READ_ONLY`].
    pub flags: u32,
    /// The name the model calls it by.
    pub name: Str,
    /// What it does.
    pub description: Str,
    /// The JSON Schema of its input, as JSON text.
    pub input_schema: Str,
    /// Handed back to `call`.
    pub user: *mut c_void,
    /// The call.
    pub call: Option<ToolFn>,
}

/// A model provider, as an extension describes it.
#[repr(C)]
#[derive(Debug)]
pub struct Provider {
    /// The size of this table as the extension knows it.
    pub size: u32,
    /// Unused; zero.
    pub reserved: u32,
    /// Handed back to `complete`.
    pub user: *mut c_void,
    /// Start answering a request.
    pub complete: Option<CompleteFn>,
}

/// What the host offers an extension. See `include/corduroy.h` for what each does.
#[repr(C)]
#[derive(Debug)]
pub struct Host {
    /// [`ABI`].
    pub abi: u32,
    /// The size of this table as the host knows it.
    pub size: u32,
    /// Add text to an answer.
    pub write: extern "C" fn(out: *mut Out, text: Str),
    /// Whether the run this thread serves was asked to stop.
    pub stopping: extern "C" fn() -> i32,
    /// Say why `apply` is about to fail.
    pub fail: extern "C" fn(ctx: *mut Ctx, why: Str),
    /// Register a tool.
    pub tool: extern "C" fn(ctx: *mut Ctx, tool: *const Tool) -> i32,
    /// Provide a service other plugins call.
    pub service:
        extern "C" fn(ctx: *mut Ctx, name: Str, call: Option<ServiceFn>, user: *mut c_void) -> i32,
    /// Register a gate.
    pub gate:
        extern "C" fn(ctx: *mut Ctx, name: Str, check: Option<GateFn>, user: *mut c_void) -> i32,
    /// Register a part of the system prompt.
    pub prompt:
        extern "C" fn(ctx: *mut Ctx, name: Str, render: Option<TextFn>, user: *mut c_void) -> i32,
    /// Register a secret.
    pub secret:
        extern "C" fn(ctx: *mut Ctx, name: Str, reveal: Option<TextFn>, user: *mut c_void) -> i32,
    /// Listen for events.
    pub on: extern "C" fn(
        ctx: *mut Ctx,
        topics: Str,
        listener: Option<ListenFn>,
        user: *mut c_void,
    ) -> i32,
    /// Record what undoes something the extension did.
    pub effect: extern "C" fn(ctx: *mut Ctx, dispose: Option<DisposeFn>, user: *mut c_void) -> i32,
    /// The extension's hold on the harness after `apply`.
    pub handle: extern "C" fn(ctx: *mut Ctx) -> *const Handle,
    /// Emit an event with the extension as its source.
    pub emit: extern "C" fn(handle: *const Handle, topic: Str, payload: Str) -> i32,
    /// Call another plugin's service.
    pub call: extern "C" fn(
        handle: *const Handle,
        service: Str,
        method: Str,
        payload: Str,
        receive: Option<ReceiveFn>,
        into: *mut c_void,
    ) -> i32,
    /// Register a model provider. Null in a host built without the loop.
    pub provider: Option<extern "C" fn(ctx: *mut Ctx, name: Str, provider: *const Provider) -> i32>,
    /// Complete an answer. Null in a host built without the loop.
    pub complete: Option<extern "C" fn(done: *mut Completion, status: i32, text: Str)>,
}

/// An extension, as its entry function describes it.
#[repr(C)]
#[derive(Debug)]
pub struct Extension {
    /// The version the host speaks; set by the host.
    pub abi: u32,
    /// The size of this table as the host knows it; set by the host.
    pub size: u32,
    /// The extension's name.
    pub name: Str,
    /// The services `apply` uses.
    pub inject: *const Str,
    /// How many.
    pub inject_len: usize,
    /// Handed back to `apply` and `drop`.
    pub user: *mut c_void,
    /// Set the extension up.
    pub apply: Option<ApplyFn>,
    /// Let go of the extension.
    pub drop: Option<DisposeFn>,
}
