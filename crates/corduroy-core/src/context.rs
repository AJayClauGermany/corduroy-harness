//! Plugin contexts with spatio-temporal composability, after Cordis.
//!
//! An [`App`] is a tree of *fibers*, one per loaded [`Plugin`]. A plugin never touches global
//! state directly: everything it sets up goes through the [`Context`] it is handed, which
//! records each registration as an *effect* owned by the plugin's fiber.
//!
//! * **Temporal composability**: unloading a plugin runs its effects' disposers in reverse
//!   order, so a plugin's side effects are completely reverted when it goes away. A plugin
//!   whose `apply` fails is rolled back the same way.
//! * **Spatial composability**: a plugin names the services it needs in
//!   [`Plugin::inject`]. It stays [`FiberState::Pending`] until all of them exist, is suspended
//!   (effects reverted) when one disappears, and is applied again when it returns. Dependents
//!   are always unloaded before the service they depend on.
//!
//! **A name belongs to one plugin at a time.** Events, a plugin's stored state and calls
//! between plugins all identify a plugin by its name, so two loaded plugins never share one:
//! the plugin loaded first holds the name until it is disposed, and a later plugin of the
//! same name is not applied ([`FiberState::Failed`]) until then. The names of the other
//! event sources ([`RESERVED_NAMES`]) and the empty name are never a plugin's.
//!
//! The tree is changed through `&mut App`. Code that cannot have that (another thread, or a
//! plugin's call while the tree is busy elsewhere) asks for a change through [`Requests`]; it
//! is applied the next time the tree settles, and in the meantime the new plugin already has
//! its [`FiberId`].

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::error::Error;
use core::fmt;
use core::mem;

use crate::event::{Draft, Event, EventId, EventStream, ListenerId, LogError, Source};
use crate::json;
use crate::pattern::Pattern;
use crate::sync::Lock;

/// Names no plugin may have: they are how the other sources of events, and the harness as
/// a caller of services, are written.
pub const RESERVED_NAMES: [&str; 4] = ["host", "agent", "user", "environment"];

/// Identifies one loaded plugin instance within an [`App`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FiberId(u64);

impl fmt::Display for FiberId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fiber {}", self.0)
    }
}

/// Where a plugin instance is in its lifecycle. A disposed plugin has no state at all.
///
/// More states may be added, so match with a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FiberState {
    /// Waiting for an injected service, or for its parent to become active.
    Pending,
    /// Applied; its effects are in place.
    Active,
    /// `apply` returned an error. Its effects were rolled back. [`App::reload`] retries it.
    Failed(String),
}

/// A unit of functionality that can be loaded into, and cleanly removed from, an [`App`].
pub trait Plugin: Send + Sync {
    /// Name used in events and diagnostics.
    fn name(&self) -> &str;

    /// Names of the services this plugin needs. It is applied only while all of them exist.
    fn inject(&self) -> Vec<String> {
        Vec::new()
    }

    /// Set the plugin up. Called each time the plugin becomes active, so it may run more than
    /// once over the plugin's life.
    ///
    /// # Errors
    ///
    /// Returns a [`PluginError`] if the plugin cannot start. Everything it registered through
    /// `ctx` before failing is reverted.
    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError>;
}

/// A boxed plugin is a plugin, so code that chooses a plugin at run time (from a
/// configuration file, say) can hand over whichever it built.
impl<P: Plugin + ?Sized> Plugin for Box<P> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn inject(&self) -> Vec<String> {
        (**self).inject()
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        (**self).apply(ctx)
    }
}

/// A [`Plugin`] built from a closure. See [`plugin_fn`].
pub struct FnPlugin<F> {
    name: String,
    inject: Vec<String>,
    apply: F,
}

impl<F> fmt::Debug for FnPlugin<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnPlugin")
            .field("name", &self.name)
            .field("inject", &self.inject)
            .finish_non_exhaustive()
    }
}

/// Build a [`Plugin`] from a name, the services it injects and an `apply` closure.
pub fn plugin_fn<F>(name: impl Into<String>, inject: &[&str], apply: F) -> FnPlugin<F>
where
    F: FnMut(&mut Context<'_>) -> Result<(), PluginError> + Send + Sync,
{
    FnPlugin {
        name: name.into(),
        inject: inject.iter().map(|&service| service.to_owned()).collect(),
        apply,
    }
}

impl<F> Plugin for FnPlugin<F>
where
    F: FnMut(&mut Context<'_>) -> Result<(), PluginError> + Send + Sync,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn inject(&self) -> Vec<String> {
        self.inject.clone()
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        (self.apply)(ctx)
    }
}

