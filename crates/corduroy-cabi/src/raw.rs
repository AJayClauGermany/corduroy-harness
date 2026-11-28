//! Everything that crosses into C, and so everything that is `unsafe`.
//!
//! Each block says what it relies on, and each reliance is a rule of `include/corduroy.h`
//! that the extension's author promised to keep. None of it can be checked from here: an
//! extension that breaks a rule breaks the process.

use core::ffi::c_void;
use core::fmt;
use core::ptr;
use core::slice;
use std::error::Error;
use std::sync::{Arc, Mutex, PoisonError};

use corduroy_core::{
    CallError, Context, Draft, EventStream, GATES, Gate, Gates, PROMPTS, Plugin, PluginError,
    PromptPart, Prompts, SECRETS, SERVICES, Secret, Secrets, Service, Services, Source, TOOLS,
    ToolError, ToolSpec, Tools, Verdict, call, tool_fn,
};

use crate::abi::{self, Str};

#[cfg(feature = "harness")]
mod provider;

/// An extension's own pointer, handed back to it untouched.
#[derive(Clone, Copy)]
struct User(*mut c_void);

// SAFETY: rule 4 of the header: what `user` points to is the extension's to keep safe on any
// thread. This side never reads through it.
unsafe impl Send for User {}
// SAFETY: as above.
unsafe impl Sync for User {}

impl User {
    /// The pointer. A method, so that a closure using it captures the whole `User`.
    const fn get(self) -> *mut c_void {
        self.0
    }
}

/// A copy of `text`, or `None` if it is not UTF-8.
///
/// # Safety
///
/// `text` must point to `len` readable bytes, or have a `len` of zero.
unsafe fn copy(text: Str) -> Option<String> {
    if text.len == 0 {
        return Some(String::new());
    }
    if text.ptr.is_null() {
        return None;
    }
    // SAFETY: the caller's promise, and the pointer is not null.
    let bytes = unsafe { slice::from_raw_parts(text.ptr, text.len) };
    str::from_utf8(bytes).ok().map(str::to_owned)
}

/// A name the extension gave to something it registers.
///
/// # Safety
///
/// As [`copy`].
unsafe fn named(name: Str) -> Result<String, String> {
    // SAFETY: the caller's promise.
    match unsafe { copy(name) } {
        Some(name) if !name.is_empty() => Ok(name),
        Some(_) => Err("a name is empty".to_owned()),
        None => Err("a name is not UTF-8".to_owned()),
    }
}

/// A table the extension filled in, if it is at least as long as the one this host knows.
///
/// # Safety
///
/// `table` must be null, or point to a `T` laid out as the header says, whose first field
/// is its size.
unsafe fn sized<'table, T>(table: *const T) -> Result<&'table T, String> {
    if table.is_null() {
        return Err("a table is null".to_owned());
    }
    // SAFETY: every table starts with its size, a `u32`.
    let size = unsafe { table.cast::<u32>().read() };
    if usize::try_from(size).is_ok_and(|size| size >= size_of::<T>()) {
        // SAFETY: not null, and as long as a `T`.
        Ok(unsafe { &*table })
    } else {
        Err(format!(
            "a table of {size} bytes is shorter than this host's {}",
            size_of::<T>()
        ))
    }
}

/// Where a callback's answer is gathered: what a `corduroy_out` points to.
#[derive(Default)]
struct Sink {
    text: String,
    garbled: bool,
}

impl Sink {
    const fn out(&mut self) -> *mut abi::Out {
        ptr::from_mut(self).cast()
    }

    /// The status and the answer, or a failure if what was written was not text.
    fn finish(self, status: i32) -> (i32, String) {
        if self.garbled {
            (abi::FAILED, "the answer was not UTF-8".to_owned())
        } else {
            (status, self.text)
        }
    }
}

extern "C" fn write(out: *mut abi::Out, text: Str) {
    // SAFETY: an `out` is the `Sink` this module passed to the callback now running.
    let Some(sink) = (unsafe { out.cast::<Sink>().as_mut() }) else {
        return;
    };
    // SAFETY: rule 1: `text` is readable for this call.
    match unsafe { copy(text) } {
        Some(text) => sink.text.push_str(&text),
        None => sink.garbled = true,
    }
}

