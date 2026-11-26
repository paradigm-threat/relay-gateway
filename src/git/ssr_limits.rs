//! SSR concurrency, wall-clock timeout with cooperative cancel, and per-key coalesce.
//!
//! Env:
//! - `RELAY_SSR_TIMEOUT_SECS` — default **30** (clamp 2..=120). `0` disables the wall-clock timeout
//!   (concurrency limit + coalesce still apply).
//! - `RELAY_SSR_MAX_CONCURRENT` — default 2 (clamp 1..=64).
//!
//! On timeout the waiting request fails fast **and** signals [`relay_runtime::signal_ssr_cancel`]
//! so the worker aborts hot loops. The concurrency permit is released at that signal, not when
//! the thread finally exits, so a cancelled render does not keep the next reader on the busy path.
//!
//! Same coalesce key ⇒ one leader thread; waiters share the leader result under the same deadline.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn timeout_secs() -> Option<u64> {
    match std::env::var("RELAY_SSR_TIMEOUT_SECS") {
        Ok(s) if s.trim() == "0" => None,
        Ok(s) => {
            let n: u64 = s.trim().parse().unwrap_or(30);
            Some(n.clamp(2, 120))
        }
        Err(_) => Some(30),
    }
}

fn max_concurrent() -> usize {
    std::env::var("RELAY_SSR_MAX_CONCURRENT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(2)
        .clamp(1, 64)
}

static INFLIGHT: AtomicUsize = AtomicUsize::new(0);
/// Workers still running after their waiter deadline (cooperative cancel in flight).
static OVERDUE: AtomicUsize = AtomicUsize::new(0);

/// True when a new SSR leader would fail-fast (slots full).
pub fn ssr_slots_full() -> bool {
    INFLIGHT.load(Ordering::Acquire) >= max_concurrent()
}

/// Count of timed-out workers that have not yet dropped their permit.
pub fn ssr_overdue() -> usize {
    OVERDUE.load(Ordering::Acquire)
}

/// Shared so the waiter can drop the inflight count at cancel time.
/// `Drop` and [`release_counted`] are idempotent.
struct Permit {
    counted: Arc<AtomicBool>,
}

fn release_counted(counted: &AtomicBool) {
    if counted.swap(false, Ordering::AcqRel) {
        INFLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        release_counted(&self.counted);
    }
}

fn try_acquire_permit() -> Option<Permit> {
    let max = max_concurrent();
    loop {
        let cur = INFLIGHT.load(Ordering::Acquire);
        if cur >= max {
            return None;
        }
        if INFLIGHT
            .compare_exchange(cur, cur + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Some(Permit {
                counted: Arc::new(AtomicBool::new(true)),
            });
        }
    }
}

struct ErasedSlot {
    cancel: Arc<AtomicBool>,
    /// Permit flag for the leader. Set before the slot is visible to joiners.
    counted: Mutex<Option<Arc<AtomicBool>>>,
    waiter_count: AtomicUsize,
}

struct TypedWaiters<T> {
    senders: Mutex<Vec<Sender<Option<T>>>>,
}

/// Process-wide (or test-local) coalesce map for a concrete result type.
pub struct CoalesceRegistry<T: Clone + Send> {
    map: Mutex<HashMap<String, (Arc<ErasedSlot>, Arc<TypedWaiters<T>>)>>,
}

impl<T: Clone + Send + 'static> CoalesceRegistry<T> {
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl<T: Clone + Send + 'static> Default for CoalesceRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}

fn await_with_timeout<T>(
    rx: Receiver<Option<T>>,
    timeout: Option<u64>,
    cancel: &Arc<AtomicBool>,
    counted: &Arc<AtomicBool>,
) -> Option<T> {
    match timeout {
        None => rx.recv().ok().flatten(),
        Some(secs) => match rx.recv_timeout(Duration::from_secs(secs)) {
            Ok(v) => v,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let already = cancel.load(Ordering::Acquire);
                relay_runtime::signal_ssr_cancel(cancel);
                // The thread may still be unwinding. New readers must not wait on it.
                release_counted(counted);
                if !already {
                    OVERDUE.fetch_add(1, Ordering::AcqRel);
                }
                tracing::warn!(
                    target: "relay_ssr",
                    timeout_secs = secs,
                    overdue = ssr_overdue(),
                    inflight = INFLIGHT.load(Ordering::Acquire),
                    "SSR wall-clock timeout; cancel signaled and concurrency slot released"
                );
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        },
    }
}

fn wake_all_none<T: Clone + Send>(waiters: &TypedWaiters<T>) {
    wake_all_value(waiters, &None);
}

fn wake_all_value<T: Clone + Send>(waiters: &TypedWaiters<T>, out: &Option<T>) {
    if let Ok(mut s) = waiters.senders.lock() {
        for tx in s.drain(..) {
            let _ = tx.send(out.clone());
        }
    }
}

fn spawn_leader<T, F>(
    permit: Permit,
    cancel: Arc<AtomicBool>,
    waiters: Arc<TypedWaiters<T>>,
    key: String,
    registry: Arc<CoalesceRegistry<T>>,
    work: F,
) -> bool
where
    T: Clone + Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let builder = std::thread::Builder::new().name("relay-ssr".into());
    let cancel_worker = cancel.clone();
    let key_worker = key.clone();
    let registry_worker = registry.clone();
    let waiters_fallback = waiters.clone();
    let spawn_ok = builder
        .spawn(move || {
            let _permit = permit;
            relay_runtime::install_ssr_cancel(Some(cancel_worker.clone()));
            let out = if relay_runtime::ssr_is_cancelled() {
                None
            } else {
                work()
            };
            if cancel_worker.load(Ordering::Acquire) {
                OVERDUE
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                        Some(n.saturating_sub(1))
                    })
                    .ok();
            }
            relay_runtime::install_ssr_cancel(None);

            // Remove under the map lock, then fan-out so a late joiner cannot
            // insert a parallel slot between remove and drain.
            let fanout = {
                let mut map = registry_worker
                    .map
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                map.remove(&key_worker).map(|(_, tw)| tw)
            };
            let targets = fanout.unwrap_or(waiters_fallback);
            let senders: Vec<_> = {
                let mut s = targets.senders.lock().unwrap_or_else(|p| p.into_inner());
                s.drain(..).collect()
            };
            for tx in senders {
                let _ = tx.send(out.clone());
            }
        })
        .is_ok();

    if !spawn_ok {
        tracing::warn!(target: "relay_ssr", "failed to spawn SSR worker thread");
        if let Ok(mut map) = registry.map.lock() {
            if let Some((_, tw)) = map.remove(&key) {
                wake_all_none(&tw);
            }
        }
        return false;
    }
    true
}

