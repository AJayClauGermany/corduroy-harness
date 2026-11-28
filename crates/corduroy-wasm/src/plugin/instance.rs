//! One live instantiation of a plugin component, starting it again after a trap, and never
//! entering it twice.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::mem;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use corduroy_core::call::Serve;
use corduroy_core::{Draft, EventId, EventStream, Grants, SessionId, Source, json};
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store, StoreLimitsBuilder};

use super::bindings;
use super::bindings::corduroy::plugin::types;
use super::host::{HostState, Lent};
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
    pub(super) protected: Vec<PathBuf>,
    pub(super) lent: Lent,
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
                protected: self.protected.clone(),
                lent: self.lent.clone(),
                outbox: Vec::new(),
                pending: BTreeMap::new(),
                writes: Vec::new(),
                calls: 0,
                limits: StoreLimitsBuilder::new()
                    .memory_size(self.limits.memory_bytes)
                    .build(),
                started: Instant::now(),
                wait: self.limits.outbound_wait,
                credentials: Vec::new(),
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

/// What one call into a plugin produced.
struct Ran<T> {
    result: wasmtime::Result<T>,
    /// Events it raised, and those the host raised about it.
    events: Vec<Draft>,
    /// Its writes to the `state` capability, by key, kept only if it returned.
    pending: BTreeMap<String, Option<String>>,
    /// The events recording `pending`.
    writes: Vec<Draft>,
}

/// Keep a call's `state` writes: apply them to the store, and queue their events.
fn keep_writes(
    lent: &Lent,
    pending: BTreeMap<String, Option<String>>,
    writes: Vec<Draft>,
    raised: &mut Vec<Draft>,
) {
    let mut store = lent.state();
    for (key, value) in pending {
        match value {
            Some(value) => store.insert(key, value),
            None => store.remove(&key),
        };
    }
    drop(store);
    raised.extend(writes);
}