/// Why a plugin could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginError(String);

impl PluginError {
    /// An error with a human-readable description.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for PluginError {}

impl From<LogError> for PluginError {
    fn from(error: LogError) -> Self {
        Self(error.to_string())
    }
}

impl From<ContextError> for PluginError {
    fn from(error: ContextError) -> Self {
        Self(error.to_string())
    }
}

/// A misuse of an [`App`] or [`Context`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextError {
    /// No plugin instance has this id; it may already have been disposed.
    UnknownFiber(FiberId),
    /// Another plugin already provides a service with this name.
    ServiceTaken(String),
    /// No plugin provides a service with this name.
    ServiceMissing(String),
    /// The service exists but is not of the requested type.
    ServiceType(String),
    /// The plugin asked for a service it did not list in [`Plugin::inject`].
    NotInjected {
        /// The plugin that asked.
        plugin: String,
        /// The service it asked for.
        service: String,
    },
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFiber(id) => write!(f, "{id} does not exist"),
            Self::ServiceTaken(name) => write!(f, "service `{name}` is already provided"),
            Self::ServiceMissing(name) => write!(f, "service `{name}` is not provided"),
            Self::ServiceType(name) => write!(f, "service `{name}` has a different type"),
            Self::NotInjected { plugin, service } => write!(
                f,
                "plugin `{plugin}` uses service `{service}` without listing it in `inject`"
            ),
        }
    }
}

impl Error for ContextError {}

/// A snapshot of one plugin instance, from [`App::fibers`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FiberInfo {
    /// The instance's id.
    pub id: FiberId,
    /// The plugin's name.
    pub name: String,
    /// The instance that loaded this one, if it is not a top-level plugin.
    pub parent: Option<FiberId>,
    /// Where the instance is in its lifecycle.
    pub state: FiberState,
}

enum Effect {
    Callback(Box<dyn FnOnce() + Send + Sync>),
    Service(String),
    Listener(ListenerId),
    Child(FiberId),
}

struct Fiber {
    name: String,
    inject: Vec<String>,
    parent: Option<FiberId>,
    /// Taken out while the plugin is being applied.
    plugin: Option<Box<dyn Plugin>>,
    state: FiberState,
    effects: Vec<Effect>,
}

type AnyService = Arc<dyn Any + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unload {
    /// Revert effects and wait to be applied again.
    Suspend,
    /// Revert effects and forget the plugin.
    Dispose,
}

/// The root of a plugin tree: owns every fiber, every service and the event stream.
pub struct App {
    fibers: BTreeMap<FiberId, Fiber>,
    services: BTreeMap<String, AnyService>,
    events: Arc<EventStream>,
    requests: Requests,
}

/// A change to the plugin tree, asked for through [`Requests`].
enum Request {
    Load {
        id: FiberId,
        parent: Option<FiberId>,
        plugin: Box<dyn Plugin>,
    },
    Dispose(FiberId),
}

/// A handle for asking an [`App`] to change from anywhere.
///
/// Another thread, or a plugin that is running, cannot have `&mut App`. Cheap to clone.
/// Changes are applied in the order asked, the next time the app settles (any call that
/// changes it, or [`App::apply_requests`]).
#[derive(Clone, Default)]
pub struct Requests {
    queue: Arc<Lock<Vec<Request>>>,
    /// Shared with the app, so that a plugin asked for has its id at once.
    next_fiber: Arc<Lock<u64>>,
}

impl fmt::Debug for Requests {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Requests")
            .field("pending", &self.pending())
            .finish_non_exhaustive()
    }
}

impl Requests {
    /// Ask for `plugin` to be loaded, as a child of `parent` if given (disposed with it), and
    /// return the id it will have. If `parent` is gone by the time the request is applied,
    /// nothing is loaded and `plugin/dropped` is emitted.
    pub fn load(&self, parent: Option<FiberId>, plugin: impl Plugin + 'static) -> FiberId {
        let id = self.fiber_id();
        self.push(Request::Load {
            id,
            parent,
            plugin: Box::new(plugin),
        });
        id
    }

