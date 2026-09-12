//! The SmugMug out-of-band OAuth 1.0a dance and the keyring that holds its
//! result.
//!
//! No consumer key or secret is compiled in: the user registers their own
//! application at api.smugmug.com and pastes the pair into settings. An
//! embedded secret in an open-source desktop binary is trivially extractable,
//! and every install would then share one rate limit.

use std::time::Duration;

use crate::publish::oauth1::{self, Credentials, parse_query, percent_encode};
use crate::publish::smugmug::model::{TokenPair, parse_auth_user};
use crate::publish::{ConsumerCredentials, PublishError};

/// Keyring service name, matching the bundle identifier in `tauri.conf.json`.
pub const KEYRING_SERVICE: &str = "io.github.CyberTimon.RapidRAW";

const REQUEST_TOKEN_URL: &str = "https://api.smugmug.com/services/oauth/1.0a/getRequestToken";
const AUTHORIZE_URL: &str = "https://api.smugmug.com/services/oauth/1.0a/authorize";
const ACCESS_TOKEN_URL: &str = "https://api.smugmug.com/services/oauth/1.0a/getAccessToken";
const AUTH_USER_URL: &str = "https://api.smugmug.com/api/v2!authuser";

/// Every call here is a handful of bytes against an interactive dialog, so a
/// short ceiling beats leaving the user watching a spinner.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// How much of an error body is worth quoting back. Enough to recognise
/// SmugMug's own wording, short of pasting an HTML page into the UI.
const ERROR_BODY_LIMIT: usize = 200;

/// Parses an `application/x-www-form-urlencoded` token response.
///
/// Both token endpoints answer in form encoding rather than JSON. A response
/// missing either half is an error: half a credential pair cannot sign
/// anything, and treating it as success would defer the failure to a much
/// more confusing place.
pub fn parse_token_response(body: &str) -> Result<TokenPair, PublishError> {
    let params = parse_query(body);
    let value = |name: &str| {
        params
            .iter()
            .find(|(key, value)| key == name && !value.is_empty())
            .map(|(_, value)| value.clone())
    };
    // The body is deliberately left out of the error: a response can carry a
    // secret without its token, and this message reaches the log and the UI.
    let missing =
        |name: &str| PublishError::Rejected(format!("the token response carried no {name}"));

    Ok(TokenPair {
        token: value("oauth_token").ok_or_else(|| missing("oauth_token"))?,
        token_secret: value("oauth_token_secret").ok_or_else(|| missing("oauth_token_secret"))?,
    })
}

/// The browser URL the user is sent to in step 3 of the dance.
///
/// `Access=Full&Permissions=Add` is the narrowest pair that permits creating
/// an album and uploading to it — deliberately not `Modify` or `Delete`.
pub fn authorize_url(request_token: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?oauth_token={}&Access=Full&Permissions=Add",
        percent_encode(request_token)
    )
}

/// Keyring account name for one SmugMug user, namespaced so a second
/// destination cannot collide with it.
pub fn account_key(nickname: &str) -> String {
    format!("smugmug:{nickname}")
}

/// Tidies the six-digit verifier the user pastes back from the browser.
///
/// Pasting picks up surrounding whitespace far more often than not, and an
/// untrimmed verifier fails signature verification with an error that blames
/// the wrong thing.
pub fn normalize_verifier(raw: &str) -> Result<String, PublishError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PublishError::NotAuthorised(
            "no verifier was entered".into(),
        ));
    }
    Ok(trimmed.to_string())
}

/// Reads back what [`secret_blob`] wrote.
///
/// A stored credential that will not parse is a storage problem, not a
/// protocol one: reporting it as a malformed *response* would send the user
/// looking at their network.
fn decode_stored(blob: &str) -> Result<TokenPair, PublishError> {
    parse_token_response(blob).map_err(|_| {
        PublishError::CredentialStorage(
            "the stored SmugMug credential is not in the expected format; reconnect the account"
                .into(),
        )
    })
}

/// Serialises a token pair for the keyring, in the same form encoding the
/// endpoints use, so [`parse_token_response`] reads it back.
fn secret_blob(pair: &TokenPair) -> String {
    format!(
        "oauth_token={}&oauth_token_secret={}",
        percent_encode(&pair.token),
        percent_encode(&pair.token_secret)
    )
}

/// Token storage in the OS keyring.
///
/// There is deliberately no plaintext fallback. Where no credential store is
/// available the user is told so: a token written to a file in the home
/// directory is a worse outcome than an error message, and silently
/// downgrading the guarantee is not ours to do.
pub struct SmugMugAuth;

