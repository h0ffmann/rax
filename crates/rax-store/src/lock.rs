//! Exclusive locks per resource kind, atespace and name.
//!
//! When the server runs with more than one replica, every replica must use a
//! [`Locker`] backed by the same shared system, such as
//! [`PostgresStore`](crate::PostgresStore). [`MemoryLocker`] serves a single
//! process.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::OwnedMutexGuard;

use crate::{Result, default_atespace};

/// Formats the key a lock is held under.
pub fn lock_key(kind: &str, atespace: &str, name: &str) -> String {
    format!("lock:{kind}:{}:{name}", default_atespace(atespace))
}

#[async_trait]
pub trait Locker: Send + Sync {
    /// Acquires an exclusive lock for the resource, waiting until it is free.
    /// Dropping the returned future stops waiting.
    async fn lock(&self, kind: &str, atespace: &str, name: &str) -> Result<LockGuard>;
}

pub(crate) type ReleaseFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A held lock. [`LockGuard::release`] releases it and waits until that is
/// done; dropping the guard releases it in the background.
pub struct LockGuard {
    release: Option<Box<dyn FnOnce() -> ReleaseFuture + Send>>,
}

impl LockGuard {
    pub(crate) fn new(release: impl FnOnce() -> ReleaseFuture + Send + 'static) -> Self {
        Self {
            release: Some(Box::new(release)),
        }
    }

    /// Releases the lock and waits until another holder can take it.
    pub async fn release(mut self) {
        if let Some(release) = self.release.take() {
            release().await;
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let fut = release();
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(fut);
            }
        }
    }
}

type Entries = Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;

/// An in-memory keyed mutex for tests and single-process runs.
#[derive(Default)]
pub struct MemoryLocker {
    entries: Entries,
}

impl MemoryLocker {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

/// A reference to one key's mutex. Dropping the last reference outside the
/// map removes the key, so the map only holds keys that are locked or waited
/// on.
struct EntryRef {
    entries: Entries,
    key: String,
    mutex: Option<Arc<tokio::sync::Mutex<()>>>,
}

impl Drop for EntryRef {
    fn drop(&mut self) {
        drop(self.mutex.take());
        let mut entries = self.entries.lock().unwrap();
        if entries
            .get(&self.key)
            .is_some_and(|m| Arc::strong_count(m) == 1)
        {
            entries.remove(&self.key);
        }
    }
}

#[async_trait]
impl Locker for MemoryLocker {
    async fn lock(&self, kind: &str, atespace: &str, name: &str) -> Result<LockGuard> {
        let key = lock_key(kind, atespace, name);
        let mutex = self
            .entries
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_default()
            .clone();
        let entry = EntryRef {
            entries: self.entries.clone(),
            key,
            mutex: Some(mutex.clone()),
        };
        let guard: OwnedMutexGuard<()> = mutex.lock_owned().await;
        Ok(LockGuard::new(move || {
            // Unlock first, then let go of the entry.
            drop(guard);
            drop(entry);
            Box::pin(async {})
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn serializes_same_resource() {
        let locker = Arc::new(MemoryLocker::new());
        let active = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let (locker, active, max) = (locker.clone(), active.clone(), max.clone());
            handles.push(tokio::spawn(async move {
                let guard = locker.lock("task", "default", "same").await.unwrap();
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                guard.release().await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            max.load(Ordering::SeqCst),
            1,
            "more than one holder at a time"
        );
        assert_eq!(locker.len(), 0, "released keys must be removed");
    }

    #[tokio::test]
    async fn different_resources_run_concurrently() {
        let locker = MemoryLocker::new();
        let a = locker.lock("task", "default", "a").await.unwrap();
        let b =
            tokio::time::timeout(Duration::from_secs(1), locker.lock("task", "default", "b")).await;
        assert!(b.is_ok(), "a different resource must not wait");
        // The same name in another kind or atespace is a different resource.
        let _c = locker.lock("workspace", "default", "a").await.unwrap();
        let _d = locker.lock("task", "other", "a").await.unwrap();
        drop(a);
    }

    #[tokio::test]
    async fn waiting_can_be_cancelled() {
        let locker = MemoryLocker::new();
        let held = locker.lock("task", "", "t").await.unwrap();
        let waited = tokio::time::timeout(
            Duration::from_millis(20),
            locker.lock("task", "default", "t"),
        )
        .await;
        assert!(waited.is_err(), "lock must wait while it is held");
        held.release().await;
        assert_eq!(
            locker.len(),
            0,
            "a cancelled waiter must not leave its key behind"
        );
        // Dropping a guard releases it too.
        drop(locker.lock("task", "default", "t").await.unwrap());
        let again =
            tokio::time::timeout(Duration::from_secs(1), locker.lock("task", "default", "t")).await;
        assert!(again.is_ok());
    }

    #[test]
    fn keys() {
        assert_eq!(lock_key("task", "", "a"), "lock:task:default:a");
        assert_eq!(lock_key("model", "team", "m"), "lock:model:team:m");
    }
}
