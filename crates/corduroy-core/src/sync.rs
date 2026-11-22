//! How the core locks its own shared state: registries, the event stream, the plugin
//! requests queue and, without `std`, the [call scope](crate::call).
//!
//! Nothing here is public; this is what an embedder needs to know. With the `std` feature the
//! lock is the standard library's mutex. Without it, it is a
//! [critical section](https://docs.rs/critical-section): whoever builds for the device (its
//! HAL, or the extension made for it) supplies the implementation, so the core itself needs no
//! `unsafe` and no operating system. A critical section may mask interrupts. The core holds
//! the lock only for its own bookkeeping and while the stream's [`Log`](crate::Log) stores a
//! commit, never while it calls a plugin or a listener, so a log for a device should store a
//! commit quickly or hand it on.
//!
//! The lock is not re-entrant: taking it again while it is held deadlocks with `std` and
//! panics without it. The core never does.

#[cfg(not(feature = "std"))]
use core::cell::RefCell;
// With both features the standard library's lock wins; the dependency is still there.
use core::fmt;
#[cfg(all(feature = "std", feature = "critical-section"))]
use critical_section as _;

/// A value shared between threads, reached only inside [`with`](Self::with).
pub(crate) struct Lock<T> {
    #[cfg(feature = "std")]
    inner: std::sync::Mutex<T>,
    #[cfg(not(feature = "std"))]
    inner: critical_section::Mutex<RefCell<T>>,
}

impl<T> Lock<T> {
    /// A lock holding `value`.
    pub(crate) const fn new(value: T) -> Self {
        Self {
            #[cfg(feature = "std")]
            inner: std::sync::Mutex::new(value),
            #[cfg(not(feature = "std"))]
            inner: critical_section::Mutex::new(RefCell::new(value)),
        }
    }

    /// Run `work` with the value, holding the lock until it returns.
    #[cfg(feature = "std")]
    pub(crate) fn with<R>(&self, work: impl FnOnce(&mut T) -> R) -> R {
        // Every mutation the core makes under this lock completes before anything that could
        // panic runs, so a poisoned lock holds consistent state. Recover it.
        let mut value = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        work(&mut value)
    }

    /// Run `work` with the value, inside a critical section until it returns.
    #[cfg(not(feature = "std"))]
    pub(crate) fn with<R>(&self, work: impl FnOnce(&mut T) -> R) -> R {
        critical_section::with(|section| work(&mut self.inner.borrow_ref_mut(section)))
    }
}

impl<T: Default> Default for Lock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> fmt::Debug for Lock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lock").finish_non_exhaustive()
    }
}
