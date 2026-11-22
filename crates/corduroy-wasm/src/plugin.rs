//! A WebAssembly component as a [`Plugin`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use corduroy_core::{
    CallError, Context, ContextError, Event, EventStream, GATES, Gate, Gates, Grants, KERNELS,
    Kernels, POINTS, PROMPTS, Pattern, Plugin, PluginError, PromptPart, Prompts, SERVICES, Scope,
    Service, Services, Source, TOOL_CALL, TOOLS, Tool, ToolError, ToolSpec, Tools, Verdict,
};
use serde_json::Value;
use wasmtime::component::{Component, HasSelf, Linker};

use crate::runtime::{WasmError, WasmRuntime};

mod bindings;
mod host;
mod instance;
mod paths;

use bindings::corduroy::plugin::{
    clock, events, fs_read, fs_write, kernel, log, random, service, state, tools, types,
};
use host::{HostState, Lent};
use instance::{Blueprint, Instance};

/// The package every host interface belongs to.
const PACKAGE: &str = "corduroy:plugin/";

/// The host interfaces that are capabilities, by the name they are granted under.
const CAPABILITIES: [&str; 10] = [
    "fs-read", "fs-write", "kernel", "events", "log", "clock", "random", "tools", "state",
    "service",
];

/// A plugin implemented as a WebAssembly component.
///
/// Created by [`WasmRuntime::load`] with no capabilities. The component is instantiated
/// each time the plugin becomes active and dropped when it is unloaded, so a reload starts
/// from fresh memory.
pub struct WasmPlugin {
    runtime: WasmRuntime,
    name: String,
    component: Component,
    requires: Vec<String>,
    grants: Grants,
    config: String,
}

impl fmt::Debug for WasmPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WasmPlugin")
            .field("name", &self.name)
            .field("requires", &self.requires)
            .field("grants", &self.grants)
            .finish_non_exhaustive()
    }
}

impl WasmPlugin {
    pub(crate) fn new(
        runtime: WasmRuntime,
        name: String,
        component: Component,
    ) -> Result<Self, WasmError> {
        let mut requires = Vec::new();
        for (import, _) in component.component_type().imports(runtime.engine()) {
            match capability_of(import) {
                Some("types") => {}
                Some(capability) if CAPABILITIES.contains(&capability) => {
                    requires.push(capability.to_owned());
                }
                _ => {
                    return Err(WasmError::Invalid(format!(
                        "it imports `{import}`, which no capability provides"
                    )));
                }
            }
        }
        Ok(Self {
            runtime,
            name,
            component,
            requires,
            grants: Grants::none(),
            config: "{}".to_owned(),
        })
    }

    /// Use `name` for this plugin in events and diagnostics instead of its file name.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Grant one capability within `scope`.
    #[must_use]
    pub fn grant(mut self, capability: impl Into<String>, scope: Scope) -> Self {
        self.grants = self.grants.grant(capability, scope);
        self
    }

    /// Replace everything granted so far.
    #[must_use]
    pub fn with_grants(mut self, grants: Grants) -> Self {
        self.grants = grants;
        self
    }

    /// Configuration passed to the plugin's `activate`, as JSON text. Defaults to `{}`.
    #[must_use]
    pub fn config(mut self, json: impl Into<String>) -> Self {
        self.config = json.into();
        self
    }

    /// The capabilities this plugin imports, and so must be granted before it can start.
    #[must_use]
    pub fn requires(&self) -> &[String] {
        &self.requires
    }

    /// A linker holding the interfaces this plugin imports, and nothing else.
    fn linker(&self) -> Result<Linker<HostState>, PluginError> {
        let mut linker: Linker<HostState> = Linker::new(self.runtime.engine());
        types::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(|error| link_error(&error))?;
        for capability in &self.requires {
            match capability.as_str() {
                "fs-read" => fs_read::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                "fs-write" => fs_write::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                "kernel" => kernel::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                "events" => events::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                "log" => log::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                "clock" => clock::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                "random" => random::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                "tools" => tools::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                "state" => state::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                "service" => service::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state),
                _ => Ok(()),
            }
            .map_err(|error| link_error(&error))?;
        }
        Ok(linker)
    }

    /// What `get` returns, if this plugin imports `capability`.
    fn lends<T>(
        &self,
        capability: &str,
        get: impl FnOnce() -> Result<Arc<T>, ContextError>,
    ) -> Result<Option<Arc<T>>, PluginError> {
        if self.requires.iter().any(|required| required == capability) {
            Ok(Some(get()?))
        } else {
            Ok(None)
        }
    }

    /// Instantiate the component with exactly the granted interfaces linked.
    fn instantiate(&self, ctx: &Context<'_>) -> Result<Instance, PluginError> {
        let missing: Vec<&str> = self
            .requires
            .iter()
            .map(String::as_str)
            .filter(|capability| !self.grants.has(capability))
            .collect();
        if !missing.is_empty() {
            return Err(PluginError::new(format!(
                "plugin `{}` was not granted: {}",
                self.name,
                missing.join(", ")
            )));
        }

        let lent = Lent {
            kernels: self.lends("kernel", || ctx.service::<Kernels>(KERNELS))?,
            tools: self.lends("tools", || ctx.service::<Tools>(TOOLS))?,
            gates: self.lends("tools", || ctx.service::<Gates>(GATES))?,
            services: self.lends("service", || ctx.service::<Services>(SERVICES))?,
            state: Arc::new(Mutex::new(stored_state(&ctx.events(), &self.name))),
        };
        let blueprint = Blueprint {
            name: self.name.clone(),
            engine: self.runtime.engine().clone(),
            component: self.component.clone(),
            linker: self.linker()?,
            grants: self.grants.clone(),
            root: self.runtime.root().to_owned(),
            lent,
            limits: self.runtime.limits(),
            config: self.config.clone(),
        };
        let inner = blueprint.instantiate().map_err(PluginError::new)?;
        Ok(Instance::new(blueprint, inner, ctx.events()))
    }
}