extern "C" fn stopping() -> i32 {
    i32::from(call::stopping())
}

/// What an extension keeps hold of after `apply`: what a `corduroy_handle` points to.
struct Shared {
    name: String,
    parts: Mutex<Option<Parts>>,
}

/// The parts of the app a handle reaches, as of the last `apply`.
#[derive(Clone)]
struct Parts {
    events: Arc<EventStream>,
    services: Option<Arc<Services>>,
}

impl Shared {
    fn parts(&self) -> Option<Parts> {
        self.parts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// An `apply` in progress: what a `corduroy_ctx` points to.
struct Applying<'ctx, 'app> {
    ctx: &'ctx mut Context<'app>,
    shared: &'ctx Arc<Shared>,
    /// Why the first registration that failed did.
    error: Option<String>,
}

/// Run `register` on the `apply` behind `ctx`, noting why if it fails.
fn applying(
    ctx: *mut abi::Ctx,
    register: impl FnOnce(&mut Applying<'_, '_>) -> Result<(), String>,
) -> i32 {
    // SAFETY: rule 3: a `ctx` is used inside `apply` only, where it is the `Applying` that
    // `CExtension::apply` made and lends to nothing else while the extension runs.
    let Some(applying) = (unsafe { ctx.cast::<Applying<'_, '_>>().as_mut() }) else {
        return abi::FAILED;
    };
    match register(applying) {
        Ok(()) => abi::OK,
        Err(why) => {
            applying.error.get_or_insert(why);
            abi::FAILED
        }
    }
}

extern "C" fn fail(ctx: *mut abi::Ctx, why: Str) {
    // SAFETY: rule 1.
    let why = unsafe { copy(why) }.unwrap_or_else(|| "it said why in bytes, not text".to_owned());
    let _status: i32 = applying(ctx, |_| Err(why));
}

extern "C" fn tool(ctx: *mut abi::Ctx, tool: *const abi::Tool) -> i32 {
    applying(ctx, |applying| {
        // SAFETY: the extension passes a `corduroy_tool` it filled in.
        let tool = unsafe { sized(tool) }?;
        let run = tool.call.ok_or("a tool has no function")?;
        let user = User(tool.user);
        // SAFETY: rule 1, for each of the three.
        let (name, description, schema) = unsafe {
            (
                named(tool.name)?,
                copy(tool.description).ok_or("a description is not UTF-8")?,
                copy(tool.input_schema).ok_or("a schema is not UTF-8")?,
            )
        };
        let spec = ToolSpec::new(name.clone(), description, schema)
            .replay_safe(tool.flags & abi::TOOL_REPLAY_SAFE != 0)
            .read_only(tool.flags & abi::TOOL_READ_ONLY != 0);
        let entry = tool_fn(spec, move |input: &str| {
            let mut sink = Sink::default();
            // SAFETY: the function and pointer the extension registered together; the
            // input and the sink outlive the call.
            let status = unsafe { run(user.get(), Str::of(input), sink.out()) };
            match sink.finish(status) {
                (abi::OK, text) => Ok(text),
                (abi::INVALID_INPUT, why) => Err(ToolError::InvalidInput(why)),
                (abi::DENIED, why) => Err(ToolError::Denied(why)),
                (_, why) => Err(ToolError::Failed(why)),
            }
        });
        let tools = applying.ctx.service::<Tools>(TOOLS).map_err(said)?;
        tools
            .register(applying.ctx, name, Arc::new(entry))
            .map_err(said)
    })
}

extern "C" fn service(
    ctx: *mut abi::Ctx,
    name: Str,
    run: Option<abi::ServiceFn>,
    user: *mut c_void,
) -> i32 {
    applying(ctx, |applying| {
        // SAFETY: rule 1.
        let name = unsafe { named(name) }?;
        let run = run.ok_or("a service has no function")?;
        let user = User(user);
        let entry = move |caller: &str, method: &str, payload: &str| {
            let mut sink = Sink::default();
            // SAFETY: as for a tool.
            let status = unsafe {
                run(
                    user.get(),
                    Str::of(caller),
                    Str::of(method),
                    Str::of(payload),
                    sink.out(),
                )
            };
            match sink.finish(status) {
                (abi::OK, text) => Ok(text),
                (abi::INVALID_INPUT, why) => Err(CallError::InvalidInput(why)),
                (abi::DENIED, why) => Err(CallError::Denied(why)),
                (abi::UNKNOWN, why) => Err(CallError::Unknown(why)),
                (_, why) => Err(CallError::Failed(why)),
            }
        };
        let entry: Arc<dyn Service> = Arc::new(entry);
        let services = applying.ctx.service::<Services>(SERVICES).map_err(said)?;
        services.register(applying.ctx, name, entry).map_err(said)
    })
}

extern "C" fn gate(
    ctx: *mut abi::Ctx,
    name: Str,
    check: Option<abi::GateFn>,
    user: *mut c_void,
) -> i32 {
    applying(ctx, |applying| {
        // SAFETY: rule 1.
        let name = unsafe { named(name) }?;
        let check = check.ok_or("a gate has no function")?;
        let user = User(user);
        let entry = move |point: &str, request: &str| {
            let mut sink = Sink::default();
            // SAFETY: as for a tool.
            let answer = unsafe { check(user.get(), Str::of(point), Str::of(request), sink.out()) };
            match sink.finish(answer) {
                (abi::NO_OPINION, _) => None,
                (abi::ALLOW, _) => Some(Verdict::Allow),
                (abi::ASK, why) => Some(Verdict::Ask(why)),
                (abi::DENY, why) => Some(Verdict::Deny(why)),
                // A gate that cannot answer denies.
                (other, _) => Some(Verdict::Deny(format!("the gate answered {other}"))),
            }
        };
        let entry: Arc<dyn Gate> = Arc::new(entry);
        let gates = applying.ctx.service::<Gates>(GATES).map_err(said)?;
        gates.register(applying.ctx, name, entry).map_err(said)
    })
}

/// Ask a prompt part or a secret for its text.
fn ask(render: abi::TextFn, user: User) -> (i32, String) {
    let mut sink = Sink::default();
    // SAFETY: as for a tool.
    let status = unsafe { render(user.get(), sink.out()) };
    sink.finish(status)
}

extern "C" fn prompt(
    ctx: *mut abi::Ctx,
    name: Str,
    render: Option<abi::TextFn>,
    user: *mut c_void,
) -> i32 {
    applying(ctx, |applying| {
        // SAFETY: rule 1.
        let name = unsafe { named(name) }?;
        let render = render.ok_or("a prompt part has no function")?;
        let user = User(user);
        let entry: Arc<dyn PromptPart> = Arc::new(move || match ask(render, user) {
            (abi::OK, text) => Some(text),
            _ => None,
        });
        let prompts = applying.ctx.service::<Prompts>(PROMPTS).map_err(said)?;
        prompts.register(applying.ctx, name, entry).map_err(said)
    })
}

extern "C" fn secret(
    ctx: *mut abi::Ctx,
    name: Str,
    reveal: Option<abi::TextFn>,
    user: *mut c_void,
) -> i32 {
    applying(ctx, |applying| {
        // SAFETY: rule 1.
        let name = unsafe { named(name) }?;
        let reveal = reveal.ok_or("a secret has no function")?;
        let user = User(user);
        let entry: Arc<dyn Secret> = Arc::new(move || match ask(reveal, user) {
            (abi::OK, value) => Ok(value),
            (_, why) => Err(why),
        });
        let secrets = applying.ctx.service::<Secrets>(SECRETS).map_err(said)?;
        secrets.register(applying.ctx, name, entry).map_err(said)
    })
}

extern "C" fn on(
    ctx: *mut abi::Ctx,
    topics: Str,
    listener: Option<abi::ListenFn>,
    user: *mut c_void,
) -> i32 {
    applying(ctx, |applying| {
        // SAFETY: rule 1.
        let topics = unsafe { named(topics) }?;
        let listener = listener.ok_or("a listener has no function")?;
        let user = User(user);
        applying.ctx.on(topics, move |event| {
            let source = event.source.to_string();
            // SAFETY: as for a tool; the three texts outlive the call.
            unsafe {
                listener(
                    user.get(),
                    Str::of(&event.topic),
                    Str::of(&source),
                    Str::of(&event.payload),
                );
            }
        });
        Ok(())
    })
}

extern "C" fn effect(
    ctx: *mut abi::Ctx,
    dispose: Option<abi::DisposeFn>,
    user: *mut c_void,
) -> i32 {
    applying(ctx, |applying| {
        let dispose = dispose.ok_or("an effect has no function")?;
        let user = User(user);
        applying.ctx.effect(move || {
            // SAFETY: the function and pointer the extension registered together.
            unsafe { dispose(user.get()) }
        });
        Ok(())
    })
}

extern "C" fn handle(ctx: *mut abi::Ctx) -> *const abi::Handle {
    let mut handle = ptr::null();
    let _status: i32 = applying(ctx, |applying| {
        handle = Arc::as_ptr(applying.shared).cast();
        Ok(())
    });
    handle
}

/// The `Shared` behind `handle`.
///
/// # Safety
///
/// `handle` must be null, or one [`handle`] returned to an extension that has not been
/// dropped.
const unsafe fn shared<'handle>(handle: *const abi::Handle) -> Option<&'handle Shared> {
    // SAFETY: the caller's promise: the `CExtension` holds the `Arc` until it is dropped.
    unsafe { handle.cast::<Shared>().as_ref() }
}

extern "C" fn emit(handle: *const abi::Handle, topic: Str, payload: Str) -> i32 {
    // SAFETY: the header: a handle is used until `drop` and no later.
    let Some(shared) = (unsafe { shared(handle) }) else {
        return abi::FAILED;
    };
    // SAFETY: rule 1.
    let (Some(topic), Some(payload)) = (unsafe { (copy(topic), copy(payload)) }) else {
        return abi::INVALID_INPUT;
    };
    let Some(parts) = shared.parts() else {
        return abi::FAILED;
    };
    let draft = Draft::new(Source::Plugin(shared.name.clone()), topic).payload(payload);
    match parts.events.emit(draft) {
        Ok(_) => abi::OK,
        Err(_) => abi::FAILED,
    }
}

extern "C" fn call_service(
    handle: *const abi::Handle,
    service: Str,
    method: Str,
    payload: Str,
    receive: Option<abi::ReceiveFn>,
    into: *mut c_void,
) -> i32 {
    // SAFETY: as for `emit`.
    let Some(shared) = (unsafe { shared(handle) }) else {
        return abi::FAILED;
    };
    // SAFETY: rule 1.
    let texts = unsafe { (copy(service), copy(method), copy(payload)) };
    let (status, text) = match (texts, shared.parts().and_then(|parts| parts.services)) {
        ((Some(service), Some(method), Some(payload)), Some(services)) => {
            match services.call(&shared.name, &service, &method, &payload) {
                Ok(answer) => (abi::OK, answer),
                Err(CallError::InvalidInput(why)) => (abi::INVALID_INPUT, why),
                Err(CallError::Denied(why)) => (abi::DENIED, why),
                Err(CallError::Unknown(why)) => (abi::UNKNOWN, why),
                Err(CallError::Failed(why)) => (abi::FAILED, why),
            }
        }
        (_, None) => (
            abi::FAILED,
            "the extension did not list `services` in its inject".to_owned(),
        ),
        _ => (abi::INVALID_INPUT, "a text is not UTF-8".to_owned()),
    };
    if let Some(receive) = receive {
        // SAFETY: the function and pointer the extension passed together; the text
        // outlives the call.
        unsafe { receive(into, Str::of(&text)) };
    }
    status
}

/// The size of a table, as its first field holds it.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a table of a few dozen pointers"
)]
const fn size<T>() -> u32 {
    size_of::<T>() as u32
}

