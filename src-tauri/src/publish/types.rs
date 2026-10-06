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

/// One image a container holds, as listed for matching local photos to it.
/// Everything past the name is filled only where the destination's listing
/// provides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImage {
    pub id: RemoteImageId,
    /// As the destination stores it, which for an upload is the name it was
    /// sent with.
    pub file_name: String,
    /// When the photo was taken, by the camera's own clock.
    pub captured_at: Option<CaptureTime>,
    pub camera_model: Option<String>,
    /// A small rendering of the whole picture, uncropped.
    pub thumbnail_url: Option<String>,
}

impl RemoteImage {
    /// An image known only by its id and name.
    pub fn named(id: RemoteImageId, file_name: impl Into<String>) -> Self {
        Self {
            id,
            file_name: file_name.into(),
            captured_at: None,
            camera_model: None,
            thumbnail_url: None,
        }
    }
}

/// A capture time as EXIF records it: the camera's clock with no time zone,
/// to the second, and the fraction of a second when the camera wrote one.
///
/// Deliberately zone-free. A service that converts the camera's clock to UTC
/// has to guess the zone, and the guess would never match a local reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptureTime {
    pub at: chrono::NaiveDateTime,
    pub millis: Option<u16>,
}

impl CaptureTime {
    /// `at` to the second; the fraction goes in `millis` when there is one.
    pub fn new(at: chrono::NaiveDateTime, millis: Option<u16>) -> Self {
        use chrono::Timelike;
        Self {
            at: at.with_nanosecond(0).unwrap_or(at),
            millis,
        }
    }

    /// The same moment: the same second, and the same fraction when both
    /// sides know it. A side without one cannot tell burst frames apart.
    pub fn same_moment(&self, other: &Self) -> bool {
        self.at == other.at
            && match (self.millis, other.millis) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    }

    /// Milliseconds from the digits of a fraction of a second, as EXIF's
    /// `SubSecTimeOriginal` and ISO 8601 write them: `"45"` is 450 ms.
    pub fn millis_from_fraction(digits: &str) -> Option<u16> {
        let digits = digits.trim();
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let padded: String = digits.chars().chain("000".chars()).take(3).collect();
        padded.parse().ok()
    }
}

/// What a linked container currently looks like at the destination.
///
/// Refresh uses the images to retain matching records, and to find uploads
/// whose outcome was never recorded. It never changes them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerSnapshot {
    pub name: String,
    pub web_url: Option<String>,
    pub images: Vec<SnapshotImage>,
}

/// One image in a [`ContainerSnapshot`]. Everything past the id is filled
/// only where the destination's listing provides it; without a name and size
/// an unrecorded upload can never be recognised.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotImage {
    pub id: RemoteImageId,
    /// As the destination stores it, which for an upload is the name it was
    /// sent with.
    pub file_name: Option<String>,
    /// The size of the bytes that were uploaded.
    pub size_bytes: Option<u64>,
    /// RFC 3339.
    pub uploaded_at: Option<String>,
}

impl SnapshotImage {
    pub fn id_only(id: RemoteImageId) -> Self {
        Self {
            id,
            file_name: None,
            size_bytes: None,
            uploaded_at: None,
        }
    }
}

/// Opaque destination-side identifier for anything in the container tree,
/// folder or album, e.g. `/api/v2/node/1c3l4nd`. Not a [`RemoteContainerId`]:
/// a destination may name an album's place in the tree and the album itself
/// differently, and only the latter takes uploads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteNodeId(pub String);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RemoteNodeKind {
    Folder,
    Album,
}

/// One entry in the destination's container tree, as the Publish Manager
/// browses it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteNode {
    pub id: RemoteNodeId,
    /// `Some` for an album: what a link records and uploads go into.
    pub container: Option<RemoteContainerId>,
    pub kind: RemoteNodeKind,
    pub name: String,
    pub web_url: Option<String>,
    pub has_children: bool,
}

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
    /// Containers can sit inside folders, which
    /// [`list_containers`](crate::publish::PublishDestination::list_containers)
    /// then returns for browsing.
    pub supports_nested_containers: bool,
    /// Per-file upload ceiling, when the destination publishes one.
    pub max_bytes: Option<u64>,
    pub accepted_mime_types: &'static [&'static str],
    /// The privacy levels a new container can be created with, so the Publish
    /// Manager offers only what the destination can honour.
    pub supported_privacy: &'static [ContainerPrivacy],
}

/// Who can see a container the destination creates. Destination-neutral: each
/// destination maps it onto its own vocabulary.
///
/// Applies only when creating. Finding or linking an existing container never
/// changes its privacy, which the user may have set deliberately on the
/// service itself.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContainerPrivacy {
    /// The default, matching SmugMug's own Lightroom plugin.
    #[default]
    Public,
    /// Reachable by anyone with the link, but not listed.
    Unlisted,
    Private,
}

/// A local album as the session sees it, before any destination mapping.
#[derive(Debug, Clone)]
pub struct LocalContainer {
    pub album_id: String,
    pub name: String,
    /// Ancestors, outermost first.
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
    /// The privacy of a container [`create_container`](crate::publish::PublishDestination::create_container)
    /// creates, from the destination's settings.
    ///
    /// A field here rather than an argument, because it changes the trait
    /// less: no destination method gains a parameter, and a later create path
    /// reads the same field instead of threading an options value through.
    pub new_container_privacy: ContainerPrivacy,
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
    /// The album's link is marked broken: its remote album was not found, so
    /// nothing is published into it until it is linked again.
    LinkBroken,
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
            | Self::LinkBroken
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
            Self::LinkBroken => write!(f, "the linked album no longer exists"),
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
