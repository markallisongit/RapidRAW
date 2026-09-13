pub mod commands;
pub mod credential_store;
pub mod links;
pub mod oauth1;
pub mod registry;
pub mod session;
pub mod settings;
pub mod smugmug;
pub mod spool;
pub mod state;
pub mod types;

use async_trait::async_trait;

pub use registry::PublishRegistry;
pub use types::{
    AuthChallenge, AuthStatus, ConsumerCredentials, ContainerPrivacy, DestinationCapabilities,
    LocalContainer, PublishContext, PublishError, PublishItem, RemoteContainerId, RemoteImageId,
    RemoteNode, RemoteNodeId, RemoteNodeKind,
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

    /// Forgets the access token, leaving the consumer credentials, the state
    /// and its links alone, so reconnecting the same account resumes where
    /// it left off. Idempotent.
    async fn disconnect(&self, ctx: &PublishContext) -> Result<(), PublishError>;

    /// One level of the destination's container tree, every page. Folders
    /// appear only when `capabilities().supports_nested_containers`. `None` is
    /// the account root.
    async fn list_containers(
        &self,
        parent: Option<&RemoteNodeId>,
        ctx: &PublishContext,
    ) -> Result<Vec<RemoteNode>, PublishError>;

    /// A container directly under the root with exactly this name, if any.
    async fn find_container(
        &self,
        name: &str,
        ctx: &PublishContext,
    ) -> Result<Option<RemoteNode>, PublishError>;

    /// Creates a container directly under the root, with
    /// [`PublishContext::new_container_privacy`]. Never reuses one of the same
    /// name: that is [`find_container`](Self::find_container)'s to report.
    async fn create_container(
        &self,
        name: &str,
        ctx: &PublishContext,
    ) -> Result<RemoteNode, PublishError>;

    /// One container, read and never modified.
    async fn container(
        &self,
        id: &RemoteContainerId,
        ctx: &PublishContext,
    ) -> Result<RemoteNode, PublishError>;

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
