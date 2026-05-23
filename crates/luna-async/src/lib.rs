//! Cross-target async runtime façade for Luna.
//!
//! Exposes a minimal API (`spawn_local`, `sleep`, `yield_now`, plus channel
//! re-exports) backed by `tokio` on native targets and
//! `wasm-bindgen-futures` + `gloo-timers` on `wasm32-unknown-unknown`.
//!
//! Bans direct use of `tokio::*` and `crossbeam-channel` in the Luna core,
//! both of which fail (or panic at runtime) under WASM.
//!
//! # Send vs `!Send`
//!
//! Every primitive here works with `!Send` futures (the WASM single-thread
//! lowest common denominator). On native, this requires a
//! `tokio::task::LocalSet` to be running before [`spawn_local`] is called.
//! Use [`runtime::block_on_local`] in tests and in entry points.
//!
//! # Channels
//!
//! [`mpsc`] and [`oneshot`] come from `futures::channel` because they are
//! the only mainstream channel implementation that compiles **and runs**
//! identically on native and on `wasm32-unknown-unknown`. `crossbeam-channel`
//! panics on WASM (no parking primitive) and `tokio::sync::mpsc` is not
//! available there either.
//!
//! See `ARCHITECTURE.md` §4.1.

use core::future::Future;
use core::time::Duration;

pub use futures::channel::{mpsc, oneshot};

// =============================================================================
// spawn_local
// =============================================================================

/// Spawn a `!Send` future on the current local task set.
///
/// On native, this delegates to [`tokio::task::spawn_local`] and therefore
/// requires a `LocalSet` to be currently running on the calling thread
/// (use [`runtime::block_on_local`] in tests, or the helper in
/// `luna-mcp-server` in production). On WASM it delegates to
/// [`wasm_bindgen_futures::spawn_local`].
///
/// The handle is not returned; if you need to await completion, pair the
/// future with a [`oneshot`] channel.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_local<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    tokio::task::spawn_local(future);
}

/// Spawn a `!Send` future on the current event loop's microtask queue.
#[cfg(target_arch = "wasm32")]
pub fn spawn_local<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}

// =============================================================================
// sleep
// =============================================================================

/// Suspend the current task for at least `duration`.
///
/// Native: backed by `tokio::time::sleep`. WASM: backed by
/// `gloo_timers::future::TimeoutFuture` (which delegates to `setTimeout`,
/// so the minimum delay is clamped by the browser — typically 4 ms).
#[cfg(not(target_arch = "wasm32"))]
pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Suspend the current task for at least `duration` (browser-clamped).
#[cfg(target_arch = "wasm32")]
pub async fn sleep(duration: Duration) {
    // gloo_timers takes a u32 millisecond count.
    let millis = u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
    gloo_timers::future::TimeoutFuture::new(millis).await;
}

// =============================================================================
// yield_now
// =============================================================================

/// Yield control to the runtime so other tasks can make progress.
#[cfg(not(target_arch = "wasm32"))]
pub async fn yield_now() {
    tokio::task::yield_now().await;
}

/// Yield control to the JS microtask queue.
#[cfg(target_arch = "wasm32")]
pub async fn yield_now() {
    // No direct equivalent; a 0 ms timeout drops us back on the microtask
    // queue, which is what tokio::task::yield_now effectively does.
    gloo_timers::future::TimeoutFuture::new(0).await;
}

// =============================================================================
// runtime helpers (native only — WASM has no blocking primitives)
// =============================================================================

/// Native-only helpers to drive futures from synchronous code.
///
/// These exist so that binaries (`luna-cli`, `luna-mcp-server`) and tests
/// can construct a `LocalSet` without depending on `tokio` directly. On
/// `wasm32-unknown-unknown` there is no blocking equivalent — the
/// JavaScript event loop drives futures via the microtask queue, not via
/// a `block_on`-style call.
#[cfg(not(target_arch = "wasm32"))]
pub mod runtime {
    use core::future::Future;

    /// Run `future` to completion on a `current_thread` runtime wrapped in
    /// a `LocalSet`, so [`super::spawn_local`] is usable inside.
    pub fn block_on_local<F: Future>(future: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio current_thread runtime should build");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, future)
    }
}

// =============================================================================
// Tests (native only — wasm-bindgen-test would be a separate crate)
// =============================================================================

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use core::time::Duration;
    use futures::{SinkExt, StreamExt};
    use std::time::Instant;

    #[test]
    fn sleep_actually_sleeps() {
        runtime::block_on_local(async {
            let start = Instant::now();
            sleep(Duration::from_millis(40)).await;
            assert!(start.elapsed() >= Duration::from_millis(35));
        });
    }

    #[test]
    fn mpsc_round_trip() {
        runtime::block_on_local(async {
            let (mut tx, mut rx) = mpsc::channel::<u32>(4);
            tx.send(42).await.unwrap();
            assert_eq!(rx.next().await, Some(42));
        });
    }

    #[test]
    fn oneshot_round_trip() {
        runtime::block_on_local(async {
            let (tx, rx) = oneshot::channel::<&'static str>();
            tx.send("ping").unwrap();
            assert_eq!(rx.await, Ok("ping"));
        });
    }

    #[test]
    fn spawn_local_sees_the_runtime() {
        runtime::block_on_local(async {
            let (tx, rx) = oneshot::channel::<u32>();
            spawn_local(async move {
                yield_now().await;
                tx.send(7).unwrap();
            });
            assert_eq!(rx.await, Ok(7));
        });
    }

    #[test]
    fn yield_now_does_not_panic() {
        runtime::block_on_local(async {
            yield_now().await;
        });
    }
}
