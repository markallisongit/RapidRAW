//! The registry of publish destinations.
//!
//! A plain `Vec<Arc<dyn PublishDestination>>` behind an explicit constructor,
//! rather than link-time registration magic: [`PublishRegistry::new`] is the
//! single site where a destination is registered, which keeps the list
//! greppable and gives a fork one obvious line to change.
//!
//! It also holds the cancel flag of the session in progress. The registry is
//! the one piece of publish state `AppState` carries, and keeping the flag
//! beside it costs upstream's struct no second field.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::publish::smugmug::SmugMugDestination;
use crate::publish::{PublishDestination, PublishError};

/// Two destinations claimed the same [`PublishDestination::id`]. A programming
/// error in [`PublishRegistry::new`], not something a user can cause.
#[derive(Debug, PartialEq, Eq)]
pub struct DuplicateDestinationId(pub &'static str);

impl std::fmt::Display for DuplicateDestinationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "duplicate publish destination id: {}", self.0)
    }
}

impl std::error::Error for DuplicateDestinationId {}

pub struct PublishRegistry {
    destinations: Vec<Arc<dyn PublishDestination>>,
    /// The cancel flag of the running session, if one is running. One at a
    /// time: two sessions would share the export pipeline's single task slot
    /// and could race each other's writes to the same state file.
    active_session: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

impl PublishRegistry {
    /// The registered destinations.
    pub fn new() -> Self {
        Self::new_with(vec![Arc::new(SmugMugDestination::new())])
    }

    /// Panics on a duplicate id, which can only be a mistake in [`Self::new`].
    pub fn new_with(destinations: Vec<Arc<dyn PublishDestination>>) -> Self {
        Self::try_new_with(destinations).expect("publish destination ids must be unique")
    }

    pub fn try_new_with(
        destinations: Vec<Arc<dyn PublishDestination>>,
    ) -> Result<Self, DuplicateDestinationId> {
        let mut seen = HashSet::with_capacity(destinations.len());
        for destination in &destinations {
            if !seen.insert(destination.id()) {
                return Err(DuplicateDestinationId(destination.id()));
            }
        }
        Ok(Self {
            destinations,
            active_session: Arc::new(Mutex::new(None)),
        })
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn PublishDestination>> {
        self.destinations.iter().find(|d| d.id() == id)
    }

    pub fn all(&self) -> &[Arc<dyn PublishDestination>] {
        &self.destinations
    }

    /// Claims the session slot. The slot is released when the guard drops, so
    /// a session that panics does not block every publish after it.
    pub fn begin_session(&self) -> Result<SessionGuard, PublishError> {
        let mut active = self.active_session.lock().map_err(|_| poisoned())?;
        if active.is_some() {
            return Err(PublishError::Rejected(
                "a publish is already in progress".into(),
            ));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *active = Some(Arc::clone(&cancel));
        Ok(SessionGuard {
            slot: Arc::clone(&self.active_session),
            cancel,
        })
    }

    /// Asks the running session to stop. `false` when none is running.
    pub fn cancel_session(&self) -> Result<bool, PublishError> {
        let active = self.active_session.lock().map_err(|_| poisoned())?;
        Ok(match active.as_ref() {
            Some(cancel) => {
                cancel.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        })
    }
}

/// Holds the session slot for as long as a session runs.
pub struct SessionGuard {
    slot: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    cancel: Arc<AtomicBool>,
}

impl SessionGuard {
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        // A poisoned slot is still cleared: a later publish must not be
        // refused forever because an earlier one panicked.
        let mut active = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        *active = None;
    }
}

fn poisoned() -> PublishError {
    PublishError::Io("the publish session lock was poisoned".into())
}

impl Default for PublishRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use crate::publish::PublishDestination;
    use crate::publish::registry::PublishRegistry;
    use crate::publish::types::{
        AuthChallenge, AuthStatus, DestinationCapabilities, LocalContainer, PublishContext,
        PublishError, PublishItem, RemoteContainerId, RemoteImageId,
    };

    struct StubDestination;

    #[async_trait::async_trait]
    impl PublishDestination for StubDestination {
        fn id(&self) -> &'static str {
            "stub"
        }

        fn display_name(&self) -> &'static str {
            "Stub"
        }

        fn capabilities(&self) -> DestinationCapabilities {
            DestinationCapabilities {
                supports_replace: true,
                supports_reconcile: true,
                supports_nested_containers: false,
                max_bytes: None,
                accepted_mime_types: &["image/jpeg"],
            }
        }

        async fn auth_status(&self, _ctx: &PublishContext) -> Result<AuthStatus, PublishError> {
            unimplemented!()
        }

        async fn begin_auth(&self, _ctx: &PublishContext) -> Result<AuthChallenge, PublishError> {
            unimplemented!()
        }

        async fn complete_auth(
            &self,
            _verifier: &str,
            _ctx: &PublishContext,
        ) -> Result<(), PublishError> {
            unimplemented!()
        }

        async fn ensure_container(
            &self,
            _local: &LocalContainer,
            _ctx: &PublishContext,
        ) -> Result<RemoteContainerId, PublishError> {
            unimplemented!()
        }

        async fn publish_image(
            &self,
            _item: &PublishItem<'_>,
            _ctx: &PublishContext,
        ) -> Result<RemoteImageId, PublishError> {
            unimplemented!()
        }

        async fn reconcile(
            &self,
            _container: &RemoteContainerId,
            _expected: &[PublishItem<'_>],
            _ctx: &PublishContext,
        ) -> Result<Vec<(String, RemoteImageId)>, PublishError> {
            unimplemented!()
        }
    }

    #[test]
    fn registry_looks_up_by_id() {
        let registry = PublishRegistry::new_with(vec![Arc::new(StubDestination)]);
        assert_eq!(registry.get("stub").unwrap().display_name(), "Stub");
        assert!(registry.get("nope").is_none());
        assert_eq!(registry.all().len(), 1);
    }

    #[test]
    fn smugmug_is_registered() {
        assert!(PublishRegistry::new().get("smugmug").is_some());
    }

    #[test]
    fn only_one_session_runs_at_a_time() {
        let registry = PublishRegistry::new_with(Vec::new());
        let guard = registry.begin_session().unwrap();
        assert!(registry.begin_session().is_err());

        drop(guard);
        assert!(
            registry.begin_session().is_ok(),
            "the slot frees when the session ends"
        );
    }

    #[test]
    fn cancelling_reaches_the_running_session_only() {
        let registry = PublishRegistry::new_with(Vec::new());
        assert!(!registry.cancel_session().unwrap(), "nothing to cancel");

        let guard = registry.begin_session().unwrap();
        let cancel = guard.cancel_flag();
        assert!(registry.cancel_session().unwrap());
        assert!(cancel.load(Ordering::SeqCst));

        drop(guard);
        let next = registry.begin_session().unwrap();
        assert!(
            !next.cancel_flag().load(Ordering::SeqCst),
            "a new session does not inherit the last one's cancel"
        );
    }

    #[test]
    fn registry_rejects_duplicate_ids() {
        let result = PublishRegistry::try_new_with(vec![
            Arc::new(StubDestination),
            Arc::new(StubDestination),
        ]);
        assert!(result.is_err());
    }
}