impl Inner {
    /// Run one call into the plugin and collect what it raised.
    fn run<T>(
        &mut self,
        fuel: u64,
        call: impl FnOnce(&bindings::Plugin, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> Ran<T> {
        let result = self
            .store
            .set_fuel(fuel)
            .and_then(|()| call(&self.exports, &mut self.store));
        let state = self.store.data_mut();
        Ran {
            result,
            events: mem::take(&mut state.outbox),
            pending: mem::take(&mut state.pending),
            writes: mem::take(&mut state.writes),
        }
    }
}

thread_local! {
    /// The instances this thread is inside, outermost first. A plugin's call to the host can
    /// lead into another plugin (a tool, a service, a gate), all on this thread, so this is
    /// the chain of calls that would deadlock if it came back to one of its own links.
    static CHAIN: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// Gives each instance an identity on the call chain.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Whether instance `id` is on this thread's chain, and whether the chain is empty.
fn chain_has(id: u64) -> (bool, bool) {
    CHAIN.with_borrow(|chain| (chain.contains(&id), chain.is_empty()))
}

/// This thread is inside instance `id` until dropped.
struct Link;

impl Link {
    fn push(id: u64) -> Self {
        CHAIN.with_borrow_mut(|chain| chain.push(id));
        Self
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        CHAIN.with_borrow_mut(Vec::pop);
    }
}

/// The event recording that a plugin's queue of events was full, so that it missed one.
const MISSED: &str = "plugin/missed";

/// An event for a plugin's `on-event`: the event as the plugin sees it, and what the host
/// keeps of it for the call.
pub(super) struct Heard {
    /// The event's id: the cause of whatever the plugin raises on hearing it.
    pub(super) cause: EventId,
    /// The session the event belongs to, which the plugin hears it for.
    pub(super) session: Option<SessionId>,
    pub(super) event: types::Event,
}

/// Why a call could not have the instance.
enum Refused {
    /// The instance is further up this thread's chain of calls.
    Running,
    /// It stayed in a call on another thread for this long.
    Busy(Duration),
    /// It trapped this many times, and is stopped until reloaded.
    Stopped(u32),
}

impl Refused {
    /// Why, in words, for the plugin `name`.
    fn said(&self, name: &str) -> String {
        match self {
            Self::Running => format!(
                "plugin `{name}` is already running further up this call: a plugin is never \
                 called back while it runs"
            ),
            Self::Busy(wait) => {
                format!("plugin `{name}` stayed busy with another call for {wait:?}")
            }
            Self::Stopped(traps) => format!(
                "plugin `{name}` was stopped after trapping {traps} times and must be reloaded"
            ),
        }
    }
}

/// The instance, if it is alive, how often it has trapped, and whether a call has it.
struct Slot {
    /// `None` once a call has trapped: a trapped component instance cannot be entered again.
    /// Also `None` while a call has taken it out to run.
    live: Option<Inner>,
    traps: u32,
    /// A call is running. Others wait on [`Instance::freed`].
    busy: bool,
    /// Events that arrived while a call was running, oldest first, for when it returns.
    missed: VecDeque<Heard>,
    /// A thread is handing `missed` over. Another call that ends leaves it to that thread.
    draining: bool,
}

/// A call's hold on the instance. Dropping it, even while unwinding, puts the instance back
/// and wakes the next caller.
struct Claim<'instance> {
    instance: &'instance Instance,
    live: Option<Inner>,
    traps: u32,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let mut slot = self.instance.slot();
        slot.live = self.live.take();
        slot.traps = self.traps;
        slot.busy = false;
        drop(slot);
        self.instance.freed.notify_all();
    }
}

/// A plugin's instance, shared by everything the plugin registered.
///
/// The instance lives exactly as long as those registrations: when the plugin is unloaded
/// its tools, listeners, prompt part, gate and services are removed, the last reference
/// goes, and the instance's memory is freed with it.
///
/// One call runs at a time. A call that would enter the instance while it is already on this
/// thread's chain of calls (the plugin calling its own tool, or A calling B calling A) is
/// refused at once, since waiting would never end. A call made from inside another plugin's
/// call waits at most [`Limits::nested_wait`] for a call on another thread to finish, so
/// that two chains waiting on each other give up rather than deadlock. Only a call from
/// outside every plugin waits for as long as it takes.
///
/// An event that arrives for the plugin while it is in a call is not lost: it is kept, up to
/// [`Limits::missed_events`] of them, and handed over when the call returns, in the order
/// the events arrived. They outlive a trap, and the instance started in its place hears them;
/// a plugin that is stopped hears nothing more.
///
/// A call that traps kills the instance. The next call makes a new one from the blueprint
/// and activates it with the plugin's configuration again, so the plugin starts from fresh
/// memory and keeps its registrations. After [`Limits::restarts`] restarts, the next trap
/// stops the plugin for good: every call fails until the plugin is reloaded.
pub(super) struct Instance {
    id: u64,
    blueprint: Blueprint,
    slot: Mutex<Slot>,
    /// Signalled when a call puts the instance back.
    freed: Condvar,
    events: Arc<EventStream>,
}

impl Instance {
    pub(super) fn new(blueprint: Blueprint, inner: Inner, events: Arc<EventStream>) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            blueprint,
            slot: Mutex::new(Slot {
                live: Some(inner),
                traps: 0,
                busy: false,
                missed: VecDeque::new(),
                draining: false,
            }),
            freed: Condvar::new(),
            events,
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.blueprint.name
    }

    /// Whether the event `id` is so far down a chain of causes that this plugin must not be
    /// called for it.
    pub(super) fn too_deep(&self, id: EventId) -> bool {
        let limit = self.blueprint.limits.cascade;
        self.events.depth(id, limit) >= limit
    }

