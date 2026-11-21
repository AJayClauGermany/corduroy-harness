//! One live instantiation of a plugin component.

use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use corduroy_core::{Draft, EventStream, Source, json};
use wasmtime::Store;

use super::bindings;
use super::host::HostState;

/// A plugin's store and exports, shared by everything the plugin registered.
///
/// The instance lives exactly as long as those registrations: when the plugin is unloaded
/// its tools, listeners, prompt part and gate are removed, the last reference goes, and the
/// instance's memory is freed with it.
pub(super) struct Instance {
    name: String,
    inner: Mutex<Inner>,
    events: Arc<EventStream>,
    fuel_per_call: u64,
    /// Set once a call has trapped. A trapped component instance cannot be entered again.
    trapped: AtomicBool,
}

struct Inner {
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

impl Instance {
    pub(super) fn new(
        store: Store<HostState>,
        exports: bindings::Plugin,
        events: Arc<EventStream>,
        fuel_per_call: u64,
    ) -> Self {
        Self {
            name: store.data().plugin.clone(),
            inner: Mutex::new(Inner { store, exports }),
            events,
            fuel_per_call,
            trapped: AtomicBool::new(false),
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// Call into the plugin with a fresh fuel budget.
    ///
    /// Events the plugin raised are delivered after the call has returned and the instance
    /// is unlocked, so a listener may call straight back into this plugin.
    ///
    /// # Errors
    ///
    /// Returns a description of the trap if the plugin trapped, now or on an earlier call.
    pub(super) fn call<T>(
        &self,
        call: impl FnOnce(&bindings::Plugin, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> Result<T, String> {
        if self.trapped.load(Ordering::SeqCst) {
            return Err(format!(
                "plugin `{}` trapped earlier and must be reloaded",
                self.name
            ));
        }
        let (result, outbox) = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .run(self.fuel_per_call, call);
        // The plugin's call has returned, so there is nobody to hand a refused commit to. The
        // stream is closed by then and the next commit of the run reports it.
        drop(self.events.commit(outbox));
        result.map_err(|error| {
            let reason = format!("{error:#}");
            if !self.trapped.swap(true, Ordering::SeqCst) {
                let payload = format!(
                    "{{\"plugin\":{},\"reason\":{}}}",
                    json::string(&self.name),
                    json::string(&reason)
                );
                drop(
                    self.events
                        .emit(Draft::new(Source::Host, "plugin/trapped").payload(payload)),
                );
            }
            format!("plugin `{}` trapped: {reason}", self.name)
        })
    }
}
