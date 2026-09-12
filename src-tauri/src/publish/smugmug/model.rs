//! Deserialised shapes for the SmugMug authorisation responses.
//!
//! Only the fields the authorisation flow actually reads are declared, so a
//! new field on SmugMug's side cannot break parsing. The OAuth token endpoints
//! answer in `application/x-www-form-urlencoded`, not JSON — those are parsed
//! by hand in [`auth`](super::auth); what is left here is the one JSON call
//! the flow makes, `GET /api/v2!authuser`.

use serde::Deserialize;

use crate::publish::PublishError;

/// An OAuth 1.0a credentials pair: a temporary one before authorisation, a
/// token one after. The secret half never leaves the process except to go
/// into the OS keyring.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenPair {
    pub token: String,
    pub token_secret: String,
}

/// Redacts the secret: a token pair otherwise reaches the log the first time
/// anything derives `Debug` on a struct holding one.
impl std::fmt::Debug for TokenPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenPair")
            .field("token", &self.token)
            .field("token_secret", &"<redacted>")
            .finish()
    }
}

/// The authenticated user, from `GET /api/v2!authuser`.
///
/// `uri` is unused by the authorisation flow itself and is kept because the
/// album code needs a starting node; the nickname is what names the keyring
/// entry and what the panel shows.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthUser {
    #[serde(rename = "NickName")]
    pub nick_name: String,
    #[serde(rename = "Uri")]
    pub uri: String,
}

#[derive(Debug, Deserialize)]
struct AuthUserResponse {
    #[serde(rename = "User")]
    user: AuthUser,
}

#[derive(Debug, Deserialize)]
struct AuthUserEnvelope {
    #[serde(rename = "Response")]
    response: AuthUserResponse,
}

/// Pulls the authenticated user out of an `/api/v2!authuser` payload.
pub fn parse_auth_user(body: &str) -> Result<AuthUser, PublishError> {
    serde_json::from_str::<AuthUserEnvelope>(body)
        .map(|envelope| envelope.response.user)
        .map_err(|e| PublishError::Rejected(format!("unexpected authuser response: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real response; the unread fields are kept to prove they
    /// are ignored rather than fought with.
    const AUTHUSER_BODY: &str = r#"{
        "Response": {
            "Uri": "/api/v2!authuser",
            "Locator": "User",
            "User": {
                "NickName": "somephotographer",
                "Name": "Some Photographer",
                "Uri": "/api/v2/user/somephotographer",
                "WebUri": "https://somephotographer.smugmug.com",
                "Uris": {
                    "Node": { "Uri": "/api/v2/node/abc123" }
                }
            }
        },
        "Code": 200,
        "Message": "Ok"
    }"#;

    #[test]
    fn reads_the_nickname_and_uri_of_the_authenticated_user() {
        let user = parse_auth_user(AUTHUSER_BODY).unwrap();
        assert_eq!(user.nick_name, "somephotographer");
        assert_eq!(user.uri, "/api/v2/user/somephotographer");
    }

    #[test]
    fn rejects_a_payload_with_no_user() {
        let body = r#"{"Response": {"Uri": "/api/v2!authuser"}, "Code": 200}"#;
        assert!(parse_auth_user(body).is_err());
    }

    #[test]
    fn rejects_a_body_that_is_not_json() {
        assert!(parse_auth_user("<html>404 Not Found</html>").is_err());
    }

    #[test]
    fn debug_output_does_not_include_the_token_secret() {
        let pair = TokenPair {
            token: "tok123".into(),
            token_secret: "sec456".into(),
        };
        let rendered = format!("{pair:?}");
        assert!(rendered.contains("tok123"));
        assert!(
            !rendered.contains("sec456"),
            "the secret must never reach a log line: {rendered}"
        );
    }
}