    /// The stream this plugin's events go to.
    pub(super) const fn events(&self) -> &Arc<EventStream> {
        &self.events
    }

    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Call into the plugin with a fresh fuel budget, first restarting it if an earlier call
    /// trapped and the restart limit allows.
    ///
    /// Events the plugin raised, and those the host raises about it, are delivered after the
    /// call has returned and the instance is put back, so a listener may call straight back
    /// into this plugin.
    ///
    /// # Errors
    ///
    /// Returns why, if the call would re-enter the plugin, waited too long for it, or the
    /// plugin trapped now, could not be restarted, or was stopped after trapping too often.
    pub(super) fn call<T>(
        &self,
        call: impl FnOnce(&bindings::Plugin, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> Result<T, String> {
        self.call_caused(None, call)
    }

    /// As [`call`](Self::call), for a call made because of the event `cause`: every event the
    /// plugin raises during it names `cause` as its cause, so that a chain of plugins reacting
    /// to each other can be measured and cut off.
    pub(super) fn call_caused<T>(
        &self,
        cause: Option<EventId>,
        call: impl FnOnce(&bindings::Plugin, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> Result<T, String> {
        let claim = self.claim().map_err(|refused| refused.said(self.name()))?;
        self.run(claim, cause, call)
    }

    /// Run `call` in the instance `claim` holds, put the instance back, commit what the
    /// call raised, and hand over the events that arrived meanwhile.
    fn run<T>(
        &self,
        mut claim: Claim<'_>,
        cause: Option<EventId>,
        call: impl FnOnce(&bindings::Plugin, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> Result<T, String> {
        let link = Link::push(self.id);
        let mut raised = Vec::new();
        let result = self.revive(&mut claim, &mut raised).and_then(|mut inner| {
            let ran = inner.run(self.blueprint.limits.fuel_per_call, call);
            raised.extend(ran.events);
            match ran.result {
                Ok(value) => {
                    keep_writes(&self.blueprint.lent, ran.pending, ran.writes, &mut raised);
                    claim.live = Some(inner);
                    Ok(value)
                }
                Err(error) => Err(self.trapped(&mut claim.traps, &error, &mut raised)),
            }
        });
        drop(link);
        drop(claim);
        if let Some(cause) = cause {
            raised = raised.into_iter().map(|draft| draft.cause(cause)).collect();
        }
        // What a plugin raises while serving a tool call belongs to that call's session.
        if let Some(scope) = corduroy_core::call::current() {
            raised = raised
                .into_iter()
                .map(|draft| draft.session(scope.session.clone()))
                .collect();
        }
        // The plugin's call has returned, so there is nobody to hand a refused commit to. The
        // stream is closed by then and the next commit of the run reports it.
        drop(self.events.commit(raised));
        self.drain();
        result
    }

    /// Hand the plugin an event: now if it is free, and once the call it is in has returned
    /// if it is not, so that a plugin does not miss an event because it was busy.
    pub(super) fn hear(&self, heard: Heard) {
        self.offer(heard, false);
    }

    /// Call `on-event` with `heard`, or keep it for when the plugin is free: at the front of
    /// what is kept if `first`. Returns whether it was kept.
    fn offer(&self, heard: Heard, first: bool) -> bool {
        match self.claim() {
            Ok(claim) => {
                self.handle(claim, heard);
                false
            }
            Err(Refused::Stopped(_)) => false,
            Err(Refused::Running | Refused::Busy(_)) => {
                self.keep(heard, first);
                true
            }
        }
    }

    /// Call `on-event` with `heard` in the instance `claim` holds.
    fn handle(&self, claim: Claim<'_>, heard: Heard) {
        let Heard {
            cause,
            session,
            event,
        } = heard;
        // A trap is already on the event stream as `plugin/trapped`; a listener has nobody
        // to return it to.
        let handle = || {
            let heard = |plugin: &bindings::Plugin, store: &mut Store<HostState>| {
                plugin.call_on_event(store, &event)
            };
            drop(self.run(claim, Some(cause), heard));
        };
        // An event of a session is handled for that session: the plugin can ask which it
        // is, and what it raises belongs to it.
        match session {
            Some(session) => drop(Serve::session(session).run(handle)),
            None => handle(),
        }
    }

    /// Keep `heard` for when the call the plugin is in returns. When as many are kept as the
    /// limit allows, the oldest goes, and the stream says so.
    fn keep(&self, heard: Heard, first: bool) {
        let mut slot = self.slot();
        if first {
            slot.missed.push_front(heard);
        } else {
            slot.missed.push_back(heard);
        }
        let dropped = if slot.missed.len() > self.blueprint.limits.missed_events {
            slot.missed.pop_front()
        } else {
            None
        };
        drop(slot);
        // The record of a missed event is not itself counted as missed: a plugin that
        // listens to everything would be told of each one's loss for ever.
        if let Some(dropped) = dropped.filter(|dropped| dropped.event.topic != MISSED) {
            let missed = self
                .host_event(MISSED, &format!("\"event\":{}", dropped.cause.get()))
                .cause(dropped.cause);
            drop(self.events.emit(missed));
        }
    }

    /// Hand the plugin the events kept while it was in a call, oldest first, until none is
    /// left or the plugin is in another call, whose end hands over the rest.
    fn drain(&self) {
        {
            let mut slot = self.slot();
            if slot.draining || slot.missed.is_empty() {
                return;
            }
            slot.draining = true;
        }
        while let Some(next) = self.next_missed() {
            // Kept again, and the plugin is in a call: its end hands over the rest.
            if self.offer(next, true) && self.left_to_the_call() {
                return;
            }
        }
    }

    /// The oldest event kept, for the thread that hands them over; with none left, that
    /// thread is done.
    fn next_missed(&self) -> Option<Heard> {
        let mut slot = self.slot();
        let next = slot.missed.pop_front();
        if next.is_none() {
            slot.draining = false;
        }
        next
    }

    /// Whether a call has the instance, to whose end the events kept are then left. If it
    /// was freed in the meantime nobody else will hand them over, and this thread goes on.
    fn left_to_the_call(&self) -> bool {
        let mut slot = self.slot();
        let busy = slot.busy;
        if busy {
            slot.draining = false;
        }
        busy
    }

    /// Take the instance for one call, waiting for any call running on another thread.
    fn claim(&self) -> Result<Claim<'_>, Refused> {
        let (reentry, outermost) = chain_has(self.id);
        if reentry {
            return Err(Refused::Running);
        }
        let slot = self.slot();
        let mut slot = if outermost {
            self.freed
                .wait_while(slot, |slot| slot.busy)
                .unwrap_or_else(PoisonError::into_inner)
        } else {
            let wait = self.blueprint.limits.nested_wait;
            let (slot, _timeout) = self
                .freed
                .wait_timeout_while(slot, wait, |slot| slot.busy)
                .unwrap_or_else(PoisonError::into_inner);
            if slot.busy {
                return Err(Refused::Busy(wait));
            }
            slot
        };
        if slot.live.is_none() && slot.traps > self.blueprint.limits.restarts {
            // A stopped plugin hears nothing more, of what was kept for it either.
            slot.missed.clear();
            return Err(Refused::Stopped(slot.traps));
        }
        slot.busy = true;
        Ok(Claim {
            instance: self,
            live: slot.live.take(),
            traps: slot.traps,
        })
    }

    /// The claimed instance, or a new one if the last call trapped.
    fn revive(&self, claim: &mut Claim<'_>, raised: &mut Vec<Draft>) -> Result<Inner, String> {
        if let Some(inner) = claim.live.take() {
            return Ok(inner);
        }
        let mut inner = self.blueprint.instantiate()?;
        let ran = inner.run(self.blueprint.limits.fuel_per_call, |plugin, store| {
            plugin.call_activate(store, &self.blueprint.config)
        });
        raised.extend(ran.events);
        match ran.result {
            Err(error) => return Err(self.trapped(&mut claim.traps, &error, raised)),
            Ok(Err(reason)) => {
                let error = wasmtime::Error::msg(format!("refused to restart: {reason}"));
                return Err(self.trapped(&mut claim.traps, &error, raised));
            }
            Ok(Ok(_info)) => keep_writes(&self.blueprint.lent, ran.pending, ran.writes, raised),
        }
        raised.push(self.host_event("plugin/restarted", &format!("\"attempt\":{}", claim.traps)));
        Ok(inner)
    }

    /// Count a trap with `error` and say so on the stream. Returns the message for the
    /// caller.
    fn trapped(&self, traps: &mut u32, error: &wasmtime::Error, raised: &mut Vec<Draft>) -> String {
        let reason = format!("{error:#}");
        *traps = traps.saturating_add(1);
        raised.push(self.host_event(
            "plugin/trapped",
            &format!("\"reason\":{},\"traps\":{traps}", json::string(&reason)),
        ));
        if *traps > self.blueprint.limits.restarts {
            raised.push(self.host_event("plugin/stopped", &format!("\"traps\":{traps}")));
        }
        format!("plugin `{}` trapped: {reason}", self.name())
    }

    /// An event from the host about this plugin, with `fields` after its name.
    fn host_event(&self, topic: &str, fields: &str) -> Draft {
        let payload = format!("{{\"plugin\":{},{fields}}}", json::string(self.name()));
        Draft::new(Source::Host, topic).payload(payload)
    }
}
