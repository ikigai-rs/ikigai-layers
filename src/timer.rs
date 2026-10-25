//! [`Timer`]: what an [`Asker`](crate::gateway::Asker) races a feed wait against, so a
//! deadline is kept even when the layer host holds the wait longer.
//!
//! A wait is not a [`Clock`]: a clock answers "what time is it" and
//! never "wake me when", so a participant that waits needs something that can suspend a
//! future. Core has no such seam, so the asker takes one, injected like its clock (the
//! shape ikigai-throttle's `Timer` set, ledger #2189): a native target defaults to
//! futures-timer, whose backend runs a helper thread; wasm32-unknown-unknown has no default.
//! A browser has timers, but only behind JavaScript imports, and a runtime host may have no
//! JavaScript at all, so the HOST supplies one (ledger #2193). An asker without a timer
//! refuses to ask, naming [`with_timer`](crate::gateway::Asker::with_timer): never a panic,
//! and never a wait with no bound.
//!
//! A browser timer's future is not `Send` and this trait's is, so a browser host resolves a
//! `setTimeout` promise in a `spawn_local` task and awaits a `oneshot` channel that the task
//! completes, which is `Send`: the same bridge a wasm host already uses for `fetch`.
//!
//! A test injects one that returns at once, so a deadline is "reached" in no real time.

use std::sync::Arc;
use std::time::Duration;

use ikigai_core::Clock;

/// Suspends the caller for a delay. Injected into an [`Asker`](crate::gateway::Asker) with
/// [`with_timer`](crate::gateway::Asker::with_timer).
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use std::time::Duration;
/// use ikigai_layers::timer::Timer;
///
/// /// A timer that records what it was asked to wait and returns at once.
/// #[derive(Default)]
/// struct Recording(Mutex<Vec<Duration>>);
///
/// #[async_trait::async_trait]
/// impl Timer for Recording {
///     async fn sleep(&self, delay: Duration) {
///         self.0.lock().unwrap().push(delay);
///     }
/// }
///
/// let timer = Arc::new(Recording::default());
/// futures::executor::block_on(timer.sleep(Duration::from_millis(250)));
/// assert_eq!(*timer.0.lock().unwrap(), [Duration::from_millis(250)]);
/// ```
#[async_trait::async_trait]
pub trait Timer: Send + Sync {
    /// Return after `delay` has passed.
    async fn sleep(&self, delay: Duration);
}

/// The timer an asker uses when its host installed none: futures-timer on a native target,
/// none on wasm32 (see the module doc).
pub(crate) fn default_timer() -> Option<Arc<dyn Timer>> {
    #[cfg(not(target_arch = "wasm32"))]
    let timer: Option<Arc<dyn Timer>> = Some(Arc::new(crate::native::NativeTimer));
    #[cfg(target_arch = "wasm32")]
    let timer: Option<Arc<dyn Timer>> = None;
    timer
}

/// The clock an asker uses when its host installed none: a monotonic one anchored at the
/// wall clock on a native target, none on wasm32, where Rust's own clocks compile and panic
/// when read, so the host installs the clock it gave its kernel.
pub(crate) fn default_clock() -> Option<Arc<dyn Clock>> {
    #[cfg(not(target_arch = "wasm32"))]
    let clock: Option<Arc<dyn Clock>> = Some(Arc::new(crate::native::Monotonic::new()));
    #[cfg(target_arch = "wasm32")]
    let clock: Option<Arc<dyn Clock>> = None;
    clock
}

/// What tells this process's correlation ids from another's minted by the same principal in
/// the same millisecond: the process id on a native target, and 0 on wasm32, where there is
/// no process to ask (a collision there is the Conflict [`Asker::ask`] already retries on a
/// fresh id).
///
/// [`Asker::ask`]: crate::gateway::Asker::ask
pub(crate) fn process_tag() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    let tag = crate::native::process_tag();
    #[cfg(target_arch = "wasm32")]
    let tag = 0;
    tag
}