    /// Ask for the plugin `id` to be disposed. Unknown ids are ignored.
    pub fn dispose(&self, id: FiberId) {
        self.push(Request::Dispose(id));
    }

    /// How many requests wait to be applied.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.with(|queue| queue.len())
    }

    fn fiber_id(&self) -> FiberId {
        self.next_fiber.with(|next| {
            *next += 1;
            FiberId(*next)
        })
    }

    fn push(&self, request: Request) {
        self.queue.with(|queue| queue.push(request));
    }

    fn take(&self) -> Vec<Request> {
        self.queue.with(mem::take)
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("App")
            .field("fibers", &self.fibers())
            .field("services", &self.services.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl App {
    /// An app with no plugins and a fresh event stream.
    #[must_use]
    pub fn new() -> Self {
        Self::with_events(Arc::new(EventStream::new()))
    }

    /// An app that records to an existing event stream, for example one loaded from a log.
    #[must_use]
    pub fn with_events(events: Arc<EventStream>) -> Self {
        Self {
            fibers: BTreeMap::new(),
            services: BTreeMap::new(),
            events,
            requests: Requests::default(),
        }
    }

    /// A handle for asking this app to change from where `&mut App` cannot be had.
    #[must_use]
    pub fn requests(&self) -> Requests {
        self.requests.clone()
    }

    /// Apply the changes asked for through [`Requests`], and anything they make ready.
    /// Returns whether there were any.
    pub fn apply_requests(&mut self) -> bool {
        let applied = self.requests.pending() > 0;
        self.settle();
        applied
    }

    /// The stream every plugin in this app emits to and listens on.
    #[must_use]
    pub const fn events(&self) -> &Arc<EventStream> {
        &self.events
    }

    /// Load a top-level plugin. It is applied at once if its injected services exist, and
    /// otherwise waits as [`FiberState::Pending`].
    pub fn plugin(&mut self, plugin: impl Plugin + 'static) -> FiberId {
        let id = self.add_fiber(None, Box::new(plugin));
        self.settle();
        id
    }

    /// Provide a service owned by the host rather than by a plugin. It is never removed.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::ServiceTaken`] if the name is in use.
    pub fn provide<T: Any + Send + Sync>(
        &mut self,
        name: impl Into<String>,
        service: Arc<T>,
    ) -> Result<(), ContextError> {
        self.insert_service(name.into(), service)?;
        self.settle();
        Ok(())
    }

    /// Look a service up by name and type.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::ServiceMissing`] or [`ContextError::ServiceType`].
    pub fn service<T: Any + Send + Sync>(&self, name: &str) -> Result<Arc<T>, ContextError> {
        let service = self
            .services
            .get(name)
            .ok_or_else(|| ContextError::ServiceMissing(name.to_owned()))?;
        Arc::clone(service)
            .downcast::<T>()
            .map_err(|_| ContextError::ServiceType(name.to_owned()))
    }

    /// Unload a plugin for good: its dependents are suspended, its children disposed and its
    /// effects reverted in reverse order.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::UnknownFiber`] if `id` is not loaded.
    pub fn dispose(&mut self, id: FiberId) -> Result<(), ContextError> {
        self.fiber(id)?;
        self.forget(id);
        self.settle();
        Ok(())
    }

    /// Revert a plugin's effects and apply it again, as a hot reload does. Also retries a
    /// plugin that [`FiberState::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::UnknownFiber`] if `id` is not loaded.
    pub fn reload(&mut self, id: FiberId) -> Result<(), ContextError> {
        self.fiber(id)?;
        self.unload(id, Unload::Suspend);
        if let Some(fiber) = self.fibers.get_mut(&id) {
            fiber.state = FiberState::Pending;
        }
        self.settle();
        Ok(())
    }

    /// The lifecycle state of a plugin instance, or `None` once it has been disposed.
    #[must_use]
    pub fn state(&self, id: FiberId) -> Option<&FiberState> {
        self.fibers.get(&id).map(|fiber| &fiber.state)
    }

    /// Every loaded plugin instance, in load order.
    #[must_use]
    pub fn fibers(&self) -> Vec<FiberInfo> {
        self.fibers
            .iter()
            .map(|(&id, fiber)| FiberInfo {
                id,
                name: fiber.name.clone(),
                parent: fiber.parent,
                state: fiber.state.clone(),
            })
            .collect()
    }

    fn fiber(&self, id: FiberId) -> Result<&Fiber, ContextError> {
        self.fibers.get(&id).ok_or(ContextError::UnknownFiber(id))
    }

    fn add_fiber(&mut self, parent: Option<FiberId>, plugin: Box<dyn Plugin>) -> FiberId {
        let id = self.requests.fiber_id();
        self.insert_fiber(id, parent, plugin);
        id
    }

    fn insert_fiber(&mut self, id: FiberId, parent: Option<FiberId>, plugin: Box<dyn Plugin>) {
        self.fibers.insert(
            id,
            Fiber {
                name: plugin.name().to_owned(),
                inject: plugin.inject(),
                parent,
                plugin: Some(plugin),
                state: FiberState::Pending,
                effects: Vec::new(),
            },
        );
    }

    /// Apply the requests waiting in the queue, in order.
    fn take_requests(&mut self) {
        for request in self.requests.take() {
            match request {
                Request::Load { id, parent, plugin } => self.adopt(id, parent, plugin),
                Request::Dispose(id) => self.forget(id),
            }
        }
    }

    /// Add a requested plugin under `parent`, unless `parent` is gone.
    fn adopt(&mut self, id: FiberId, parent: Option<FiberId>, plugin: Box<dyn Plugin>) {
        let Some(parent) = parent else {
            self.insert_fiber(id, None, plugin);
            return;
        };
        let Some(owner) = self.fibers.get_mut(&parent) else {
            let payload = format!(
                "{{\"plugin\":{},\"reason\":\"its parent was unloaded first\"}}",
                json::string(plugin.name())
            );
            drop(
                self.events
                    .emit(Draft::new(Source::Host, "plugin/dropped").payload(payload)),
            );
            return;
        };
        owner.effects.push(Effect::Child(id));
        self.insert_fiber(id, Some(parent), plugin);
    }

    /// Unload `id` for good and remove it from its parent's effects. Unknown ids are ignored.
    fn forget(&mut self, id: FiberId) {
        let Some(fiber) = self.fibers.get(&id) else {
            return;
        };
        let parent = fiber.parent;
        self.unload(id, Unload::Dispose);
        if let Some(parent) = parent.and_then(|parent| self.fibers.get_mut(&parent)) {
            parent
                .effects
                .retain(|effect| !matches!(effect, Effect::Child(child) if *child == id));
        }
    }

    fn insert_service(&mut self, name: String, service: AnyService) -> Result<(), ContextError> {
        if self.services.contains_key(&name) {
            return Err(ContextError::ServiceTaken(name));
        }
        self.services.insert(name, service);
        Ok(())
    }

    /// Apply every pending plugin whose requirements are now met, until none is left.
    fn settle(&mut self) {
        loop {
            self.take_requests();
            let Some(id) = self.next_ready() else {
                break;
            };
            self.activate(id);
        }
    }

    fn next_ready(&self) -> Option<FiberId> {
        self.fibers
            .iter()
            .find(|(_, fiber)| {
                fiber.state == FiberState::Pending
                    && fiber.plugin.is_some()
                    && fiber
                        .inject
                        .iter()
                        .all(|service| self.services.contains_key(service))
                    && fiber
                        .parent
                        .is_none_or(|parent| self.state(parent) == Some(&FiberState::Active))
            })
            .map(|(&id, _)| id)
    }

    /// Why the fiber `id` may not go by its name, if it may not: the name is empty or
    /// reserved, or a plugin loaded earlier holds it.
    fn name_refused(&self, id: FiberId) -> Option<String> {
        let name = self.fibers.get(&id)?.name.as_str();
        if name.is_empty() {
            return Some("a plugin needs a name".to_owned());
        }
        if RESERVED_NAMES.contains(&name) {
            return Some(format!("`{name}` is reserved and cannot name a plugin"));
        }
        // Ids are handed out in the order plugins were loaded, so the earliest holds the name.
        self.fibers
            .range(..id)
            .any(|(_, earlier)| earlier.name == name)
            .then(|| format!("another plugin named `{name}` is already loaded"))
    }

    fn activate(&mut self, id: FiberId) {
        let refused = self.name_refused(id);
        let Some(mut plugin) = self
            .fibers
            .get_mut(&id)
            .and_then(|fiber| fiber.plugin.take())
        else {
            return;
        };
        let result = refused.map_or_else(
            || {
                plugin.apply(&mut Context {
                    app: self,
                    fiber: id,
                })
            },
            |reason| Err(PluginError::new(reason)),
        );
        if let Err(error) = &result {
            // Temporal composability also covers a failed start: leave nothing behind.
            self.unwind(id);
            self.announce("plugin/failed", id, Some(&error.to_string()));
        }
        let Some(fiber) = self.fibers.get_mut(&id) else {
            return;
        };
        fiber.plugin = Some(plugin);
        fiber.state = match result {
            Ok(()) => FiberState::Active,
            Err(error) => FiberState::Failed(error.to_string()),
        };
        if fiber.state == FiberState::Active {
            self.announce("plugin/activated", id, None);
        }
    }

    fn unload(&mut self, id: FiberId, mode: Unload) {
        let Some(fiber) = self.fibers.get(&id) else {
            return;
        };
        let was_active = fiber.state == FiberState::Active;
        if mode == Unload::Suspend && !was_active {
            return;
        }

        // Spatial composability: whoever depends on this subtree's services goes first.
        let mut subtree = BTreeSet::new();
        let mut provided = BTreeSet::new();
        self.survey(id, &mut subtree, &mut provided);
        let dependents: Vec<FiberId> = self
            .fibers
            .iter()
            .filter(|(other, fiber)| {
                fiber.state == FiberState::Active
                    && !subtree.contains(*other)
                    && fiber
                        .inject
                        .iter()
                        .any(|service| provided.contains(service))
            })
            .map(|(&other, _)| other)
            .collect();
        for dependent in dependents {
            self.unload(dependent, Unload::Suspend);
        }

        self.unwind(id);
        match mode {
            Unload::Suspend => {
                if let Some(fiber) = self.fibers.get_mut(&id) {
                    fiber.state = FiberState::Pending;
                }
                self.announce("plugin/suspended", id, None);
            }
            Unload::Dispose => {
                self.announce("plugin/disposed", id, None);
                self.fibers.remove(&id);
            }
        }
    }

    /// Collect the fibers in `id`'s subtree and the services they provide.
    fn survey(
        &self,
        id: FiberId,
        subtree: &mut BTreeSet<FiberId>,
        provided: &mut BTreeSet<String>,
    ) {
        let Some(fiber) = self.fibers.get(&id) else {
            return;
        };
        subtree.insert(id);
        for effect in &fiber.effects {
            match effect {
                Effect::Service(name) => {
                    provided.insert(name.clone());
                }
                Effect::Child(child) => self.survey(*child, subtree, provided),
                Effect::Callback(_) | Effect::Listener(_) => {}
            }
        }
    }

    /// Revert a fiber's effects, newest first.
    fn unwind(&mut self, id: FiberId) {
        let Some(fiber) = self.fibers.get_mut(&id) else {
            return;
        };
        let effects = mem::take(&mut fiber.effects);
        for effect in effects.into_iter().rev() {
            match effect {
                Effect::Callback(dispose) => dispose(),
                Effect::Service(name) => {
                    self.services.remove(&name);
                }
                Effect::Listener(listener) => self.events.unsubscribe(listener),
                Effect::Child(child) => self.unload(child, Unload::Dispose),
            }
        }
    }

    fn announce(&self, topic: &str, id: FiberId, reason: Option<&str>) {
        let Some(fiber) = self.fibers.get(&id) else {
            return;
        };
        let mut payload = format!("{{\"plugin\":{}", json::string(&fiber.name));
        if let Some(reason) = reason {
            payload.push_str(",\"reason\":");
            payload.push_str(&json::string(reason));
        }
        payload.push('}');
        // Nobody is waiting on a lifecycle event. If the log refused it the stream is closed,
        // and whoever commits next is told.
        drop(
            self.events
                .emit(Draft::new(Source::Host, topic).payload(payload)),
        );
    }
}

/// A plugin's handle on the [`App`]. Everything registered through it belongs to the plugin
/// and is reverted when the plugin is unloaded.
pub struct Context<'app> {
    app: &'app mut App,
    fiber: FiberId,
}

impl fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("fiber", &self.fiber)
            .field("plugin", &self.name())
            .finish_non_exhaustive()
    }
}