impl SmugMugAuth {
    /// `None` where the keyring is reachable but holds nothing for this
    /// account, which is an ordinary "not connected yet", not a failure.
    pub fn load_tokens(account: &str) -> Result<Option<(String, String)>, PublishError> {
        match store::get(account)? {
            Some(blob) => {
                let pair = decode_stored(&blob)?;
                Ok(Some((pair.token, pair.token_secret)))
            }
            None => Ok(None),
        }
    }

    pub fn store_tokens(account: &str, token: &str, secret: &str) -> Result<(), PublishError> {
        let blob = secret_blob(&TokenPair {
            token: token.to_string(),
            token_secret: secret.to_string(),
        });
        store::set(account, &blob)
    }

    /// Deleting what is already absent succeeds: disconnecting an account
    /// whose entry the user removed by hand is not an error.
    pub fn delete_tokens(account: &str) -> Result<(), PublishError> {
        store::delete(account)
    }
}

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
                format!("{error}. {hint} RapidRAW will not store tokens in a plain file.")
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

    pub fn delete(account: &str) -> Result<(), PublishError> {
        match entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(unavailable(error)),
        }
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

    pub fn delete(_account: &str) -> Result<(), PublishError> {
        Err(unsupported())
    }
}

/// Step 2: temporary credentials, with `oauth_callback=oob` because there is
/// no loopback server to redirect to — the user reads a verifier off the page.
pub async fn fetch_request_token(
    consumer: &ConsumerCredentials,
) -> Result<TokenPair, PublishError> {
    let creds = Credentials {
        consumer_key: consumer.key.clone(),
        consumer_secret: consumer.secret.clone(),
        token: None,
        token_secret: None,
    };
    let params = vec![("oauth_callback".to_string(), "oob".to_string())];
    parse_token_response(&post_signed(REQUEST_TOKEN_URL, &creds, &params).await?)
}

/// Step 5: trade the verifier the user pasted for long-lived token
/// credentials, signed with the temporary pair from [`fetch_request_token`].
pub async fn exchange_verifier(
    consumer: &ConsumerCredentials,
    temporary: &TokenPair,
    verifier: &str,
) -> Result<TokenPair, PublishError> {
    let creds = Credentials {
        consumer_key: consumer.key.clone(),
        consumer_secret: consumer.secret.clone(),
        token: Some(temporary.token.clone()),
        token_secret: Some(temporary.token_secret.clone()),
    };
    let params = vec![("oauth_verifier".to_string(), verifier.to_string())];
    parse_token_response(&post_signed(ACCESS_TOKEN_URL, &creds, &params).await?)
}

/// The nickname names the keyring entry and is what the panel shows, so it is
/// fetched once, immediately after authorising.
pub async fn fetch_nickname(
    consumer: &ConsumerCredentials,
    access: &TokenPair,
) -> Result<String, PublishError> {
    let creds = Credentials {
        consumer_key: consumer.key.clone(),
        consumer_secret: consumer.secret.clone(),
        token: Some(access.token.clone()),
        token_secret: Some(access.token_secret.clone()),
    };
    let header = oauth1::authorization_header(
        "GET",
        AUTH_USER_URL,
        &[],
        &creds,
        &oauth1::nonce(),
        oauth1::timestamp(),
    );
    let response = client()?
        .get(AUTH_USER_URL)
        .header("Authorization", header)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(transport)?;

    Ok(parse_auth_user(&body_or_error(response).await?)?.nick_name)
}

fn client() -> Result<reqwest::Client, PublishError> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(transport)
}

/// The OAuth parameters ride in the `Authorization` header, never the query
/// string. Ordinary SmugMug endpoints tolerate either; the upload endpoint
/// accepts only the header, and one code path means one set of failures.
async fn post_signed(
    url: &str,
    creds: &Credentials,
    extra_params: &[(String, String)],
) -> Result<String, PublishError> {
    let header = oauth1::authorization_header(
        "POST",
        url,
        extra_params,
        creds,
        &oauth1::nonce(),
        oauth1::timestamp(),
    );
    let response = client()?
        .post(url)
        .header("Authorization", header)
        .header("Content-Length", "0")
        .send()
        .await
        .map_err(transport)?;

    body_or_error(response).await
}

fn transport(error: reqwest::Error) -> PublishError {
    if error.is_timeout() {
        PublishError::Network(format!("timed out: {error}"))
    } else {
        PublishError::Network(error.to_string())
    }
}

/// Maps a response onto the shared error vocabulary, so the caller retries a
/// 503 and stops on a 401 without knowing anything about SmugMug.
async fn body_or_error(response: reqwest::Response) -> Result<String, PublishError> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = response.text().await.map_err(transport)?;

    if status.is_success() {
        return Ok(body);
    }

    let detail = format!("HTTP {status}: {}", truncate(&body, ERROR_BODY_LIMIT));
    Err(match status.as_u16() {
        401 | 403 => PublishError::NotAuthorised(detail),
        429 => PublishError::RateLimited { retry_after },
        500..=599 => PublishError::Network(detail),
        _ => PublishError::Rejected(detail),
    })
}

