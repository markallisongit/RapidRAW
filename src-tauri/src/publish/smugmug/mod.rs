//! SmugMug as a publish destination: the trait implementation over
//! [`auth`], [`api`] and [`upload`], which hold the protocol detail.

pub mod api;
pub mod auth;
pub mod model;
pub mod upload;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::publish::oauth1::Credentials;
use crate::publish::smugmug::api::SmugMugApi;
use crate::publish::smugmug::auth::{
    SmugMugAuth, account_key, authorize_url, exchange_verifier, fetch_nickname,
    fetch_request_token, normalize_verifier,
};
use crate::publish::smugmug::model::{ChildNode, TokenPair};
use crate::publish::smugmug::upload::SmugMugUploader;
use crate::publish::state::PublishState;
use crate::publish::{
    AuthChallenge, AuthStatus, ConsumerCredentials, ContainerPrivacy, ContainerSnapshot,
    DestinationCapabilities, PublishContext, PublishDestination, PublishError, PublishItem,
    RemoteContainerId, RemoteImage, RemoteImageId, RemoteNode, RemoteNodeId, RemoteNodeKind,
};

/// Also the name of the state file, so it must not change once shipped.
pub const DESTINATION_ID: &str = "smugmug";

pub struct SmugMugDestination {
    /// The temporary credentials from [`PublishDestination::begin_auth`],
    /// held until the user pastes the verifier. In memory only: they are
    /// worthless without the verifier and expire on their own, so writing
    /// them to the keyring would add exposure for no gain. A restart
    /// mid-dance means starting the dance again, which is the right outcome.
    pending: Mutex<Option<TokenPair>>,
    /// The signed clients for the connected account, built on first use
    /// rather than per call: every image would otherwise cost a state-file
    /// read and a keyring lookup, and the uploader would lose the throughput
    /// its timeouts are sized from.
    connection: Mutex<Option<Arc<Connection>>>,
}

/// Signed clients for one consumer and one access token.
struct Connection {
    creds: Credentials,
    api: SmugMugApi,
    uploader: SmugMugUploader,
}

impl SmugMugDestination {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(None),
            connection: Mutex::new(None),
        }
    }

    /// The nickname of the connected account, which names its token entry.
    ///
    /// Falls back to the state file's account for an install connected before
    /// the keyring recorded it, which is where phase 1 kept it.
    fn connected_nickname(ctx: &PublishContext) -> Result<Option<String>, PublishError> {
        match SmugMugAuth::connected_account()? {
            Some(nickname) => Ok(Some(nickname)),
            None => PublishState::account_in(&ctx.state_dir, DESTINATION_ID),
        }
    }

    /// Reuses the cached clients while the consumer credentials match. A new
    /// access token only arrives through [`PublishDestination::complete_auth`],
    /// which drops the cache itself.
    fn connection(&self, ctx: &PublishContext) -> Result<Arc<Connection>, PublishError> {
        let consumer = Self::consumer(ctx)?;
        let mut cached = self.connection.lock().map_err(|_| poisoned())?;
        if let Some(connection) = cached.as_ref()
            && connection.creds.consumer_key == consumer.key
            && connection.creds.consumer_secret == consumer.secret
        {
            return Ok(Arc::clone(connection));
        }

        let not_connected =
            || PublishError::NotAuthorised("no SmugMug account is connected".into());
        let nickname = Self::connected_nickname(ctx)?.ok_or_else(not_connected)?;
        let (token, token_secret) =
            SmugMugAuth::load_tokens(&account_key(&nickname))?.ok_or_else(not_connected)?;
        let creds = Credentials {
            consumer_key: consumer.key.clone(),
            consumer_secret: consumer.secret.clone(),
            token: Some(token),
            token_secret: Some(token_secret),
        };
        let connection = Arc::new(Connection {
            api: SmugMugApi::new(creds.clone())?,
            uploader: SmugMugUploader::new(creds.clone())?,
            creds,
        });
        *cached = Some(Arc::clone(&connection));
        Ok(connection)
    }

    /// Stores a freshly authorised account's tokens and makes it the connected
    /// one. Never touches the state file: its recorded account says whom the
    /// links belong to, which reconnecting as someone else must not rewrite.
    fn remember_connection(
        &self,
        ctx: &PublishContext,
        nickname: &str,
        access: &TokenPair,
    ) -> Result<(), PublishError> {
        let previous = Self::connected_nickname(ctx)?;

        // Tokens before the pointer: a pointer naming an account whose tokens
        // were never stored would report Connected and then fail every call.
        SmugMugAuth::store_tokens(&account_key(nickname), &access.token, &access.token_secret)?;
        SmugMugAuth::set_connected_account(Some(nickname))?;
        *self.connection.lock().map_err(|_| poisoned())? = None;

        // One account at a time: the previous one's token would otherwise sit
        // in the keyring, and resurface through the phase 1 fallback.
        match previous {
            Some(previous) if previous != nickname => {
                SmugMugAuth::delete_tokens(&account_key(&previous))
            }
            _ => Ok(()),
        }
    }

    fn consumer(ctx: &PublishContext) -> Result<&ConsumerCredentials, PublishError> {
        ctx.consumer.as_ref().ok_or_else(|| {
            PublishError::NotAuthorised(
                "no SmugMug API key and secret have been entered yet".into(),
            )
        })
    }
}