/// Run `work` under concurrency + timeout + coalesce for `key`.
pub fn run_ssr_coalesced_with<T, F>(
    registry: Arc<CoalesceRegistry<T>>,
    key: String,
    work: F,
) -> Option<T>
where
    T: Clone + Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let timeout = timeout_secs();
    let (tx, rx) = mpsc::channel::<Option<T>>();

    {
        let mut map = registry.map.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((slot, waiters)) = map.get(&key) {
            if let Ok(mut s) = waiters.senders.lock() {
                s.push(tx);
            }
            slot.waiter_count.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(target: "relay_ssr", key = %key, "SSR coalesce join");
            let cancel = slot.cancel.clone();
            let counted = slot
                .counted
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            drop(map);
            let Some(counted) = counted else {
                return await_with_timeout(
                    rx,
                    timeout,
                    &cancel,
                    &Arc::new(AtomicBool::new(false)),
                );
            };
            return await_with_timeout(rx, timeout, &cancel, &counted);
        } else {
            let Some(permit) = try_acquire_permit() else {
                tracing::warn!(
                    target: "relay_ssr",
                    max = max_concurrent(),
                    "SSR concurrency limit reached; failing fast"
                );
                return None;
            };
            let counted = permit.counted.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let slot = Arc::new(ErasedSlot {
                cancel: cancel.clone(),
                counted: Mutex::new(Some(counted.clone())),
                waiter_count: AtomicUsize::new(1),
            });
            let waiters = Arc::new(TypedWaiters {
                senders: Mutex::new(vec![tx]),
            });
            map.insert(key.clone(), (slot, waiters.clone()));
            drop(map);
            let started = spawn_leader(
                permit,
                cancel.clone(),
                waiters,
                key.clone(),
                registry.clone(),
                work,
            );
            if !started {
                return None;
            }
            tracing::debug!(target: "relay_ssr", key = %key, "SSR coalesce leader");
            return await_with_timeout(rx, timeout, &cancel, &counted);
        }
    }
}

/// Non-coalesced isolated run (tests / single-shot). Still uses cancel on timeout.
pub fn run_ssr_isolated<T, F>(work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let Some(permit) = try_acquire_permit() else {
        tracing::warn!(
            target: "relay_ssr",
            max = max_concurrent(),
            "SSR concurrency limit reached; failing fast"
        );
        return None;
    };

    let timeout = timeout_secs();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    let counted = permit.counted.clone();
    let (tx, rx) = mpsc::channel();
    let builder = std::thread::Builder::new().name("relay-ssr".into());
    if builder
        .spawn(move || {
            let _permit = permit;
            relay_runtime::install_ssr_cancel(Some(cancel_worker.clone()));
            let out = if relay_runtime::ssr_is_cancelled() {
                None
            } else {
                work()
            };
            if cancel_worker.load(Ordering::Acquire) {
                OVERDUE
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                        Some(n.saturating_sub(1))
                    })
                    .ok();
            }
            relay_runtime::install_ssr_cancel(None);
            let _ = tx.send(out);
        })
        .is_err()
    {
        tracing::warn!(target: "relay_ssr", "failed to spawn SSR worker thread");
        return None;
    }

    await_with_timeout(rx, timeout, &cancel, &counted)
}

