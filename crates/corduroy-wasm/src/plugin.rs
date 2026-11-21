//! A WebAssembly component as a [`Plugin`].

use std::fmt;
use std::sync::Arc;

use corduroy_core::{
    Context, Event, Grants, KERNELS, Kernels, PROMPTS, Plugin, PluginError, PromptPart, Prompts,
    Scope, Source, TOOLS, Tool, ToolError, ToolSpec, Tools,
};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Store, StoreLimitsBuilder};

use crate::runtime::{WasmError, WasmRuntime};

mod bindings;
mod host;
mod instance;
mod paths;

use bindings::corduroy::plugin::{events, fs_read, fs_write, kernel, types};
use host::HostState;
use instance::Instance;

/// The package every host interface belongs to.
const PACKAGE: &str = "corduroy:plugin/";

/// The host interfaces that are capabilities, by the name they are granted under.
const CAPABILITIES: [&str; 4] = ["fs-read", "fs-write", "kernel", "events"];

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
                _ => Ok(()),
            }
            .map_err(|error| link_error(&error))?;
        }
        Ok(linker)
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

        let engine = self.runtime.engine();
        let linker = self.linker()?;

        let limits = self.runtime.limits();
        let kernels = if self
            .requires
            .iter()
            .any(|capability| capability == "kernel")
        {
            Some(ctx.service::<Kernels>(KERNELS)?)
        } else {
            None
        };
        let mut store = Store::new(
            engine,
            HostState {
                plugin: self.name.clone(),
                grants: self.grants.clone(),
                root: self.runtime.root().to_owned(),
                kernels,
                outbox: Vec::new(),
                limits: StoreLimitsBuilder::new()
                    .memory_size(limits.memory_bytes)
                    .build(),
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(limits.fuel_per_call)
            .map_err(|error| link_error(&error))?;
        let exports = bindings::Plugin::instantiate(&mut store, &self.component, &linker)
            .map_err(|error| link_error(&error))?;
        Ok(Instance::new(
            store,
            exports,
            ctx.events(),
            limits.fuel_per_call,
        ))
    }
}

impl Plugin for WasmPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn inject(&self) -> Vec<String> {
        let mut services = vec![TOOLS.to_owned(), PROMPTS.to_owned()];
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
        Ok(())
    }
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
