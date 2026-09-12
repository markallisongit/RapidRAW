// Nothing calls into this module yet: the session driver and the destination
// implementations that consume this trait land in later changes. Until then
// every item here is dead code as far as the binary is concerned, and the
// convenience re-exports below have no callers, though the unit tests
// exercise the rest.
#![allow(dead_code, unused_imports)]

pub mod oauth1;
pub mod registry;
pub mod smugmug;
pub mod spool;
pub mod state;
pub mod types;

use async_trait::async_trait;

pub use registry::{DuplicateDestinationId, PublishRegistry};
pub use types::{
    AuthChallenge, AuthStatus, ConsumerCredentials, DestinationCapabilities, LocalContainer,
    PublishContext, PublishError, PublishItem, RemoteContainerId, RemoteImageId,
};

/// A place photos can be published to.
///
/// `async_trait` is required: `dyn` async traits are not object-safe without
/// boxing on Rust 1.98, and the registry stores destinations as trait objects.
#[async_trait]
pub trait PublishDestination: Send + Sync {
    /// Stable, machine-readable, used as the state file name. Never shown.
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn capabilities(&self) -> DestinationCapabilities;

    async fn auth_status(&self, ctx: &PublishContext) -> Result<AuthStatus, PublishError>;
    async fn begin_auth(&self, ctx: &PublishContext) -> Result<AuthChallenge, PublishError>;
    async fn complete_auth(&self, verifier: &str, ctx: &PublishContext)
    -> Result<(), PublishError>;

    /// Idempotent.
    async fn ensure_container(
        &self,
        local: &LocalContainer,
        ctx: &PublishContext,
    ) -> Result<RemoteContainerId, PublishError>;

    async fn publish_image(
        &self,
        item: &PublishItem<'_>,
        ctx: &PublishContext,
    ) -> Result<RemoteImageId, PublishError>;

    /// After an ambiguous failure, ask the destination what actually landed.
    /// Returns the file name and remote id of each expected item that is
    /// present in the container.
    async fn reconcile(
        &self,
        container: &RemoteContainerId,
        expected: &[PublishItem<'_>],
        ctx: &PublishContext,
    ) -> Result<Vec<(String, RemoteImageId)>, PublishError>;
}