/// What every extension is handed.
static HOST: abi::Host = abi::Host {
    abi: abi::ABI,
    size: size::<abi::Host>(),
    write,
    stopping,
    fail,
    tool,
    service,
    gate,
    prompt,
    secret,
    on,
    effect,
    handle,
    emit,
    call: call_service,
    #[cfg(feature = "harness")]
    provider: Some(provider::provider),
    #[cfg(feature = "harness")]
    complete: Some(provider::complete),
    #[cfg(not(feature = "harness"))]
    provider: None,
    #[cfg(not(feature = "harness"))]
    complete: None,
};

/// An error's text.
fn said(error: impl fmt::Display) -> String {
    error.to_string()
}

/// Why an extension could not be taken in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError(String);

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the extension could not be loaded: {}", self.0)
    }
}

impl Error for LoadError {}

/// An extension written against `include/corduroy.h`, as a [`Plugin`].
///
/// It is a native plugin like any other: what the foreign code registers goes through this
/// plugin's [`Context`], so unloading it takes all of it back.
pub struct CExtension {
    name: String,
    inject: Vec<String>,
    user: User,
    apply: abi::ApplyFn,
    drop: Option<abi::DisposeFn>,
    shared: Arc<Shared>,
}

impl fmt::Debug for CExtension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CExtension")
            .field("name", &self.name)
            .field("inject", &self.inject)
            .finish_non_exhaustive()
    }
}

