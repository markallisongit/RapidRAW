//! Deserialised shapes for the SmugMug API v2 responses.
//!
//! Only the fields RapidRAW actually reads are declared, so a new field on
//! SmugMug's side cannot break parsing. The OAuth token endpoints answer in
//! `application/x-www-form-urlencoded`, not JSON — those are parsed by hand in
//! [`auth`](super::auth); everything JSON-shaped is parsed here.
//!
//! Every response arrives wrapped in the same `{"Response": …, "Code": …}`
//! envelope, and every list endpoint pages, so those two shapes are shared.

use serde::Deserialize;

use crate::publish::{PublishError, RemoteImageId};

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

/// The `{"Response": …, "Code": …}` wrapper every API v2 call answers with.
#[derive(Debug, Deserialize)]
struct Envelope<T> {
    #[serde(rename = "Response")]
    response: T,
}

/// A `Uris` entry, which is always an object carrying a single `Uri`.
#[derive(Debug, Clone, Deserialize)]
struct UriRef {
    #[serde(rename = "Uri")]
    uri: String,
}

/// The paging block on a list response.
///
/// Only `NextPage` is read: following the link until it is absent is both
/// simpler and more robust than arithmetic on `Start` and `Total`, which move
/// under a caller that is also creating albums.
#[derive(Debug, Default, Deserialize)]
struct Pages {
    #[serde(rename = "NextPage")]
    next_page: Option<String>,
}

/// Parses one envelope, naming the endpoint so a malformed response says
/// which call produced it.
fn parse_envelope<T: serde::de::DeserializeOwned>(
    what: &str,
    body: &str,
) -> Result<T, PublishError> {
    serde_json::from_str::<Envelope<T>>(body)
        .map(|envelope| envelope.response)
        .map_err(|e| PublishError::Rejected(format!("unexpected {what} response: {e}")))
}

/// The authenticated user, from `GET /api/v2!authuser`.
///
/// `node_uri` is the user's root node, which is where album lookup starts;
/// the nickname is what names the keyring entry and what the panel shows.
/// Note that `uri` identifies the *user* and is not a node — SmugMug will not
/// accept it where a node is wanted.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub nick_name: String,
    pub uri: String,
    pub node_uri: String,
}

#[derive(Debug, Deserialize)]
struct AuthUserResponse {
    #[serde(rename = "User")]
    user: RawAuthUser,
}

#[derive(Debug, Deserialize)]
struct RawAuthUser {
    #[serde(rename = "NickName")]
    nick_name: String,
    #[serde(rename = "Uri")]
    uri: String,
    #[serde(rename = "Uris")]
    uris: AuthUserUris,
}

/// `Node` is required rather than optional: a user with no root node leaves
/// nowhere to create an album, and discovering that at creation time would
/// blame the wrong call.
#[derive(Debug, Deserialize)]
struct AuthUserUris {
    #[serde(rename = "Node")]
    node: UriRef,
}

/// Pulls the authenticated user out of an `/api/v2!authuser` payload.
pub fn parse_auth_user(body: &str) -> Result<AuthUser, PublishError> {
    let raw = parse_envelope::<AuthUserResponse>("authuser", body)?.user;
    Ok(AuthUser {
        nick_name: raw.nick_name,
        uri: raw.uri,
        node_uri: raw.uris.node.uri,
    })
}

/// One child of a node: a folder, an album or a page.
///
/// A node and the album it holds are two different objects with two different
/// URIs. Uploads and image listings want the album URI, which is why
/// [`album_uri`](Self::album_uri) exists and `uri` is not used in its place.
#[derive(Debug, Clone, Deserialize)]
pub struct ChildNode {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Type")]
    pub node_type: String,
    #[serde(rename = "Uri")]
    pub uri: String,
    #[serde(rename = "Uris", default)]
    uris: ChildNodeUris,
}

/// Absent on a folder, which is exactly what distinguishes one from an album.
#[derive(Debug, Clone, Default, Deserialize)]
struct ChildNodeUris {
    #[serde(rename = "Album")]
    album: Option<UriRef>,
}

impl ChildNode {
    pub fn is_album(&self) -> bool {
        self.node_type == "Album"
    }