impl Plugin for WasmPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn inject(&self) -> Vec<String> {
        // What a plugin contributes is known only once it is running, so every registry it
        // might contribute to is a dependency.
        let mut services = vec![
            TOOLS.to_owned(),
            PROMPTS.to_owned(),
            GATES.to_owned(),
            SERVICES.to_owned(),
        ];
        if self
            .requires
            .iter()
            .any(|capability| capability == "kernel")
        {
            services.push(KERNELS.to_owned());
        }
        services
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        let instance = Arc::new(self.instantiate(ctx)?);
        let info = instance
            .call(|plugin, store| plugin.call_activate(store, &self.config))
            .map_err(PluginError::new)?
            .map_err(|reason| {
                PluginError::new(format!("plugin `{}` refused to start: {reason}", self.name))
            })?;

        let tools = ctx.service::<Tools>(TOOLS)?;
        for spec in info.tools {
            let name = spec.name.clone();
            let tool = WasmTool {
                instance: Arc::clone(&instance),
                spec: ToolSpec {
                    name: spec.name,
                    description: spec.description,
                    input_schema: spec.input_schema,
                    replay_safe: spec.replay_safe,
                    read_only: spec.read_only,
                },
            };
            tools.register(ctx, name, Arc::new(tool))?;
        }

        for topics in info.subscriptions {
            let listener = Arc::clone(&instance);
            ctx.on(topics, move |event| deliver(&listener, event));
        }

        if info.prompt {
            let prompts = ctx.service::<Prompts>(PROMPTS)?;
            let part: Arc<dyn PromptPart> = Arc::new(WasmPrompt(Arc::clone(&instance)));
            prompts.register(ctx, self.name.clone(), part)?;
        }

        self.register_gate(ctx, &instance, info.gates)?;

        let services = ctx.service::<Services>(SERVICES)?;
        for name in info.provides {
            let provider: Arc<dyn Service> = Arc::new(WasmService {
                instance: Arc::clone(&instance),
                name: name.clone(),
            });
            services.register(ctx, name, provider)?;
        }
        Ok(())
    }
}

impl WasmPlugin {
    /// Register the plugin as a gate at `points`, if it named any.
    fn register_gate(
        &self,
        ctx: &mut Context<'_>,
        instance: &Arc<Instance>,
        points: Vec<String>,
    ) -> Result<(), PluginError> {
        if points.is_empty() {
            return Ok(());
        }
        if let Some(unknown) = points
            .iter()
            .find(|point| !POINTS.contains(&point.as_str()))
        {
            return Err(PluginError::new(format!(
                "plugin `{}` wants to gate `{unknown}`, which is not a decision point; \
                 the points are: {}",
                self.name,
                POINTS.join(", ")
            )));
        }
        // A plugin's own tool calls pass the gates, so it would be asked about them while it
        // runs. Gating tool calls and making them are kept apart.
        if points.iter().any(|point| point == TOOL_CALL)
            && self.requires.iter().any(|capability| capability == "tools")
        {
            return Err(PluginError::new(format!(
                "plugin `{}` gates `{TOOL_CALL}` and imports `tools`: a plugin may gate tool \
                 calls or make them, not both",
                self.name
            )));
        }
        let gates = ctx.service::<Gates>(GATES)?;
        let gate: Arc<dyn Gate> = Arc::new(WasmGate {
            instance: Arc::clone(instance),
            points,
        });
        gates.register(ctx, self.name.clone(), gate)?;
        Ok(())
    }
}

