//! One project store per app session, open while someone uses it.
//!
//! The store takes a writer lock: two writers in one project is an error. A turn
//! used to open the store, run, and close it. A delegated child that outlives the
//! turn that started it (prime-agent's children run apart from the parent's turn)
//! needs the store after that close, so the turn and its children share one open
//! store instead. Each user holds a [`StoreLease`]; the store opens with the
//! first lease and closes when the last one is dropped, so an idle app still
//! releases the lock for another terminal, as it did when a turn closed it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{HarnessError, HostId};

/// How long a lease waits for a store another holder is still closing.
const OPEN_RETRIES: u32 = 30;
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(100);

/// The session's project store, shared by its turns and its children.
pub struct SharedStore {
    dir: PathBuf,
    slot: tokio::sync::Mutex<Slot>,
}

#[derive(Default)]
struct Slot {
    store: Option<Arc<SqliteStore>>,
    leases: usize,
}

impl SharedStore {
    #[must_use]
    pub fn new(dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            dir,
            slot: tokio::sync::Mutex::new(Slot::default()),
        })
    }

    /// A lease on the open store, opening it if nobody holds one.
    ///
    /// # Errors
    /// The store cannot be opened: another terminal holds the writer lock, or the
    /// directory is unusable.
    pub async fn lease(self: &Arc<Self>) -> Result<StoreLease, HarnessError> {
        let mut slot = self.slot.lock().await;
        if slot.store.is_none() {
            slot.store = Some(Arc::new(self.open().await?));
        }
        slot.leases += 1;
        let store = slot.store.as_ref().map(Arc::clone);
        Ok(StoreLease {
            store,
            owner: Arc::clone(self),
        })
    }

    /// Whether the store is open now: some lease is held or is being released.
    #[cfg(test)]
    pub async fn is_open(&self) -> bool {
        self.slot.lock().await.store.is_some()
    }

    async fn open(&self) -> Result<SqliteStore, HarnessError> {
        let mut attempt = 0;
        loop {
            match SqliteStore::open_writer(WriterOpenOptions::new(
                self.dir.clone(),
                HostId::generate(),
            ))
            .await
            {
                Ok(store) => return Ok(store),
                // A store dropped a moment ago may still hold its lock while its
                // last handle goes away; a short wait is what that takes.
                Err(_) if attempt < OPEN_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(OPEN_RETRY_DELAY).await;
                }
                Err(error) => {
                    return Err(HarnessError::new(error.code(), error.to_string()));
                }
            }
        }
    }

    /// One lease ended: close the store when it was the last.
    async fn release(self: Arc<Self>) {
        let mut slot = self.slot.lock().await;
        slot.leases = slot.leases.saturating_sub(1);
        if slot.leases > 0 {
            return;
        }
        if let Some(store) = slot.store.take() {
            // Another handle to the store (a task still finishing) keeps it open
            // until it drops; the lock goes with the last handle.
            if let Ok(store) = Arc::try_unwrap(store) {
                let _ = store.close().await;
            }
        }
    }
}

/// A holder's use of the shared store. Dropping it ends the use.
pub struct StoreLease {
    store: Option<Arc<SqliteStore>>,
    owner: Arc<SharedStore>,
}

impl StoreLease {
    /// The open store.
    ///
    /// # Panics
    /// Never: the store is only taken when the lease is dropped.
    #[must_use]
    pub fn store(&self) -> Arc<SqliteStore> {
        Arc::clone(self.store.as_ref().expect("a live lease holds the store"))
    }
}

impl Drop for StoreLease {
    fn drop(&mut self) {
        // The lease's own handle goes first, so the release can close the store.
        drop(self.store.take());
        let owner = Arc::clone(&self.owner);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(owner.release());
        } else if let Ok(mut slot) = owner.slot.try_lock() {
            slot.leases = slot.leases.saturating_sub(1);
            if slot.leases == 0 {
                slot.store = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SharedStore;

    /// Two holders share one open store, and the last one to leave closes it:
    /// another writer can open the project afterwards.
    #[tokio::test]
    async fn q01_a_lease_opens_once_and_closes_after_the_last_drop() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let shared = SharedStore::new(temporary.path().join("store"));
        let first = shared.lease().await.expect("first lease opens the store");
        let second = shared.lease().await.expect("second lease shares it");
        assert!(
            std::sync::Arc::ptr_eq(&first.store(), &second.store()),
            "one store for both holders"
        );
        drop(first);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(shared.is_open().await, "still open while a lease is held");
        drop(second);
        for _ in 0..50 {
            if !shared.is_open().await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!shared.is_open().await, "closed after the last lease");
        let other = harness_store_sqlite::SqliteStore::open_writer(
            harness_store_sqlite::WriterOpenOptions::new(
                temporary.path().join("store"),
                harness_types::HostId::generate(),
            ),
        )
        .await
        .expect("the writer lock was released");
        let _ = other.close().await;
    }
}