fn truncate(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}…", &trimmed[..cut]),
        None => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_form_encoded_request_token_response() {
        let body = "oauth_token=abc123&oauth_token_secret=def456&oauth_callback_confirmed=true";
        let parsed = parse_token_response(body).unwrap();
        assert_eq!(parsed.token, "abc123");
        assert_eq!(parsed.token_secret, "def456");
    }

    #[test]
    fn rejects_a_response_missing_the_secret() {
        assert!(parse_token_response("oauth_token=abc123").is_err());
    }

    #[test]
    fn rejects_a_response_missing_the_token() {
        assert!(parse_token_response("oauth_token_secret=def456").is_err());
    }

    #[test]
    fn rejects_an_error_body_that_carries_no_token_at_all() {
        assert!(parse_token_response("Invalid signature").is_err());
        assert!(parse_token_response("").is_err());
    }

    #[test]
    fn percent_decodes_values_in_the_response() {
        let parsed = parse_token_response("oauth_token=a%2Bb&oauth_token_secret=c%2Fd").unwrap();
        assert_eq!(parsed.token, "a+b");
        assert_eq!(parsed.token_secret, "c/d");
    }

    #[test]
    fn a_parse_failure_never_echoes_the_secret() {
        let error = parse_token_response("oauth_token_secret=def456").unwrap_err();
        let rendered = error.to_string();
        assert!(
            !rendered.contains("def456"),
            "the error goes to the log and the UI: {rendered}"
        );
    }

    #[test]
    fn authorize_url_requests_least_privilege() {
        let url = authorize_url("tok123");
        assert!(url.contains("oauth_token=tok123"));
        assert!(url.contains("Access=Full"));
        assert!(url.contains("Permissions=Add"));
        assert!(!url.contains("Permissions=Modify"));
        assert!(!url.contains("Delete"));
    }

    #[test]
    fn authorize_url_percent_encodes_the_request_token() {
        let url = authorize_url("a+b/c");
        assert!(url.contains("oauth_token=a%2Bb%2Fc"), "{url}");
    }

    #[test]
    fn the_keyring_account_is_namespaced_by_nickname() {
        assert_eq!(account_key("somephotographer"), "smugmug:somephotographer");
    }

    #[test]
    fn a_stored_pair_round_trips_through_the_parser() {
        let pair = TokenPair {
            token: "tok+with/awkward=bytes".into(),
            token_secret: "sec&more%stuff".into(),
        };
        let parsed = decode_stored(&secret_blob(&pair)).unwrap();
        assert_eq!(parsed, pair);
    }

    #[test]
    fn a_corrupt_keyring_entry_is_reported_as_a_storage_problem() {
        let error = decode_stored("not a credential").unwrap_err();
        assert!(
            matches!(error, PublishError::CredentialStorage(_)),
            "a mangled keyring entry is not a bad response: {error}"
        );
    }

    #[test]
    fn a_pasted_verifier_is_trimmed() {
        assert_eq!(normalize_verifier("  123456 \n").unwrap(), "123456");
    }

    #[test]
    fn an_empty_verifier_is_rejected() {
        assert!(normalize_verifier("   ").is_err());
    }

    /// The one step no fixture can stand in for: whether SmugMug itself
    /// accepts our signature. A 401 here means `oauth1` is wrong and is to be
    /// fixed there, not worked around in this module.
    ///
    /// ```text
    /// SMUGMUG_CONSUMER_KEY=… SMUGMUG_CONSUMER_SECRET=… \
    ///     cargo test --lib -- --ignored --nocapture live_request_token
    /// ```
    #[tokio::test]
    #[ignore = "live: needs SMUGMUG_CONSUMER_KEY and SMUGMUG_CONSUMER_SECRET"]
    async fn live_request_token_is_accepted_by_smugmug() {
        let consumer = ConsumerCredentials {
            key: std::env::var("SMUGMUG_CONSUMER_KEY").expect("SMUGMUG_CONSUMER_KEY is not set"),
            secret: std::env::var("SMUGMUG_CONSUMER_SECRET")
                .expect("SMUGMUG_CONSUMER_SECRET is not set"),
        };
        let temporary = fetch_request_token(&consumer)
            .await
            .expect("getRequestToken should return temporary credentials");

        assert!(!temporary.token.is_empty());
        assert!(!temporary.token_secret.is_empty());
        println!("authorize at: {}", authorize_url(&temporary.token));
    }
}