/// The `state` store of the plugin `name`, as its `state/set` and `state/remove` events on
/// `events` leave it.
fn stored_state(events: &EventStream, name: &str) -> BTreeMap<String, String> {
    let source = Source::Plugin(name.to_owned());
    let mut store = BTreeMap::new();
    for event in events.matching(&Pattern::new("state/*")) {
        if event.source != source {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
            continue;
        };
        let Some(key) = payload.get("key").and_then(Value::as_str) else {
            continue;
        };
        match (
            event.topic.as_str(),
            payload.get("value").and_then(Value::as_str),
        ) {
            ("state/set", Some(value)) => store.insert(key.to_owned(), value.to_owned()),
            ("state/remove", _) => store.remove(key),
            _ => None,
        };
    }
    store
}

/// The capability an import names, if it is one of the host's interfaces.
///
/// `corduroy:plugin/fs-read@0.1.0` names `fs-read`.
fn capability_of(import: &str) -> Option<&str> {
    let interface = import.strip_prefix(PACKAGE)?;
    Some(
        interface
            .split_once('@')
            .map_or(interface, |(name, _)| name),
    )
}

fn link_error(error: &wasmtime::Error) -> PluginError {
    PluginError::new(format!("{error:#}"))
}

/// Hand an event to a plugin's `on-event`, unless the plugin raised it itself.
fn deliver(instance: &Instance, event: &Event) {
    if event.source == Source::Plugin(instance.name().to_owned()) {
        return;
    }
    let event = types::Event {
        id: event.id.get(),
        session: event
            .session
            .as_ref()
            .map(|session| session.as_str().to_owned()),
        source: event.source.to_string(),
        cause: event.cause.map(corduroy_core::EventId::get),
        topic: event.topic.clone(),
        payload: event.payload.clone(),
    };
    // A trap is already on the event stream as `plugin/trapped`; a listener has nobody to
    // return it to.
    drop(instance.call(|plugin, store| plugin.call_on_event(store, &event)));
}

struct WasmTool {
    instance: Arc<Instance>,
    spec: ToolSpec,
}

impl Tool for WasmTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn call(&self, input: &str) -> Result<String, ToolError> {
        self.instance
            .call(|plugin, store| plugin.call_call_tool(store, &self.spec.name, input))
            .map_err(ToolError::Failed)?
            .map_err(|error| match error {
                types::ToolError::UnknownTool(name) => ToolError::UnknownTool(name),
                types::ToolError::InvalidInput(reason) => ToolError::InvalidInput(reason),
                types::ToolError::Failed(reason) => ToolError::Failed(reason),
                types::ToolError::Denied(reason) => ToolError::Denied(reason),
            })
    }
}

struct WasmPrompt(Arc<Instance>);

impl PromptPart for WasmPrompt {
    fn render(&self) -> Option<String> {
        self.0
            .call(|plugin, store| plugin.call_prompt_part(store))
            .ok()
            .flatten()
    }
}

/// A plugin's `gate` export, asked only at the points the plugin listed.
struct WasmGate {
    instance: Arc<Instance>,
    points: Vec<String>,
}

impl Gate for WasmGate {
    fn check(&self, point: &str, request: &str) -> Option<Verdict> {
        if !self.points.iter().any(|guarded| guarded == point) {
            return None;
        }
        let verdict = self
            .instance
            .call(|plugin, store| plugin.call_gate(store, point, request));
        // A gate that cannot answer fails closed: a policy that trapped is no reason to let
        // through what it was there to stop.
        Some(match verdict {
            Ok(types::Verdict::Allow) => Verdict::Allow,
            Ok(types::Verdict::Ask(reason)) => Verdict::Ask(reason),
            Ok(types::Verdict::Deny(reason)) => Verdict::Deny(reason),
            Err(trapped) => Verdict::Deny(trapped),
        })
    }
}

/// A service the plugin provides, answered by its `handle-call` export.
struct WasmService {
    instance: Arc<Instance>,
    name: String,
}

impl Service for WasmService {
    fn call(&self, caller: &str, method: &str, payload: &str) -> Result<String, CallError> {
        let call = types::ServiceCall {
            caller: caller.to_owned(),
            service: self.name.clone(),
            method: method.to_owned(),
            payload: payload.to_owned(),
        };
        self.instance
            .call(|plugin, store| plugin.call_handle_call(store, &call))
            .map_err(CallError::Failed)?
            .map_err(|error| match error {
                types::CallError::Unknown(what) => CallError::Unknown(what),
                types::CallError::InvalidInput(reason) => CallError::InvalidInput(reason),
                types::CallError::Failed(reason) => CallError::Failed(reason),
                types::CallError::Denied(reason) => CallError::Denied(reason),
            })
    }
}
