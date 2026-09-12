//! SmugMug as a publish destination.
//!
//! This change carries the authorisation half only: the destination can say
//! whether it is connected, start the out-of-band OAuth dance and finish it.
//! Albums and uploads follow in later changes, which is why the remaining
//! trait methods answer with an error rather than doing anything.

pub mod api;
pub mod auth;
pub mod model;
pub mod upload;

use std::sync::Mutex;

use async_trait::async_trait;

use crate::publish::smugmug::auth::{
    SmugMugAuth, account_key, authorize_url, exchange_verifier, fetch_nickname,
    fetch_request_token, normalize_verifier,
};
use crate::publish::smugmug::model::TokenPair;
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
}

impl SmugMugDestination {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(None),
        }
    }

    /// The nickname of the account this install last connected, which names
    /// both the keyring entry and the state file's id map.
    fn connected_nickname(ctx: &PublishContext) -> Result<Option<String>, PublishError> {
        Ok(PublishState::load_in(&ctx.state_dir, DESTINATION_ID)?
            .account()
            .map(str::to_string))
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

/// Albums and uploads land with the tasks that follow this one. An error
/// rather than a `todo!()`: a panic inside a Tauri command would take the
/// whole app down, and an unimplemented destination method is not worth that.
fn not_yet_implemented(what: &str) -> PublishError {
    PublishError::Rejected(format!("SmugMug {what} is not implemented yet"))
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
        *self
            .pending
            .lock()
            .map_err(|_| PublishError::Io("the authorisation lock was poisoned".into()))? =
            Some(temporary);

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
            .map_err(|_| PublishError::Io("the authorisation lock was poisoned".into()))?
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

        let mut state = PublishState::load_in(&ctx.state_dir, DESTINATION_ID)?;
        state.set_account(Some(nickname));
        state.save_in(&ctx.state_dir)
    }

    async fn ensure_container(
        &self,
        _local: &LocalContainer,
        _ctx: &PublishContext,
    ) -> Result<RemoteContainerId, PublishError> {
        Err(not_yet_implemented("album creation"))
    }

    async fn publish_image(
        &self,
        _item: &PublishItem<'_>,
        _ctx: &PublishContext,
    ) -> Result<RemoteImageId, PublishError> {
        Err(not_yet_implemented("upload"))
    }

    async fn reconcile(
        &self,
        _container: &RemoteContainerId,
        _expected: &[PublishItem<'_>],
        _ctx: &PublishContext,
    ) -> Result<Vec<(String, RemoteImageId)>, PublishError> {
        Err(not_yet_implemented("reconciliation"))
    }
}
