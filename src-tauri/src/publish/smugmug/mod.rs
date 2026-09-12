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
use crate::publish::smugmug::model::TokenPair;
use crate::publish::smugmug::upload::SmugMugUploader;
use crate::publish::state::PublishState;
use crate::publish::{
    AuthChallenge, AuthStatus, ConsumerCredentials, DestinationCapabilities, LocalContainer,
    PublishContext, PublishDestination, PublishError, PublishItem, RemoteContainerId,
    RemoteImageId,
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

    /// The nickname of the account this install last connected, which names
    /// both the keyring entry and the state file's id map.
    fn connected_nickname(ctx: &PublishContext) -> Result<Option<String>, PublishError> {
        Ok(PublishState::load_in(&ctx.state_dir, DESTINATION_ID)?
            .account()
            .map(str::to_string))
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

/// Phase 1 does not mirror the group tree, so an album's groups are folded
/// into its SmugMug name: "Travel" › "Iceland" publishes as "Travel - Iceland".
fn flattened_name(local: &LocalContainer) -> String {
    local
        .parent_path
        .iter()
        .chain(std::iter::once(&local.name))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" - ")
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
            // Phase 1 flattens the local hierarchy into the album name.
            supports_nested_containers: false,
            // SmugMug's per-file ceiling varies by plan and is not published
            // as one number, so the upload reports the server's own rejection
            // rather than guessing at a limit here.
            max_bytes: None,
            // Deliberately narrow: these are the two formats the upload path
            // is exercised against. Widening it belongs with that task.
            accepted_mime_types: &["image/jpeg", "image/png"],
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

        // Keyring first: a state file naming an account whose tokens were
        // never stored would report Connected and then fail on every call.
        SmugMugAuth::store_tokens(&account_key(&nickname), &access.token, &access.token_secret)?;
        *self.connection.lock().map_err(|_| poisoned())? = None;

        let mut state = PublishState::load_in(&ctx.state_dir, DESTINATION_ID)?;
        state.set_account(Some(nickname));
        state.save_in(&ctx.state_dir)
    }

    /// Albums go directly under the account's root node until the panel
    /// offers a choice of folder.
    async fn ensure_container(
        &self,
        local: &LocalContainer,
        ctx: &PublishContext,
    ) -> Result<RemoteContainerId, PublishError> {
        let connection = self.connection(ctx)?;
        let root = connection.api.auth_user().await?.node_uri;
        connection
            .api
            .ensure_album(&root, &flattened_name(local))
            .await
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
    use super::*;

    #[test]
    fn groups_are_folded_into_the_album_name() {
        let local = |parent_path: &[&str]| LocalContainer {
            album_id: "a".into(),
            name: "Iceland".into(),
            parent_path: parent_path.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(flattened_name(&local(&[])), "Iceland");
        assert_eq!(
            flattened_name(&local(&["Travel", "2026"])),
            "Travel - 2026 - Iceland"
        );
    }
}