impl Default for SmugMugDestination {
    fn default() -> Self {
        Self::new()
    }
}

fn poisoned() -> PublishError {
    PublishError::Io("the SmugMug destination lock was poisoned".into())
}

/// A folder or an album as the Publish Manager lists it. `None` for a page,
/// which holds no photos, and for an album without an album URI, which could
/// not be linked or uploaded into.
fn remote_node(node: &ChildNode) -> Option<RemoteNode> {
    let (kind, container) = match node.node_type.as_str() {
        "Folder" => (RemoteNodeKind::Folder, None),
        "Album" => (
            RemoteNodeKind::Album,
            Some(RemoteContainerId(node.album_uri()?.to_string())),
        ),
        _ => return None,
    };
    Some(RemoteNode {
        id: RemoteNodeId(node.uri.clone()),
        container,
        kind,
        name: node.name.clone(),
        web_url: node.web_uri.clone(),
        has_children: node.has_children,
    })
}

/// [`remote_node`], for a node that has to be a usable album.
fn album_node(node: &ChildNode) -> Result<RemoteNode, PublishError> {
    remote_node(node)
        .filter(|remote| remote.container.is_some())
        .ok_or_else(|| {
            PublishError::Rejected(format!(
                "the SmugMug node \"{}\" is not an album that can be published to",
                node.name
            ))
        })
}

/// SmugMug appends a numeric URI suffix that can change when an image is
/// replaced (`…/ImageKey-0`, `…/ImageKey-1`, …). The image key before that
/// suffix is the stable identity; the fresh full URI is still retained for
/// the next replacement request.
fn image_identity(image: &RemoteImageId) -> String {
    let locator = image.0.rsplit('/').next().unwrap_or(image.0.as_str());
    match locator.rsplit_once('-') {
        Some((identity, revision))
            if !revision.is_empty() && revision.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            identity.to_string()
        }
        _ => locator.to_string(),
    }
}