impl Context<'_> {
    /// The id of the plugin instance this context belongs to.
    #[must_use]
    pub const fn fiber(&self) -> FiberId {
        self.fiber
    }

    /// The name of the plugin this context belongs to.
    #[must_use]
    pub fn name(&self) -> &str {
        self.app
            .fibers
            .get(&self.fiber)
            .map_or("", |fiber| fiber.name.as_str())
    }

    /// Record a side effect by giving the function that undoes it. Disposers run in reverse
    /// order of registration when the plugin is unloaded.
    pub fn effect(&mut self, dispose: impl FnOnce() + Send + Sync + 'static) {
        self.push(Effect::Callback(Box::new(dispose)));
    }

    /// Provide a service to other plugins for as long as this plugin is active.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::ServiceTaken`] if the name is in use.
    pub fn provide<T: Any + Send + Sync>(
        &mut self,
        name: impl Into<String>,
        service: Arc<T>,
    ) -> Result<(), ContextError> {
        let name = name.into();
        self.app.insert_service(name.clone(), service)?;
        self.push(Effect::Service(name));
        Ok(())
    }

    /// Use a service this plugin declared in [`Plugin::inject`].
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::NotInjected`] if the service was not declared, otherwise
    /// whatever [`App::service`] returns.
    pub fn service<T: Any + Send + Sync>(&self, name: &str) -> Result<Arc<T>, ContextError> {
        let declared = self
            .app
            .fibers
            .get(&self.fiber)
            .is_some_and(|fiber| fiber.inject.iter().any(|service| service == name));
        if !declared {
            return Err(ContextError::NotInjected {
                plugin: self.name().to_owned(),
                service: name.to_owned(),
            });
        }
        self.app.service(name)
    }

    /// Listen for events whose topic matches `topics` until the plugin is unloaded.
    pub fn on(
        &mut self,
        topics: impl Into<Pattern>,
        listener: impl Fn(&Event) + Send + Sync + 'static,
    ) {
        let id = self.app.events.subscribe(topics, listener);
        self.push(Effect::Listener(id));
    }

    /// Emit an event with this plugin as its source.
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the event could not be stored.
    pub fn emit(
        &self,
        topic: impl Into<String>,
        payload: impl Into<String>,
    ) -> Result<EventId, LogError> {
        let source = Source::Plugin(self.name().to_owned());
        self.app
            .events
            .emit(Draft::new(source, topic).payload(payload))
    }

    /// The app's event stream, for handlers that need to emit after `apply` returns.
    #[must_use]
    pub fn events(&self) -> Arc<EventStream> {
        Arc::clone(&self.app.events)
    }

    /// A handle for changing the plugin tree after `apply` has returned, for example to load
    /// a child plugin from a later call. See [`Requests`].
    #[must_use]
    pub fn requests(&self) -> Requests {
        self.app.requests()
    }

    /// Load a child plugin. It is applied once this plugin is active and is disposed with it.
    pub fn plugin(&mut self, plugin: impl Plugin + 'static) -> FiberId {
        let child = self.app.add_fiber(Some(self.fiber), Box::new(plugin));
        self.push(Effect::Child(child));
        child
    }

    fn push(&mut self, effect: Effect) {
        if let Some(fiber) = self.app.fibers.get_mut(&self.fiber) {
            fiber.effects.push(effect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Journal = Arc<Mutex<Vec<String>>>;

    fn journal() -> Journal {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn note(journal: &Journal, entry: &str) {
        journal.lock().unwrap().push(entry.to_owned());
    }

    fn entries(journal: &Journal) -> Vec<String> {
        journal.lock().unwrap().clone()
    }

    /// A plugin that journals `+name` when applied and `-name` when reverted.
    fn tracked(name: &'static str, inject: &[&str], log: &Journal) -> impl Plugin + 'static {
        let log = Arc::clone(log);
        plugin_fn(name, inject, move |ctx| {
            note(&log, &format!("+{name}"));
            let log = Arc::clone(&log);
            ctx.effect(move || note(&log, &format!("-{name}")));
            Ok(())
        })
    }

    /// A plugin that provides the service `name`.
    fn provider(name: &'static str) -> impl Plugin + 'static {
        plugin_fn(name, &[], move |ctx| {
            ctx.provide(name, Arc::new(7_u32))?;
            Ok(())
        })
    }

    #[test]
    fn effects_are_reverted_in_reverse_order_on_dispose() {
        let log = journal();
        let mut app = App::new();
        let effects = Arc::clone(&log);
        let id = app.plugin(plugin_fn("p", &[], move |ctx| {
            for step in ["one", "two", "three"] {
                let effects = Arc::clone(&effects);
                ctx.effect(move || note(&effects, step));
            }
            Ok(())
        }));

        assert_eq!(app.state(id), Some(&FiberState::Active));
        app.dispose(id).unwrap();

        assert_eq!(entries(&log), ["three", "two", "one"]);
        assert_eq!(app.state(id), None);
        assert_eq!(app.dispose(id), Err(ContextError::UnknownFiber(id)));
    }

    #[test]
    fn a_plugin_waits_for_its_services_and_follows_them() {
        let log = journal();
        let mut app = App::new();

        let user = app.plugin(tracked("client", &["db"], &log));
        assert_eq!(app.state(user), Some(&FiberState::Pending));

        let db = app.plugin(provider("db"));
        assert_eq!(app.state(user), Some(&FiberState::Active));

        app.dispose(db).unwrap();
        assert_eq!(app.state(user), Some(&FiberState::Pending));

        app.plugin(provider("db"));
        assert_eq!(app.state(user), Some(&FiberState::Active));
        assert_eq!(entries(&log), ["+client", "-client", "+client"]);
    }

    #[test]
    fn dependents_unload_before_the_provider_reverts() {
        let log = journal();
        let mut app = App::new();
        let provider_log = Arc::clone(&log);
        let db = app.plugin(plugin_fn("db", &[], move |ctx| {
            ctx.provide("db", Arc::new(1_u8))?;
            let log = Arc::clone(&provider_log);
            ctx.effect(move || note(&log, "-db"));
            Ok(())
        }));
        app.plugin(tracked("client", &["db"], &log));

        app.dispose(db).unwrap();

        assert_eq!(entries(&log), ["+client", "-client", "-db"]);
    }

    #[test]
    fn children_are_disposed_with_their_parent() {
        let log = journal();
        let mut app = App::new();
        let child_log = Arc::clone(&log);
        let parent = app.plugin(plugin_fn("parent", &[], move |ctx| {
            ctx.plugin(tracked("child", &[], &child_log));
            Ok(())
        }));

        assert_eq!(app.fibers().len(), 2);
        app.dispose(parent).unwrap();

        assert_eq!(entries(&log), ["+child", "-child"]);
        assert_eq!(app.fibers(), Vec::<FiberInfo>::new());
    }

    #[test]
    fn a_failed_apply_is_rolled_back_and_can_be_retried() {
        let log = journal();
        let mut app = App::new();
        let attempts = Arc::clone(&log);
        let id = app.plugin(plugin_fn("flaky", &[], move |ctx| {
            ctx.provide("flaky", Arc::new(()))?;
            let first = entries(&attempts).is_empty();
            note(&attempts, "attempt");
            if first {
                return Err(PluginError::new("not yet"));
            }
            Ok(())
        }));

        assert_eq!(app.state(id), Some(&FiberState::Failed("not yet".into())));
        assert!(app.service::<()>("flaky").is_err());

        app.reload(id).unwrap();

        assert_eq!(app.state(id), Some(&FiberState::Active));
        assert!(app.service::<()>("flaky").is_ok());
    }

    #[test]
    fn services_must_be_injected_and_typed() {
        let mut app = App::new();
        app.plugin(provider("db"));
        let seen = journal();
        let results = Arc::clone(&seen);
        app.plugin(plugin_fn("nosy", &["db"], move |ctx| {
            note(
                &results,
                &format!("{:?}", ctx.service::<u32>("db").map(|n| *n)),
            );
            note(
                &results,
                &ctx.service::<u32>("other").unwrap_err().to_string(),
            );
            note(
                &results,
                &ctx.service::<String>("db").unwrap_err().to_string(),
            );
            note(
                &results,
                &ctx.provide("db", Arc::new(0_u32)).unwrap_err().to_string(),
            );
            Ok(())
        }));

        assert_eq!(
            entries(&seen),
            [
                "Ok(7)",
                "plugin `nosy` uses service `other` without listing it in `inject`",
                "service `db` has a different type",
                "service `db` is already provided",
            ]
        );
    }

    #[test]
    fn listeners_stop_when_the_plugin_is_disposed_and_lifecycle_is_logged() {
        let log = journal();
        let mut app = App::new();
        let heard = Arc::clone(&log);
        let id = app.plugin(plugin_fn("ears", &[], move |ctx| {
            let heard = Arc::clone(&heard);
            ctx.on("note/*", move |event| note(&heard, &event.payload));
            ctx.emit("note/hello", "from apply")?;
            Ok(())
        }));

        app.events()
            .emit(Draft::new(Source::User, "note/a").payload("while loaded"))
            .unwrap();
        app.dispose(id).unwrap();
        app.events()
            .emit(Draft::new(Source::User, "note/b").payload("after dispose"))
            .unwrap();

        assert_eq!(entries(&log), ["from apply", "while loaded"]);
        let lifecycle: Vec<String> = app
            .events()
            .matching(&Pattern::new("plugin/*"))
            .into_iter()
            .map(|event| format!("{} {}", event.topic, event.payload))
            .collect();
        assert_eq!(
            lifecycle,
            [
                "plugin/activated {\"plugin\":\"ears\"}",
                "plugin/disposed {\"plugin\":\"ears\"}",
            ]
        );
    }

    #[test]
    fn a_boxed_plugin_is_a_plugin() {
        let log = journal();
        let mut app = App::new();
        let boxed: Box<dyn Plugin> = Box::new(tracked("boxed", &["db"], &log));

        let id = app.plugin(boxed);
        assert_eq!(app.state(id), Some(&FiberState::Pending));
        app.plugin(provider("db"));

        assert_eq!(app.fibers()[0].name, "boxed");
        assert_eq!(entries(&log), ["+boxed"]);
    }

    #[test]
    fn a_name_belongs_to_the_plugin_loaded_first() {
        let log = journal();
        let mut app = App::new();
        let first = app.plugin(tracked("todo", &[], &log));
        let second = app.plugin(tracked("todo", &[], &log));

        assert_eq!(app.state(first), Some(&FiberState::Active));
        assert_eq!(
            app.state(second),
            Some(&FiberState::Failed(
                "another plugin named `todo` is already loaded".into()
            ))
        );
        // Not applied at all, so there was nothing to revert.
        assert_eq!(entries(&log), ["+todo"]);

        // The name is free once its holder is gone, and a reload takes it.
        app.dispose(first).unwrap();
        app.reload(second).unwrap();
        assert_eq!(app.state(second), Some(&FiberState::Active));

        // A plugin still waiting for a service holds its name all the same.
        let waiting = app.plugin(tracked("memory", &["db"], &log));
        let impostor = app.plugin(tracked("memory", &[], &log));
        assert_eq!(app.state(waiting), Some(&FiberState::Pending));
        assert!(matches!(app.state(impostor), Some(FiberState::Failed(_))));

        for reserved in ["host", "agent", ""] {
            let id = app.plugin(tracked(reserved, &[], &log));
            assert!(
                matches!(app.state(id), Some(FiberState::Failed(_))),
                "{reserved:?}"
            );
        }
    }

    #[test]
    fn requests_from_elsewhere_are_applied_when_the_tree_next_settles() {
        let log = journal();
        let mut app = App::new();
        let parent = app.plugin(tracked("parent", &[], &log));
        let requests = app.requests();

        let child = requests.load(Some(parent), tracked("child", &[], &log));
        assert_eq!(app.state(child), None);
        assert_eq!(requests.pending(), 1);

        assert!(app.apply_requests());
        assert_eq!(app.state(child), Some(&FiberState::Active));
        assert!(!app.apply_requests());

        // A requested child goes with its parent, like any other.
        app.dispose(parent).unwrap();
        assert_eq!(app.state(child), None);
        assert_eq!(entries(&log), ["+parent", "+child", "-child", "-parent"]);
    }

    #[test]
    fn a_request_whose_parent_is_gone_is_dropped() {
        let log = journal();
        let mut app = App::new();
        let parent = app.plugin(tracked("parent", &[], &log));
        let requests = app.requests();
        let child = requests.load(Some(parent), tracked("orphan", &[], &log));
        let top = requests.load(None, tracked("top", &[], &log));
        requests.dispose(top);

        app.dispose(parent).unwrap();

        assert_eq!(app.state(child), None);
        assert_eq!(app.state(top), None);
        // Loaded and disposed in one batch, `top` was never started.
        assert_eq!(entries(&log), ["+parent", "-parent"]);
        let dropped = app.events().matching(&Pattern::new("plugin/dropped"));
        assert_eq!(dropped.len(), 1);
    }
}
