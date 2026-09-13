//! Login throttling.
//!
//! The pilot has a single owner and a single password, so an unthrottled login endpoint
//! is an online guessing oracle — and, because verifying a password costs an Argon2
//! hash, also a cheap way to burn the machine's CPU.
//!
//! Two mechanisms, both of which must hold under *concurrent* bursts, not just
//! sequential attempts:
//!
//! 1. [`LoginThrottle::reserve`] counts an attempt **before** the password is verified
//!    and refuses once the limit is reached. Counting afterwards would let an arbitrary
//!    number of simultaneous requests pass the check while the counter is still zero.
//!    A successful sign-in clears the counter again ([`LoginThrottle::record_success`]),
//!    so the owner is not punished for one typo followed by the right password.
//! 2. [`LoginThrottle::hash_slot`] bounds how many Argon2 verifications run at once, so
//!    a burst from many addresses cannot schedule unbounded blocking work.
//!
//! State is in memory on purpose: it must not survive a restart as a lockout the owner
//! cannot clear, and there is one process.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use otdel_core::config::LoginThrottle as ThrottleConfig;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Simultaneous Argon2 verifications. The local pilot has one owner; anything above a
/// couple of parallel hashes is a burst, not normal use.
const MAX_CONCURRENT_PASSWORD_HASHES: usize = 2;

#[derive(Debug, Clone, Copy)]
struct Attempts {
    /// Attempts started (not merely failed) inside the current window.
    started: u32,
    window_started: Instant,
    locked_until: Option<Instant>,
}

#[derive(Debug)]
pub struct LoginThrottle {
    config: ThrottleConfig,
    state: Mutex<HashMap<String, Attempts>>,
    hash_slots: Arc<Semaphore>,
}