    /// The URI of the album this node holds, absent for anything else.
    pub fn album_uri(&self) -> Option<&str> {
        self.uris.album.as_ref().map(|album| album.uri.as_str())
    }
}

/// One page of `GET <node>!children`.
pub struct NodeChildrenPage {
    pub nodes: Vec<ChildNode>,
    /// A path, not an absolute URL; the caller joins it to the API base.
    pub next_page: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NodeChildrenResponse {
    /// Absent, not empty, when the node has no children.
    #[serde(rename = "Node", default)]
    node: Vec<ChildNode>,
    #[serde(rename = "Pages", default)]
    pages: Pages,
}

pub fn parse_node_children(body: &str) -> Result<NodeChildrenPage, PublishError> {
    let response = parse_envelope::<NodeChildrenResponse>("node children", body)?;
    Ok(NodeChildrenPage {
        nodes: response.node,
        next_page: response.pages.next_page,
    })
}

#[derive(Debug, Deserialize)]
struct CreatedNodeResponse {
    #[serde(rename = "Node")]
    node: ChildNode,
}

/// Reads the node a `POST <node>!children` created.
pub fn parse_created_node(body: &str) -> Result<ChildNode, PublishError> {
    Ok(parse_envelope::<CreatedNodeResponse>("album creation", body)?.node)
}

/// What an album already holds, as far as republish needs to know: enough to
/// tell whether an expected file landed, and the URI to replace it in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImageSummary {
    pub file_name: String,
    pub size_bytes: u64,
    pub image_uri: RemoteImageId,
}

/// One page of `GET <album>!images`.
pub struct AlbumImagesPage {
    pub images: Vec<RemoteImageSummary>,
    pub next_page: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AlbumImagesResponse {
    #[serde(rename = "AlbumImage", default)]
    album_image: Vec<RawAlbumImage>,
    #[serde(rename = "Pages", default)]
    pages: Pages,
}

#[derive(Debug, Deserialize)]
struct RawAlbumImage {
    #[serde(rename = "FileName")]
    file_name: String,
    /// The size of the original bytes SmugMug archived, which is what a
    /// spooled render is compared against. `OriginalSize` describes a
    /// derivative and would not match.
    #[serde(rename = "ArchivedSize", default)]
    archived_size: u64,
    #[serde(rename = "Uris")]
    uris: AlbumImageUris,
}

/// The album-image URI in the payload's `Uri` is not the image URI; only this
/// one is accepted where an image is replaced in place.
#[derive(Debug, Deserialize)]
struct AlbumImageUris {
    #[serde(rename = "Image")]
    image: UriRef,
}

pub fn parse_album_images(body: &str) -> Result<AlbumImagesPage, PublishError> {
    let response = parse_envelope::<AlbumImagesResponse>("album images", body)?;
    Ok(AlbumImagesPage {
        images: response
            .album_image
            .into_iter()
            .map(|image| RemoteImageSummary {
                file_name: image.file_name,
                size_bytes: image.archived_size,
                image_uri: RemoteImageId(image.uris.image.uri),
            })
            .collect(),
        next_page: response.pages.next_page,
    })
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

    /// Trimmed from a real `!children` response. The album's own URI hangs
    /// off `Uris.Album`; the node URI is a different thing and uploads do not
    /// accept it.
    const CHILDREN_BODY: &str = r#"{
        "Response": {
            "Uri": "/api/v2/node/rootnode!children",
            "Locator": "Node",
            "LocatorType": "Objects",
            "Node": [
                {
                    "Name": "Faroes 2025",
                    "Type": "Folder",
                    "Uri": "/api/v2/node/f4r0e5",
                    "UrlName": "Faroes-2025"
                },
                {
                    "Name": "Iceland 2026",
                    "Type": "Album",
                    "Uri": "/api/v2/node/1c3l4nd",
                    "UrlName": "Iceland-2026",
                    "Uris": {
                        "Album": { "Uri": "/api/v2/album/AbCdEf" },
                        "HighlightImage": { "Uri": "/api/v2/image/XyZ123-0" }
                    }
                }
            ],
            "Pages": {
                "Total": 42,
                "Start": 1,
                "Count": 2,
                "NextPage": "/api/v2/node/rootnode!children?start=3&count=2"
            }
        },
        "Code": 200,
        "Message": "Ok"
    }"#;