/// Build coalesce key from repo/branch/tip/path/hook.
pub fn coalesce_key(
    repo_path: &std::path::Path,
    tip: &str,
    branch: &str,
    initial_path: &str,
    client_hook: &str,
) -> String {
    format!(
        "{}\0{branch}\0{tip}\0{initial_path}\0{}",
        repo_path.display(),
        client_hook.trim_start_matches('/')
    )
}

/// Coalesced SSR using a process-wide registry for `T` supplied by the caller via
/// [`run_ssr_coalesced_with`]. Prefer that when you already hold an `Arc<CoalesceRegistry>`.
///
/// Convenience for `ClientHookSsrResult`-shaped callers that pass a once-init registry
/// through this module's typed helper below is intentionally omitted to avoid cyclic deps;
/// [`crate::git::ssr_client_hook`] owns the OnceLock.
pub fn run_ssr_coalesced<T, F>(
    registry: Arc<CoalesceRegistry<T>>,
    key: String,
    work: F,
) -> Option<T>
where
    T: Clone + Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    run_ssr_coalesced_with(registry, key, work)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn concurrency_rejects_when_full() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("RELAY_SSR_MAX_CONCURRENT", "1");
        std::env::set_var("RELAY_SSR_TIMEOUT_SECS", "2");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let h = std::thread::spawn(move || {
            run_ssr_isolated(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Some(1u8)
            })
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(run_ssr_isolated(|| Some(2u8)).is_none());
        let _ = release_tx.send(());
        assert_eq!(h.join().unwrap(), Some(1));
        std::env::remove_var("RELAY_SSR_MAX_CONCURRENT");
        std::env::remove_var("RELAY_SSR_TIMEOUT_SECS");
    }

    #[test]
    fn timeout_cancel_frees_permit_promptly() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("RELAY_SSR_MAX_CONCURRENT", "1");
        std::env::set_var("RELAY_SSR_TIMEOUT_SECS", "1");
        let (started_tx, started_rx) = mpsc::channel();
        let h = std::thread::spawn(move || {
            run_ssr_isolated(move || {
                let _ = started_tx.send(());
                for _ in 0..200 {
                    if relay_runtime::ssr_is_cancelled() {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Some(1u8)
            })
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(h.join().unwrap().is_none());
        let mut ok = false;
        for _ in 0..50 {
            if run_ssr_isolated(|| Some(3u8)).is_some() {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(ok, "permit should free after cooperative cancel");
        std::env::remove_var("RELAY_SSR_MAX_CONCURRENT");
        std::env::remove_var("RELAY_SSR_TIMEOUT_SECS");
    }

    #[test]
    fn timeout_releases_slot_before_worker_exits() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("RELAY_SSR_MAX_CONCURRENT", "1");
        std::env::set_var("RELAY_SSR_TIMEOUT_SECS", "2");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let h = std::thread::spawn(move || {
            run_ssr_isolated(move || {
                let _ = started_tx.send(());
                // Ignore cancel: the waiter must free the slot without this thread exiting.
                let _ = release_rx.recv_timeout(Duration::from_secs(20));
                Some(1u8)
            })
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut finished = false;
        for _ in 0..50 {
            if h.is_finished() {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(finished, "waiter should return at the wall-clock timeout");
        assert!(h.join().unwrap().is_none());
        assert_eq!(
            run_ssr_isolated(|| Some(9u8)),
            Some(9),
            "cancelled render must not keep the only slot"
        );
        let _ = release_tx.send(());
        std::thread::sleep(Duration::from_millis(50));
        std::env::remove_var("RELAY_SSR_MAX_CONCURRENT");
        std::env::remove_var("RELAY_SSR_TIMEOUT_SECS");
    }

    #[test]
    fn coalesce_single_spawn_two_waiters() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("RELAY_SSR_MAX_CONCURRENT", "2");
        std::env::set_var("RELAY_SSR_TIMEOUT_SECS", "5");
        let reg = Arc::new(CoalesceRegistry::<u32>::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_a = calls.clone();
        let calls_b = calls.clone();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate_rx = Arc::new(Mutex::new(Some(gate_rx)));

        let reg1 = reg.clone();
        let gr1 = gate_rx.clone();
        let h1 = std::thread::spawn(move || {
            run_ssr_coalesced_with(reg1, "k".into(), move || {
                calls_a.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut g) = gr1.lock() {
                    if let Some(rx) = g.take() {
                        let _ = rx.recv();
                    }
                }
                Some(42u32)
            })
        });
        std::thread::sleep(Duration::from_millis(80));
        let reg2 = reg.clone();
        let h2 = std::thread::spawn(move || {
            run_ssr_coalesced_with(reg2, "k".into(), move || {
                calls_b.fetch_add(1, Ordering::SeqCst);
                Some(99u32)
            })
        });
        std::thread::sleep(Duration::from_millis(50));
        let _ = gate_tx.send(());
        assert_eq!(h1.join().unwrap(), Some(42));
        assert_eq!(h2.join().unwrap(), Some(42));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        std::env::remove_var("RELAY_SSR_MAX_CONCURRENT");
        std::env::remove_var("RELAY_SSR_TIMEOUT_SECS");
    }
}
