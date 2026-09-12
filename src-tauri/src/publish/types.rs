//! Types shared by every publish destination.
//!
//! Deliberately destination-agnostic: nothing here knows about a particular
//! service, so a session can drive any destination without special-casing on
//! its id. Anything service-specific belongs in that destination's own module.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque destination-side identifier for a container (album, folder, set).
///
/// The string is whatever the destination hands back — for a REST API that is
/// typically a URI, e.g. `/api/v2/album/AbCdEf`. Never parsed locally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteContainerId(pub String);

/// Opaque destination-side identifier for a single published image, e.g.
/// `/api/v2/image/XyZ123-0`. Never parsed locally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteImageId(pub String);

/// What a destination can do, so the session branches on capabilities rather
/// than on [`PublishDestination::id`](crate::publish::PublishDestination::id).
///
/// `Serialize` only: capabilities are reported to the frontend and never read
/// back, and `accepted_mime_types` borrows for `'static` so it could not be
/// deserialised anyway.
#[derive(Debug, Clone, Serialize)]
pub struct DestinationCapabilities {
    /// Can replace an existing remote image in place, keeping its id.
    pub supports_replace: bool,
    /// Can list a container so an ambiguous upload can be resolved.
    pub supports_reconcile: bool,
    /// Containers can nest; when false the local hierarchy is flattened.
    pub supports_nested_containers: bool,
    /// Per-file upload ceiling, when the destination publishes one.
    pub max_bytes: Option<u64>,
    pub accepted_mime_types: &'static [&'static str],
}

/// A local album as the session sees it, before any destination mapping.
#[derive(Debug, Clone)]
pub struct LocalContainer {
    pub album_id: String,
    pub name: String,
    /// Ancestors, outermost first. Flattened into the name in phase 1.
    pub parent_path: Vec<String>,
}

/// How far through authorisation a destination is.
///
/// Internally tagged, so the frontend reads `{ "status": "Connected",
/// "account": "…" }` rather than a shape that differs per variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum AuthStatus {
    /// No consumer key/secret — the user has not registered an application.
    NotConfigured,
    /// Key and secret present, but no access token yet.
    NotAuthorised,
    Connected {
        account: String,
    },
}

/// The user-facing half of an interactive authorisation handshake.
#[derive(Debug, Clone, Serialize)]
pub struct AuthChallenge {
    pub authorize_url: String,
    /// i18n key for the destination's instructions, not a literal string.
    pub instructions_key: &'static str,
}

/// Consumer (application) credentials for a destination.
///
/// A key without its secret is useless, so the pair is absent or present
/// together — that absence is exactly [`AuthStatus::NotConfigured`].
#[derive(Debug, Clone)]
pub struct ConsumerCredentials {
    pub key: String,
    pub secret: String,
}

/// Everything a destination method needs that is not specific to one image.
pub struct PublishContext {
    /// Where destination state files live: [`state_dir`](crate::publish::state::state_dir)
    /// in the app. A path rather than an `AppHandle` so a destination and the
    /// session driving it can be tested without a running Tauri app.
    pub state_dir: PathBuf,
    /// `None` until the user has registered an application with the service.
    pub consumer: Option<ConsumerCredentials>,
    /// Set when the user cancels; checked between retries and uploads.
    pub cancel: Arc<AtomicBool>,
}

/// One image to publish, borrowed from the session's spool.
pub struct PublishItem<'a> {
    /// Rendered file in the spool; deleted once the upload is confirmed.
    pub file: &'a Path,
    pub file_name: String,
    pub mime: &'static str,
    pub title: Option<String>,
    pub caption: Option<String>,
    pub keywords: Vec<String>,
    pub container: &'a RemoteContainerId,
    /// `Some` replaces that remote image in place instead of adding a new one.
    pub replaces: Option<RemoteImageId>,
    /// Stable across retries so the destination can deduplicate them.
    pub request_id: Uuid,
}

/// Failure modes common to every destination.
///
/// Hand-written `Display` rather than `thiserror`: the crate is not in the
/// dependency tree and one enum does not justify adding it.
#[derive(Debug)]
pub enum PublishError {
    NotAuthorised(String),
    CredentialStorage(String),
    Network(String),
    /// Retryable. `retry_after` comes from the `Retry-After` header when the
    /// destination sends one.
    RateLimited {
        retry_after: Option<Duration>,
    },
    /// Committed-or-not is unknown — a timeout may still have landed. Never
    /// retried blindly; drives
    /// [`reconcile`](crate::publish::PublishDestination::reconcile).
    Ambiguous {
        file_name: String,
    },
    Rejected(String),
    Cancelled,
    Io(String),
}

impl PublishError {
    /// Whether a backoff-and-retry could plausibly succeed.
    ///
    /// `Ambiguous` is deliberately not retryable: the upload may already have
    /// committed, so retrying risks a duplicate. It is resolved by
    /// [`reconcile`](crate::publish::PublishDestination::reconcile) instead.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network(_) | Self::RateLimited { .. } => true,
            Self::NotAuthorised(_)
            | Self::CredentialStorage(_)
            | Self::Ambiguous { .. }
            | Self::Rejected(_)
            | Self::Cancelled
            | Self::Io(_) => false,
        }
    }
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAuthorised(detail) => write!(f, "not authorised: {detail}"),
            Self::CredentialStorage(detail) => {
                write!(f, "credential storage unavailable: {detail}")
            }
            Self::Network(detail) => write!(f, "network: {detail}"),
            Self::RateLimited { .. } => write!(f, "rate limited"),
            Self::Ambiguous { file_name } => write!(f, "ambiguous outcome for {file_name}"),
            Self::Rejected(detail) => write!(f, "rejected by destination: {detail}"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Io(detail) => write!(f, "io: {detail}"),
        }
    }
}

impl std::error::Error for PublishError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_failures_are_retryable() {
        assert!(PublishError::Network("timed out".into()).is_retryable());
        assert!(PublishError::RateLimited { retry_after: None }.is_retryable());
        assert!(
            PublishError::RateLimited {
                retry_after: Some(Duration::from_secs(30))
            }
            .is_retryable()
        );
    }

    #[test]
    fn an_ambiguous_outcome_is_not_retryable() {
        assert!(
            !PublishError::Ambiguous {
                file_name: "DSC_0001.jpg".into()
            }
            .is_retryable(),
            "a blind retry risks a duplicate upload; reconcile instead"
        );
    }

    #[test]
    fn permanent_failures_are_not_retryable() {
        assert!(!PublishError::NotAuthorised("no token".into()).is_retryable());
        assert!(!PublishError::CredentialStorage("no keyring".into()).is_retryable());
        assert!(!PublishError::Rejected("file too large".into()).is_retryable());
        assert!(!PublishError::Cancelled.is_retryable());
        assert!(!PublishError::Io("spool file missing".into()).is_retryable());
    }
}
