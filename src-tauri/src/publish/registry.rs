//! The registry of publish destinations.
//!
//! A plain `Vec<Arc<dyn PublishDestination>>` behind an explicit constructor,
//! rather than link-time registration magic: [`PublishRegistry::new`] is the
//! single site where a destination is registered, which keeps the list
//! greppable and gives a fork one obvious line to change.

use std::collections::HashSet;
use std::sync::Arc;

use crate::publish::PublishDestination;

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
}

impl PublishRegistry {
    /// The registered destinations. Empty until the first one lands.
    pub fn new() -> Self {
        Self::new_with(Vec::new())
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
        Ok(Self { destinations })
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn PublishDestination>> {
        self.destinations.iter().find(|d| d.id() == id)
    }

    pub fn all(&self) -> &[Arc<dyn PublishDestination>] {
        &self.destinations
    }
}

impl Default for PublishRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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
    fn registry_rejects_duplicate_ids() {
        let result = PublishRegistry::try_new_with(vec![
            Arc::new(StubDestination),
            Arc::new(StubDestination),
        ]);
        assert!(result.is_err());
    }
}