impl LoginThrottle {
    pub fn new(config: ThrottleConfig) -> Self {
        Self {
            config,
            state: Mutex::new(HashMap::new()),
            hash_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_PASSWORD_HASHES)),
        }
    }

    /// Count one attempt and decide whether it may proceed.
    ///
    /// `Err(retry_after)` means the caller is locked out and the password must not be
    /// verified at all. The attempt is counted *now*: N concurrent requests consume N
    /// slots, so the configured limit bounds a burst and not only a sequence.
    pub fn reserve(&self, key: &str) -> Result<(), Duration> {
        self.reserve_at(key, Instant::now())
    }

    /// A correct password clears the counter (and any lockout) for that client.
    pub fn record_success(&self, key: &str) {
        let mut state = self.lock();
        state.remove(key);
    }

    /// Wait for permission to run one Argon2 verification.
    ///
    /// The permit is **owned** so it can be moved into the blocking task that actually
    /// hashes. A permit borrowed by the request future would be released as soon as the
    /// client disconnects and the future is dropped — while the Argon2 computation, once
    /// handed to `spawn_blocking`, keeps running and keeps using the CPU. Capacity must
    /// track the real work, not the request that asked for it.
    pub async fn hash_permit(&self) -> OwnedSemaphorePermit {
        Arc::clone(&self.hash_slots)
            .acquire_owned()
            .await
            .expect("login hash semaphore is never closed")
    }

    fn reserve_at(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let mut state = self.lock();
        state.retain(|_, attempts| !Self::is_stale(attempts, now, self.config.window));

        let entry = state.entry(key.to_owned()).or_insert(Attempts {
            started: 0,
            window_started: now,
            locked_until: None,
        });

        if let Some(locked_until) = entry.locked_until {
            if locked_until > now {
                return Err(locked_until - now);
            }
            // Lockout served: start a fresh window.
            *entry = Attempts {
                started: 0,
                window_started: now,
                locked_until: None,
            };
        }

        if now.duration_since(entry.window_started) > self.config.window {
            entry.started = 0;
            entry.window_started = now;
        }

        if entry.started >= self.config.max_failures {
            entry.locked_until = Some(now + self.config.lockout);
            return Err(self.config.lockout);
        }

        entry.started += 1;
        Ok(())
    }

    fn is_stale(attempts: &Attempts, now: Instant, window: Duration) -> bool {
        let lock_expired = attempts
            .locked_until
            .is_none_or(|locked_until| locked_until <= now);
        lock_expired && now.duration_since(attempts.window_started) > window
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Attempts>> {
        // A poisoned mutex only means a previous thread panicked while counting login
        // attempts; the counter is not worth propagating a panic for.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn throttle(max_failures: u32) -> LoginThrottle {
        LoginThrottle::new(ThrottleConfig {
            max_failures,
            window: Duration::from_secs(900),
            lockout: Duration::from_secs(300),
        })
    }

    #[test]
    fn attempts_are_counted_before_verification() {
        let throttle = throttle(3);
        let now = Instant::now();

        assert!(throttle.reserve_at("a", now).is_ok());
        assert!(throttle.reserve_at("a", now).is_ok());
        assert!(throttle.reserve_at("a", now).is_ok());

        let retry_after = throttle.reserve_at("a", now).unwrap_err();
        assert_eq!(retry_after, Duration::from_secs(300));

        // A different client is unaffected.
        assert!(throttle.reserve_at("b", now).is_ok());
    }

    #[test]
    fn lockout_expires_and_success_clears_the_counter() {
        let throttle = throttle(2);
        let now = Instant::now();

        assert!(throttle.reserve_at("a", now).is_ok());
        assert!(throttle.reserve_at("a", now).is_ok());
        assert!(throttle.reserve_at("a", now).is_err());

        let later = now + Duration::from_secs(301);
        assert!(throttle.reserve_at("a", later).is_ok());

        throttle.record_success("a");
        // The counter (and the lockout) are gone after a correct password.
        assert!(throttle.reserve_at("a", later).is_ok());
        assert!(throttle.reserve_at("a", later).is_ok());
    }

    #[test]
    fn attempts_outside_the_window_do_not_accumulate() {
        let throttle = throttle(3);
        let now = Instant::now();

        assert!(throttle.reserve_at("a", now).is_ok());
        assert!(throttle.reserve_at("a", now).is_ok());

        // Far beyond the 900 s window: the counter restarts instead of locking out.
        let later = now + Duration::from_secs(1_000);
        assert!(throttle.reserve_at("a", later).is_ok());
        assert!(throttle.reserve_at("a", later).is_ok());
        assert!(throttle.reserve_at("a", later).is_ok());
        assert!(throttle.reserve_at("a", later).is_err());
    }

    /// Regression for the burst case: the limit must hold when many requests arrive at
    /// once, not only when they arrive one after another.
    #[test]
    fn concurrent_burst_is_bounded_by_the_limit() {
        let throttle = Arc::new(throttle(10));
        let admitted = Arc::new(AtomicU32::new(0));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let throttle = Arc::clone(&throttle);
            let admitted = Arc::clone(&admitted);
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    if throttle.reserve("burst").is_ok() {
                        admitted.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            admitted.load(Ordering::SeqCst),
            10,
            "400 concurrent attempts must not admit more than the configured limit"
        );
    }

    #[tokio::test]
    async fn hash_slots_are_bounded() {
        let throttle = throttle(10);
        let first = throttle.hash_permit().await;
        let second = throttle.hash_permit().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), throttle.hash_permit())
                .await
                .is_err(),
            "a third concurrent password hash must wait"
        );
        drop(first);
        drop(second);
        // Slots are released again.
        let _third = throttle.hash_permit().await;
    }

    /// Regression: a cancelled request must not hand its hashing slot back while the
    /// Argon2 computation it started is still running.
    ///
    /// The permit is moved into the blocking closure, so dropping the request future
    /// (client disconnect, timeout) cannot release capacity early — only the end of the
    /// real work does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cancelled_request_keeps_its_slot_until_the_hash_finishes() {
        let throttle = Arc::new(throttle(10));
        let work_finished = Arc::new(AtomicU32::new(0));

        // Occupy one of the two slots for the whole test.
        let _occupied = throttle.hash_permit().await;

        let request = {
            let throttle = Arc::clone(&throttle);
            let work_finished = Arc::clone(&work_finished);
            tokio::spawn(async move {
                let permit = throttle.hash_permit().await;
                let finished = Arc::clone(&work_finished);
                tokio::task::spawn_blocking(move || {
                    // Stands in for the Argon2 verification.
                    std::thread::sleep(Duration::from_millis(400));
                    finished.fetch_add(1, Ordering::SeqCst);
                    drop(permit);
                })
                .await
                .unwrap();
            })
        };

        // Let the blocking work start, then cancel the request that spawned it.
        tokio::time::sleep(Duration::from_millis(100)).await;
        request.abort();
        let _ = request.await;

        // The slot is still taken, because the hashing thread still holds the permit.
        assert_eq!(work_finished.load(Ordering::SeqCst), 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), throttle.hash_permit())
                .await
                .is_err(),
            "a cancelled request must not release the slot while the hash is still running"
        );

        // Once the work finishes, capacity comes back.
        let regained = tokio::time::timeout(Duration::from_secs(2), throttle.hash_permit())
            .await
            .expect("the slot must be released when the hashing work ends");
        assert_eq!(work_finished.load(Ordering::SeqCst), 1);
        drop(regained);
    }
}