    #[test]
    fn reads_the_children_of_a_node_with_their_types() {
        let page = parse_node_children(CHILDREN_BODY).unwrap();
        assert_eq!(page.nodes.len(), 2);
        assert!(!page.nodes[0].is_album(), "a Folder is not an album");
        assert!(page.nodes[1].is_album());
        assert_eq!(page.nodes[1].name, "Iceland 2026");
        assert_eq!(page.nodes[1].album_uri(), Some("/api/v2/album/AbCdEf"));
    }

    #[test]
    fn a_node_with_no_album_uri_reports_none_rather_than_failing_to_parse() {
        let page = parse_node_children(CHILDREN_BODY).unwrap();
        assert_eq!(page.nodes[0].album_uri(), None);
    }

    #[test]
    fn carries_the_next_page_link_when_there_is_one() {
        let page = parse_node_children(CHILDREN_BODY).unwrap();
        assert_eq!(
            page.next_page.as_deref(),
            Some("/api/v2/node/rootnode!children?start=3&count=2")
        );
    }

    /// An empty node omits the locator array entirely rather than sending an
    /// empty one, which a required field would read as a malformed response.
    #[test]
    fn a_node_with_no_children_parses_as_an_empty_last_page() {
        let body = r#"{"Response": {"Uri": "/api/v2/node/x!children"}, "Code": 200}"#;
        let page = parse_node_children(body).unwrap();
        assert!(page.nodes.is_empty());
        assert_eq!(page.next_page, None);
    }

    #[test]
    fn reads_the_album_a_creation_call_returns() {
        let body = r#"{
            "Response": {
                "Node": {
                    "Name": "Iceland 2026",
                    "Type": "Album",
                    "Uri": "/api/v2/node/1c3l4nd",
                    "Uris": { "Album": { "Uri": "/api/v2/album/AbCdEf" } }
                }
            },
            "Code": 201,
            "Message": "Created"
        }"#;
        let node = parse_created_node(body).unwrap();
        assert_eq!(node.album_uri(), Some("/api/v2/album/AbCdEf"));
    }

    #[test]
    fn reads_the_name_size_and_image_uri_of_each_album_image() {
        let body = r#"{
            "Response": {
                "AlbumImage": [
                    {
                        "FileName": "DSC_0001.jpg",
                        "ArchivedSize": 4194304,
                        "Uri": "/api/v2/album/AbCdEf/image/XyZ123-0",
                        "Uris": { "Image": { "Uri": "/api/v2/image/XyZ123-0" } }
                    }
                ],
                "Pages": { "Total": 1, "Start": 1 }
            },
            "Code": 200
        }"#;
        let page = parse_album_images(body).unwrap();
        assert_eq!(page.next_page, None);
        assert_eq!(
            page.images,
            vec![RemoteImageSummary {
                file_name: "DSC_0001.jpg".into(),
                size_bytes: 4_194_304,
                image_uri: RemoteImageId("/api/v2/image/XyZ123-0".into()),
            }]
        );
    }

    #[test]
    fn an_empty_album_lists_no_images() {
        let body = r#"{"Response": {"Uri": "/api/v2/album/AbCdEf!images"}, "Code": 200}"#;
        assert!(parse_album_images(body).unwrap().images.is_empty());
    }

    #[test]
    fn reads_the_root_node_of_the_authenticated_user() {
        let user = parse_auth_user(AUTHUSER_BODY).unwrap();
        assert_eq!(user.node_uri, "/api/v2/node/abc123");
    }

    /// Without a root node there is nowhere to put an album, and a later
    /// failure would blame album creation rather than the response.
    #[test]
    fn rejects_a_user_with_no_root_node() {
        let body = r#"{"Response": {"User": {
            "NickName": "somephotographer",
            "Uri": "/api/v2/user/somephotographer"
        }}, "Code": 200}"#;
        assert!(parse_auth_user(body).is_err());
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