impl CExtension {
    /// Ask `entry` for its extension.
    ///
    /// # Errors
    ///
    /// Returns a [`LoadError`] if the entry function refuses this host, or describes an
    /// extension with no name or no `apply`.
    ///
    /// # Safety
    ///
    /// `entry`, and every function the extension hands over, must keep the rules at the top
    /// of `include/corduroy.h`. Nothing checks that they do, and one that does not can
    /// corrupt the process: this is the host's whole authority, given to foreign code.
    pub unsafe fn load(entry: abi::Entry) -> Result<Self, LoadError> {
        let mut described = abi::Extension {
            abi: abi::ABI,
            size: size::<abi::Extension>(),
            name: Str::EMPTY,
            inject: ptr::null(),
            inject_len: 0,
            user: ptr::null_mut(),
            apply: None,
            drop: None,
        };
        // SAFETY: the caller's promise; `described` is a table this host laid out.
        let status = unsafe { entry(&raw mut described) };
        if status != abi::OK {
            return Err(LoadError(format!("its entry function returned {status}")));
        }
        // SAFETY: the entry function has just returned, so what it described is readable.
        let name = unsafe { named(described.name) }.map_err(LoadError)?;
        let listed = if described.inject.is_null() {
            &[]
        } else {
            // SAFETY: `inject` points to `inject_len` strings, by the header.
            unsafe { slice::from_raw_parts(described.inject, described.inject_len) }
        };
        let inject = listed
            .iter()
            // SAFETY: as for the name.
            .map(|service| unsafe { named(*service) })
            .collect::<Result<Vec<_>, _>>()
            .map_err(LoadError)?;
        let apply = described
            .apply
            .ok_or_else(|| LoadError("it has no `apply`".to_owned()))?;
        Ok(Self {
            shared: Arc::new(Shared {
                name: name.clone(),
                parts: Mutex::new(None),
            }),
            name,
            inject,
            user: User(described.user),
            apply,
            drop: described.drop,
        })
    }
}

impl Plugin for CExtension {
    fn name(&self) -> &str {
        &self.name
    }

    fn inject(&self) -> Vec<String> {
        self.inject.clone()
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        let parts = Parts {
            events: ctx.events(),
            services: ctx.service::<Services>(SERVICES).ok(),
        };
        *self
            .shared
            .parts
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(parts);
        let mut applying = Applying {
            ctx,
            shared: &self.shared,
            error: None,
        };
        // SAFETY: the function and pointer `load` was given; the host table lasts as long
        // as the program, and `applying` outlives the call.
        let status = unsafe {
            (self.apply)(
                self.user.get(),
                &raw const HOST,
                ptr::from_mut(&mut applying).cast(),
            )
        };
        match (status, applying.error) {
            (abi::OK, None) => Ok(()),
            // A registration that failed fails `apply`, whether or not the extension looked.
            (_, Some(why)) => Err(PluginError::new(why)),
            (status, None) => Err(PluginError::new(format!("its `apply` returned {status}"))),
        }
    }
}

impl Drop for CExtension {
    fn drop(&mut self) {
        if let Some(drop) = self.drop {
            // SAFETY: the function and pointer `load` was given, called once.
            unsafe { drop(self.user.get()) };
        }
    }
}