#[async_trait]
impl PublishDestination for SmugMugDestination {
    fn id(&self) -> &'static str {
        DESTINATION_ID
    }

    fn display_name(&self) -> &'static str {
        "SmugMug"
    }

    fn capabilities(&self) -> DestinationCapabilities {
        DestinationCapabilities {
            supports_replace: true,
            supports_reconcile: true,
            // Albums sit in folders on SmugMug.
            supports_nested_containers: true,
            // SmugMug's per-file ceiling varies by plan and is not published
            // as one number, so the upload reports the server's own rejection
            // rather than guessing at a limit here.
            max_bytes: None,
            // Deliberately narrow: these are the two formats the upload path
            // is exercised against. Widening it belongs with that task.
            accepted_mime_types: &["image/jpeg", "image/png"],
            // SmugMug's own three levels, which map one to one.
            supported_privacy: &[
                ContainerPrivacy::Public,
                ContainerPrivacy::Unlisted,
                ContainerPrivacy::Private,
            ],
        }
    }

    /// Three states, distinguished by what is missing: no consumer
    /// credentials, no token, or connected. A keyring that cannot be reached
    /// is an error rather than "not connected" — the difference matters,
    /// because reconnecting would not fix it.
    async fn auth_status(&self, ctx: &PublishContext) -> Result<AuthStatus, PublishError> {
        if ctx.consumer.is_none() {
            return Ok(AuthStatus::NotConfigured);
        }
        let Some(nickname) = Self::connected_nickname(ctx)? else {
            return Ok(AuthStatus::NotAuthorised);
        };
        match SmugMugAuth::load_tokens(&account_key(&nickname))? {
            Some(_) => Ok(AuthStatus::Connected { account: nickname }),
            None => Ok(AuthStatus::NotAuthorised),
        }
    }

    async fn begin_auth(&self, ctx: &PublishContext) -> Result<AuthChallenge, PublishError> {
        let temporary = fetch_request_token(Self::consumer(ctx)?).await?;
        let authorize_url = authorize_url(&temporary.token);
        *self.pending.lock().map_err(|_| poisoned())? = Some(temporary);

        Ok(AuthChallenge {
            authorize_url,
            instructions_key: "publish.smugmug.authInstructions",
        })
    }

    /// Takes the temporary credentials rather than borrowing them: a verifier
    /// can only be spent once, and a failed exchange needs a fresh dance.
    async fn complete_auth(
        &self,
        verifier: &str,
        ctx: &PublishContext,
    ) -> Result<(), PublishError> {
        let consumer = Self::consumer(ctx)?;
        let verifier = normalize_verifier(verifier)?;
        let temporary = self
            .pending
            .lock()
            .map_err(|_| poisoned())?
            .take()
            .ok_or_else(|| {
                PublishError::NotAuthorised(
                    "the authorisation was not started, or has already been used".into(),
                )
            })?;

        let access = exchange_verifier(consumer, &temporary, &verifier).await?;
        let nickname = fetch_nickname(consumer, &access).await?;

        self.remember_connection(ctx, &nickname, &access)
    }

    /// The pointer goes last, so a disconnect that fails part way can simply
    /// be repeated: the account it names is still the one to forget.
    async fn disconnect(&self, ctx: &PublishContext) -> Result<(), PublishError> {
        *self.connection.lock().map_err(|_| poisoned())? = None;
        if let Some(nickname) = Self::connected_nickname(ctx)? {
            SmugMugAuth::delete_tokens(&account_key(&nickname))?;
        }
        SmugMugAuth::set_connected_account(None)
    }

    async fn list_containers(
        &self,
        parent: Option<&RemoteNodeId>,
        ctx: &PublishContext,
    ) -> Result<Vec<RemoteNode>, PublishError> {
        let connection = self.connection(ctx)?;
        let parent = match parent {
            Some(parent) => parent.0.clone(),
            None => connection.api.auth_user().await?.node_uri,
        };
        let children = connection.api.list_children(&parent).await?;
        Ok(children.iter().filter_map(remote_node).collect())
    }

    async fn find_container(
        &self,
        name: &str,
        ctx: &PublishContext,
    ) -> Result<Option<RemoteNode>, PublishError> {
        let connection = self.connection(ctx)?;
        let root = connection.api.auth_user().await?.node_uri;
        match connection.api.find_child_album(&root, name).await? {
            Some(node) => album_node(&node).map(Some),
            None => Ok(None),
        }
    }

    async fn create_container(
        &self,
        name: &str,
        ctx: &PublishContext,
    ) -> Result<RemoteNode, PublishError> {
        let connection = self.connection(ctx)?;
        let root = connection.api.auth_user().await?.node_uri;
        let node = connection
            .api
            .create_album(&root, name, ctx.new_container_privacy)
            .await?;
        album_node(&node)
    }

    /// Read from the album itself, so a link to one inside a folder costs no
    /// walk of the tree.
    async fn container(
        &self,
        id: &RemoteContainerId,
        ctx: &PublishContext,
    ) -> Result<RemoteNode, PublishError> {
        let album = self.connection(ctx)?.api.album(id).await?;
        Ok(RemoteNode {
            id: RemoteNodeId(album.node_uri),
            container: Some(RemoteContainerId(album.uri)),
            kind: RemoteNodeKind::Album,
            name: album.name,
            web_url: album.web_uri,
            has_children: false,
        })
    }

    async fn inspect_container(
        &self,
        container: &RemoteContainerId,
        ctx: &PublishContext,
    ) -> Result<Option<ContainerSnapshot>, PublishError> {
        self.connection(ctx)?.api.inspect_container(container).await
    }

    async fn list_container_images(
        &self,
        container: &RemoteContainerId,
        ctx: &PublishContext,
    ) -> Result<Vec<RemoteImage>, PublishError> {
        let images = self
            .connection(ctx)?
            .api
            .list_album_images(container)
            .await?;
        Ok(images
            .into_iter()
            .map(|image| RemoteImage {
                id: image.image_uri,
                file_name: image.file_name,
            })
            .collect())
    }

    fn image_identity(&self, image: &RemoteImageId) -> String {
        image_identity(image)
    }

    async fn publish_image(
        &self,
        item: &PublishItem<'_>,
        ctx: &PublishContext,
    ) -> Result<RemoteImageId, PublishError> {
        self.connection(ctx)?
            .uploader
            .upload(item, &ctx.cancel)
            .await
    }

    async fn reconcile(
        &self,
        container: &RemoteContainerId,
        expected: &[PublishItem<'_>],
        ctx: &PublishContext,
    ) -> Result<Vec<(String, RemoteImageId)>, PublishError> {
        let connection = self.connection(ctx)?;
        upload::reconcile(&connection.api, container, expected).await
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::publish::credential_store;
    use crate::publish::state::AlbumMembership;

    fn context(state_dir: &Path) -> PublishContext {
        PublishContext {
            state_dir: state_dir.to_path_buf(),
            consumer: Some(ConsumerCredentials {
                key: "consumer-key".into(),
                secret: "consumer-secret".into(),
            }),
            cancel: Arc::new(AtomicBool::new(false)),
            new_container_privacy: ContainerPrivacy::Public,
        }
    }

    fn tokens(name: &str) -> TokenPair {
        TokenPair {
            token: format!("{name}-token"),
            token_secret: format!("{name}-secret"),
        }
    }

    /// A state whose links belong to `account`.
    fn linked_state(dir: &Path, account: &str) {
        let mut state = PublishState::empty(DESTINATION_ID);
        state.record_link(
            "iceland",
            &RemoteContainerId("/api/v2/album/Ice".into()),
            None,
        );
        state.claim_account(account).unwrap();
        state.save_in(dir).unwrap();
    }

    fn recorded_account(dir: &Path) -> Option<String> {
        PublishState::load_in(dir, DESTINATION_ID, &AlbumMembership::default())
            .unwrap()
            .account()
            .map(str::to_string)
    }

    async fn status(destination: &SmugMugDestination, ctx: &PublishContext) -> String {
        match destination.auth_status(ctx).await.unwrap() {
            AuthStatus::Connected { account } => format!("Connected as {account}"),
            other => format!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnect_forgets_the_token_and_keeps_the_consumer_and_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let destination = SmugMugDestination::new();
        credential_store::store_consumer(DESTINATION_ID, ctx.consumer.as_ref().unwrap()).unwrap();
        linked_state(dir.path(), "alice");
        destination
            .remember_connection(&ctx, "alice", &tokens("alice"))
            .unwrap();
        assert_eq!(status(&destination, &ctx).await, "Connected as alice");
        let state_before = std::fs::read(dir.path().join("smugmug.json")).unwrap();

        destination.disconnect(&ctx).await.unwrap();

        assert_eq!(status(&destination, &ctx).await, "NotAuthorised");
        assert_eq!(
            SmugMugAuth::load_tokens(&account_key("alice")).unwrap(),
            None
        );
        assert_eq!(
            credential_store::load_consumer(DESTINATION_ID)
                .unwrap()
                .map(|c| c.key),
            Some("consumer-key".into())
        );
        assert_eq!(
            std::fs::read(dir.path().join("smugmug.json")).unwrap(),
            state_before
        );
        assert!(
            destination.connection(&ctx).is_err(),
            "no cached client outlives the token"
        );
        destination
            .disconnect(&ctx)
            .await
            .expect("disconnecting twice is harmless");
    }

    #[tokio::test]
    async fn reconnecting_the_same_account_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let destination = SmugMugDestination::new();
        linked_state(dir.path(), "alice");
        destination
            .remember_connection(&ctx, "alice", &tokens("alice"))
            .unwrap();

        destination.disconnect(&ctx).await.unwrap();
        destination
            .remember_connection(&ctx, "alice", &tokens("alice-again"))
            .unwrap();

        assert_eq!(status(&destination, &ctx).await, "Connected as alice");
        assert_eq!(recorded_account(dir.path()).as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn connecting_another_account_leaves_the_recorded_owner_and_drops_the_old_token() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let destination = SmugMugDestination::new();
        linked_state(dir.path(), "alice");
        destination
            .remember_connection(&ctx, "alice", &tokens("alice"))
            .unwrap();

        destination
            .remember_connection(&ctx, "bob", &tokens("bob"))
            .unwrap();

        assert_eq!(status(&destination, &ctx).await, "Connected as bob");
        assert_eq!(
            recorded_account(dir.path()).as_deref(),
            Some("alice"),
            "the links still belong to alice, so the publish guard can refuse bob"
        );
        assert_eq!(
            SmugMugAuth::load_tokens(&account_key("alice")).unwrap(),
            None
        );
    }

    /// Phase 1 named the connected account only in the state file.
    #[tokio::test]
    async fn an_install_connected_before_the_keyring_pointer_stays_connected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let destination = SmugMugDestination::new();
        linked_state(dir.path(), "alice");
        SmugMugAuth::store_tokens(&account_key("alice"), "t", "s").unwrap();

        assert_eq!(status(&destination, &ctx).await, "Connected as alice");

        destination.disconnect(&ctx).await.unwrap();
        assert_eq!(status(&destination, &ctx).await, "NotAuthorised");
    }

    #[test]
    fn image_identity_ignores_only_smugmugs_numeric_uri_revision() {
        assert_eq!(
            image_identity(&RemoteImageId("/api/v2/image/XyZ123-0".into())),
            "XyZ123"
        );
        assert_eq!(
            image_identity(&RemoteImageId("/api/v2/image/XyZ123-12".into())),
            "XyZ123"
        );
        assert_eq!(
            image_identity(&RemoteImageId("/api/v2/album/AbCdEf/image/XyZ123-2".into())),
            "XyZ123",
            "SmugMug also returns the album-image form from upload calls"
        );
        assert_eq!(
            image_identity(&RemoteImageId("/api/v2/image/key-final".into())),
            "key-final"
        );
    }

    #[test]
    fn folders_and_albums_are_listed_and_anything_else_is_not() {
        let body = serde_json::json!({
            "Response": { "Node": [
                {
                    "Name": "Travel", "Type": "Folder", "Uri": "/api/v2/node/trv",
                    "WebUri": "https://x.smugmug.com/Travel", "HasChildren": true
                },
                {
                    "Name": "Iceland", "Type": "Album", "Uri": "/api/v2/node/ice",
                    "WebUri": "https://x.smugmug.com/Iceland",
                    "Uris": { "Album": { "Uri": "/api/v2/album/Ice" } }
                },
                { "Name": "About", "Type": "Page", "Uri": "/api/v2/node/abt" },
                { "Name": "Broken", "Type": "Album", "Uri": "/api/v2/node/brk" }
            ]},
            "Code": 200
        })
        .to_string();
        let nodes = model::parse_node_children(&body).unwrap().nodes;

        let listed: Vec<RemoteNode> = nodes.iter().filter_map(remote_node).collect();

        assert_eq!(
            listed,
            [
                RemoteNode {
                    id: RemoteNodeId("/api/v2/node/trv".into()),
                    container: None,
                    kind: RemoteNodeKind::Folder,
                    name: "Travel".into(),
                    web_url: Some("https://x.smugmug.com/Travel".into()),
                    has_children: true,
                },
                RemoteNode {
                    id: RemoteNodeId("/api/v2/node/ice".into()),
                    container: Some(RemoteContainerId("/api/v2/album/Ice".into())),
                    kind: RemoteNodeKind::Album,
                    name: "Iceland".into(),
                    web_url: Some("https://x.smugmug.com/Iceland".into()),
                    has_children: false,
                },
            ],
            "a page holds no photos, and an album with no album URI cannot be linked"
        );
    }

    #[test]
    fn smugmug_containers_nest_in_folders() {
        assert!(
            SmugMugDestination::new()
                .capabilities()
                .supports_nested_containers,
            "without folders, albums inside one could never be browsed to"
        );
    }
}
