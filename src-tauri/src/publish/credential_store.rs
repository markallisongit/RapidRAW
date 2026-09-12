//! Secrets in the OS keyring: every destination's consumer credentials, and
//! the access tokens a destination's own auth module files under its account.
//!
//! There is deliberately no plaintext fallback. Where no credential store is
//! available the user is told so: a secret written to a file in the home
//! directory is a worse outcome than an error message, and silently
//! downgrading the guarantee is not ours to do.

use std::path::Path;

use crate::publish::oauth1::{parse_query, percent_encode};
use crate::publish::state::PublishState;
use crate::publish::{ConsumerCredentials, PublishError};

/// Keyring service name, matching the bundle identifier in `tauri.conf.json`.
pub const KEYRING_SERVICE: &str = "io.github.CyberTimon.RapidRAW";

/// Keyring account holding a destination's consumer credentials.
///
/// `consumer:` leads rather than the destination id, so this can never
/// collide with a destination's own `<id>:<account>` token entries — not even
/// for a user whose nickname happens to be "consumer".
pub fn consumer_key(destination_id: &str) -> String {
    format!("consumer:{destination_id}")
}

/// `None` where the keyring is reachable but holds nothing, which is exactly
/// [`AuthStatus::NotConfigured`](crate::publish::AuthStatus::NotConfigured).
pub fn load_consumer(destination_id: &str) -> Result<Option<ConsumerCredentials>, PublishError> {
    store::get(&consumer_key(destination_id))?
        .map(|blob| decode_consumer(&blob))
        .transpose()
}

pub fn store_consumer(
    destination_id: &str,
    consumer: &ConsumerCredentials,
) -> Result<(), PublishError> {
    store::set(&consumer_key(destination_id), &encode_consumer(consumer))
}

/// Stores the credentials the user entered, trimmed, and disconnects the
/// account when they belong to a different application.
///
/// An access token is minted for one consumer key, so keeping the old
/// account after the key changes would report Connected and then fail every
/// call with a signature error. A stored pair that cannot be read counts as
/// different: re-entering the credentials is how the user repairs it.
pub fn replace_consumer(
    state_dir: &Path,
    destination_id: &str,
    key: &str,
    secret: &str,
) -> Result<(), PublishError> {
    let consumer = ConsumerCredentials {
        key: key.trim().to_string(),
        secret: secret.trim().to_string(),
    };
    if consumer.key.is_empty() || consumer.secret.is_empty() {
        return Err(PublishError::Rejected(
            "both the API key and the secret are required".into(),
        ));
    }

    let unchanged = matches!(
        load_consumer(destination_id),
        Ok(Some(previous)) if previous.key == consumer.key
    );
    store_consumer(destination_id, &consumer)?;
    if unchanged {
        return Ok(());
    }

    let mut state = PublishState::load_in(state_dir, destination_id)?;
    if state.account().is_some() {
        state.set_account(None);
        state.save_in(state_dir)?;
    }
    Ok(())
}

/// Form encoding, like the token entries, so a key or secret containing `&`
/// or `=` survives the round trip.
fn encode_consumer(consumer: &ConsumerCredentials) -> String {
    format!(
        "key={}&secret={}",
        percent_encode(&consumer.key),
        percent_encode(&consumer.secret)
    )
}

fn decode_consumer(blob: &str) -> Result<ConsumerCredentials, PublishError> {
    let params = parse_query(blob);
    let value = |name: &str| {
        params
            .iter()
            .find(|(key, value)| key == name && !value.is_empty())
            .map(|(_, value)| value.clone())
    };
    match (value("key"), value("secret")) {
        (Some(key), Some(secret)) => Ok(ConsumerCredentials { key, secret }),
        _ => Err(PublishError::CredentialStorage(
            "the stored API key and secret are not in the expected format; enter them again".into(),
        )),
    }
}

pub use store::{get, set};

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
mod store {
    use super::{KEYRING_SERVICE, PublishError};

    fn entry(account: &str) -> Result<keyring::Entry, PublishError> {
        keyring::Entry::new(KEYRING_SERVICE, account).map_err(unavailable)
    }

    /// Names the platform problem rather than reporting a bare "keyring
    /// error": on Linux the usual cause is that nothing is providing the
    /// Secret Service, which the user can act on.
    fn unavailable(error: keyring::Error) -> PublishError {
        let detail = match error {
            keyring::Error::NoDefaultStore => {
                let hint = if cfg!(target_os = "linux") {
                    "A Secret Service provider such as gnome-keyring or kwallet must be running."
                } else {
                    "The OS credential store could not be opened."
                };
                format!("{error}. {hint} RapidRAW will not store credentials in a plain file.")
            }
            keyring::Error::NoStorageAccess(_) => {
                format!("{error}. The credential store may be locked.")
            }
            other => other.to_string(),
        };
        PublishError::CredentialStorage(detail)
    }

    pub fn get(account: &str) -> Result<Option<String>, PublishError> {
        match entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(unavailable(error)),
        }
    }

    pub fn set(account: &str, secret: &str) -> Result<(), PublishError> {
        entry(account)?.set_password(secret).map_err(unavailable)
    }
}

/// `keyring` has no backend here, and the publish panel is desktop-only, so
/// reaching this is a bug rather than something a user can hit.
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod store {
    use super::PublishError;

    fn unsupported() -> PublishError {
        PublishError::CredentialStorage(
            "this platform has no OS credential store; publishing is desktop-only".into(),
        )
    }

    pub fn get(_account: &str) -> Result<Option<String>, PublishError> {
        Err(unsupported())
    }

    pub fn set(_account: &str, _secret: &str) -> Result<(), PublishError> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_credentials_round_trip_through_their_encoding() {
        let consumer = ConsumerCredentials {
            key: "k&ey=1".into(),
            secret: "s/ec+ret".into(),
        };
        let decoded = decode_consumer(&encode_consumer(&consumer)).unwrap();
        assert_eq!(decoded.key, consumer.key);
        assert_eq!(decoded.secret, consumer.secret);
    }

    #[test]
    fn half_a_stored_consumer_pair_is_a_storage_error() {
        assert!(matches!(
            decode_consumer("key=abc"),
            Err(PublishError::CredentialStorage(_))
        ));
    }

    #[test]
    fn the_consumer_entry_cannot_collide_with_a_token_entry() {
        assert_ne!(
            consumer_key("smugmug"),
            crate::publish::smugmug::auth::account_key("consumer")
        );
    }
}
