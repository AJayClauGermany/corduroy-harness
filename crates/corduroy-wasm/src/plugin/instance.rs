//! One live instantiation of a plugin component, and starting it again after a trap.

use std::mem;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use corduroy_core::{Draft, EventStream, Grants, Kernels, Source, json};
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store, StoreLimitsBuilder};

use super::bindings;
use super::host::HostState;
use crate::runtime::Limits;

/// Everything needed to make a fresh instance of a plugin: once when it is applied, and again
/// each time it is restarted after a trap.
pub(super) struct Blueprint {
    pub(super) name: String,
    pub(super) engine: Engine,
    pub(super) component: Component,
    /// Holds exactly the granted interfaces.
    pub(super) linker: Linker<HostState>,
    pub(super) grants: Grants,
    pub(super) root: PathBuf,
    pub(super) kernels: Option<Arc<Kernels>>,
    pub(super) limits: Limits,
    /// Passed to `activate` when the plugin is restarted.
    pub(super) config: String,
}

impl Blueprint {
    /// A new store and instance, not yet activated.
    pub(super) fn instantiate(&self) -> Result<Inner, String> {
        let mut store = Store::new(
            &self.engine,
            HostState {
                plugin: self.name.clone(),
                grants: self.grants.clone(),
                root: self.root.clone(),
                kernels: self.kernels.clone(),
                outbox: Vec::new(),
                limits: StoreLimitsBuilder::new()
                    .memory_size(self.limits.memory_bytes)
                    .build(),
                started: Instant::now(),
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(self.limits.fuel_per_call)
            .map_err(|error| format!("{error:#}"))?;
        let exports = bindings::Plugin::instantiate(&mut store, &self.component, &self.linker)
            .map_err(|error| format!("{error:#}"))?;
        Ok(Inner { store, exports })
    }
}

/// A store and the exports of the component instantiated in it.
pub(super) struct Inner {
    store: Store<HostState>,
    exports: bindings::Plugin,
}

impl Inner {
    /// Run one call into the plugin and collect the events it raised.
    fn run<T>(
        &mut self,
        fuel: u64,
        call: impl FnOnce(&bindings::Plugin, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> (wasmtime::Result<T>, Vec<Draft>) {
        let result = self
            .store
            .set_fuel(fuel)
            .and_then(|()| call(&self.exports, &mut self.store));
        (result, mem::take(&mut self.store.data_mut().outbox))
    }
}

/// The instance, if it is alive, and how often it has trapped.
struct Slot {
    /// `None` once a call has trapped: a trapped component instance cannot be entered again.
    live: Option<Inner>,
    traps: u32,
}

/// A plugin's instance, shared by everything the plugin registered.
///
/// The instance lives exactly as long as those registrations: when the plugin is unloaded
/// its tools, listeners, prompt part and gate are removed, the last reference goes, and the
/// instance's memory is freed with it.
///
/// A call that traps kills the instance. The next call makes a new one from the blueprint
/// and activates it with the plugin's configuration again, so the plugin starts from fresh
/// memory and keeps its registrations. After [`Limits::restarts`] restarts, the next trap
/// stops the plugin for good: every call fails until the plugin is reloaded.
pub(super) struct Instance {
    blueprint: Blueprint,
    slot: Mutex<Slot>,
    events: Arc<EventStream>,
}

impl Instance {
    pub(super) const fn new(blueprint: Blueprint, inner: Inner, events: Arc<EventStream>) -> Self {
        Self {
            blueprint,
            slot: Mutex::new(Slot {
                live: Some(inner),
                traps: 0,
            }),
            events,
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.blueprint.name
    }

    /// Call into the plugin with a fresh fuel budget, first restarting it if an earlier call
    /// trapped and the restart limit allows.
    ///
    /// Events the plugin raised, and those the host raises about it, are delivered after the
    /// call has returned and the instance is unlocked, so a listener may call straight back
    /// into this plugin.
    ///
    /// # Errors
    ///
    /// Returns a description of the trap if the plugin trapped now, could not be restarted,
    /// or was stopped after trapping too often.
    pub(super) fn call<T>(
        &self,
        call: impl FnOnce(&bindings::Plugin, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> Result<T, String> {
        let mut raised = Vec::new();
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        let fuel = self.blueprint.limits.fuel_per_call;
        let ran = self.enter(&mut slot, &mut raised).and_then(|()| {
            slot.live
                .as_mut()
                .map(|inner| inner.run(fuel, call))
                .ok_or_else(|| format!("plugin `{}` has no instance", self.name()))
        });
        let result = ran.and_then(|(result, outbox)| {
            raised.extend(outbox);
            result.map_err(|error| self.trapped(&mut slot, &error, &mut raised))
        });
        drop(slot);
        // The plugin's call has returned, so there is nobody to hand a refused commit to. The
        // stream is closed by then and the next commit of the run reports it.
        drop(self.events.commit(raised));
        result
    }

    /// Make sure there is a live instance, restarting the plugin if it trapped.
    fn enter(&self, slot: &mut Slot, raised: &mut Vec<Draft>) -> Result<(), String> {
        if slot.live.is_none() {
            if slot.traps > self.blueprint.limits.restarts {
                return Err(format!(
                    "plugin `{}` was stopped after trapping {} times and must be reloaded",
                    self.name(),
                    slot.traps
                ));
            }
            let mut inner = self.blueprint.instantiate()?;
            let (activated, outbox) = inner
                .run(self.blueprint.limits.fuel_per_call, |plugin, store| {
                    plugin.call_activate(store, &self.blueprint.config)
                });
            raised.extend(outbox);
            match activated {
                Err(error) => return Err(self.trapped(slot, &error, raised)),
                Ok(Err(reason)) => {
                    let error = wasmtime::Error::msg(format!("refused to restart: {reason}"));
                    return Err(self.trapped(slot, &error, raised));
                }
                Ok(Ok(_info)) => {}
            }
            raised
                .push(self.host_event("plugin/restarted", &format!("\"attempt\":{}", slot.traps)));
            slot.live = Some(inner);
        }
        Ok(())
    }

    /// Note that the instance trapped with `error`: drop it, count the trap, and say so on
    /// the stream. Returns the message for the caller.
    fn trapped(&self, slot: &mut Slot, error: &wasmtime::Error, raised: &mut Vec<Draft>) -> String {
        let reason = format!("{error:#}");
        slot.live = None;
        slot.traps = slot.traps.saturating_add(1);
        raised.push(self.host_event(
            "plugin/trapped",
            &format!(
                "\"reason\":{},\"traps\":{}",
                json::string(&reason),
                slot.traps
            ),
        ));
        if slot.traps > self.blueprint.limits.restarts {
            raised.push(self.host_event("plugin/stopped", &format!("\"traps\":{}", slot.traps)));
        }
        format!("plugin `{}` trapped: {reason}", self.name())
    }

    /// An event from the host about this plugin, with `fields` after its name.
    fn host_event(&self, topic: &str, fields: &str) -> Draft {
        let payload = format!("{{\"plugin\":{},{fields}}}", json::string(self.name()));
        Draft::new(Source::Host, topic).payload(payload)
    }
}
