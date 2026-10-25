//! The participant's native defaults, and the only place it reads the platform: a
//! monotonic clock, futures-timer's `Delay` and the process id. **Native only**, declared
//! behind `cfg(not(target_arch = "wasm32"))` in `lib.rs`: each of these compiles on
//! wasm32-unknown-unknown and fails there at runtime, so on wasm32 an asker has no default
//! clock or timer and the host installs them (`tests/wasm_clean.rs` holds the line).
//!
//! Monotonic on purpose. Core's [`SystemClock`](ikigai_core::SystemClock) reads the wall
//! clock, which can step backward or forward (NTP, a laptop waking), and an asker's deadline
//! measured on it would stretch or vanish when it does. This clock is anchored once at the
//! wall clock, so its readings are still milliseconds since the Unix epoch (a correlation id
//! sorts by them), and then advances only by `Instant`'s elapsed time: what the asker
//! measured its deadline with before it read a clock, kept.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ikigai_core::{Clock, Time};

use crate::timer::Timer;

/// futures-timer's `Delay`: runtime-agnostic, so it works under any executor, `block_on`
/// included.
pub(crate) struct NativeTimer;

#[async_trait::async_trait]
impl Timer for NativeTimer {
    async fn sleep(&self, delay: Duration) {
        futures_timer::Delay::new(delay).await;
    }
}

/// A [`Clock`] that never runs backward: the wall clock's reading when it was made,
/// advanced by the monotonic time elapsed since.
pub(crate) struct Monotonic {
    origin: Instant,
    epoch: u64,
}

impl Monotonic {
    pub(crate) fn new() -> Self {
        Monotonic {
            origin: Instant::now(),
            epoch: millis(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default(),
            ),
        }
    }
}

impl Clock for Monotonic {
    fn now(&self) -> Time {
        Time::from_millis(self.epoch.saturating_add(millis(self.origin.elapsed())))
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// This process's id.
pub(crate) fn process_tag() -> u64 {
    u64::from(std::process::id())
}
