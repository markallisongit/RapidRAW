//! Deserialised shapes for the SmugMug API v2 responses.
//!
//! Only the fields RapidRAW actually reads are declared, so a new field on
//! SmugMug's side cannot break parsing. The OAuth token endpoints answer in
//! `application/x-www-form-urlencoded`, not JSON — those are parsed by hand in
//! [`auth`](super::auth); everything JSON-shaped is parsed here.
//!
//! Every response arrives wrapped in the same `{"Response": …, "Code": …}`
//! envelope, and every list endpoint pages, so those two shapes are shared.

use std::collections::HashMap;

use serde::Deserialize;

use crate::publish::{CaptureTime, PublishError, RemoteImageId};

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
    #[allow(dead_code, reason = "parsed to pin the user/node distinction in tests")]
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
    /// The node, which is what browsing drills into. Never where uploads go.
    #[serde(rename = "Uri")]
    pub uri: String,
    /// The page on the account's site, which the panel opens.
    #[serde(rename = "WebUri", default)]
    pub web_uri: Option<String>,
    /// Absent on an album, which holds images rather than nodes.
    #[serde(rename = "HasChildren", default)]
    pub has_children: bool,
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

/// One album, from `GET /api/v2/album/<key>`: what linking to it records.
///
/// `node_uri` is required for the same reason as on [`AuthUser`]: an album is
/// always in a node, and one without says the response is not what it seems.
#[derive(Debug, Clone)]
pub struct Album {
    pub name: String,
    pub uri: String,
    pub web_uri: Option<String>,
    pub node_uri: String,
}

#[derive(Debug, Deserialize)]
struct AlbumResponse {
    #[serde(rename = "Album")]
    album: RawAlbum,
}

#[derive(Debug, Deserialize)]
struct RawAlbum {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Uri")]
    uri: String,
    #[serde(rename = "WebUri", default)]
    web_uri: Option<String>,
    #[serde(rename = "Uris")]
    uris: AlbumUris,
}

#[derive(Debug, Deserialize)]
struct AlbumUris {
    #[serde(rename = "Node")]
    node: UriRef,
}

pub fn parse_album(body: &str) -> Result<Album, PublishError> {
    let raw = parse_envelope::<AlbumResponse>("album", body)?.album;
    Ok(Album {
        name: raw.name,
        uri: raw.uri,
        web_uri: raw.web_uri,
        node_uri: raw.uris.node.uri,
    })
}

/// What an album already holds, as far as republish needs to know: enough to
/// tell whether an expected file landed, and the URI to replace it in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImageSummary {
    pub file_name: String,
    pub size_bytes: u64,
    pub image_uri: RemoteImageId,
    /// RFC 3339, when the listing includes it.
    pub uploaded_at: Option<String>,
    /// From the `ImageMetadata` expansion, when the listing asked for it.
    pub captured_at: Option<CaptureTime>,
    pub camera_model: Option<String>,
    /// The `S` size: the whole picture, uncropped, from the
    /// `ImageSizeDetails` expansion.
    pub thumbnail_url: Option<String>,
}

/// One page of `GET <album>!images`.
pub struct AlbumImagesPage {
    pub images: Vec<RemoteImageSummary>,
    pub next_page: Option<String>,
}

/// Expansions arrive beside `Response`, keyed on the URI each image's `Uris`
/// entry names.
#[derive(Debug, Deserialize)]
struct AlbumImagesEnvelope {
    #[serde(rename = "Response")]
    response: AlbumImagesResponse,
    #[serde(rename = "Expansions", default)]
    expansions: HashMap<String, Expansion>,
}

#[derive(Debug, Default, Deserialize)]
struct Expansion {
    #[serde(rename = "ImageMetadata")]
    image_metadata: Option<RawImageMetadata>,
    #[serde(rename = "ImageSizeDetails")]
    image_size_details: Option<RawImageSizeDetails>,
}

/// The sizes SmugMug renders of an image, trimmed to the one matching reads.
#[derive(Debug, Deserialize)]
struct RawImageSizeDetails {
    #[serde(rename = "ImageSizeSmall")]
    image_size_small: Option<RawImageSize>,
}

#[derive(Debug, Deserialize)]
struct RawImageSize {
    #[serde(rename = "Url", default)]
    url: String,
}

/// SmugMug's reading of the photo's embedded metadata.
///
/// `DateTimeCreated` is the camera's clock with no zone, as EXIF records it.
/// The image's own `DateTimeOriginal` is not: SmugMug reads that clock as US
/// Pacific time and converts it to UTC, so it never equals a local reading.
#[derive(Debug, Deserialize)]
struct RawImageMetadata {
    #[serde(rename = "DateTimeCreated", default)]
    date_time_created: String,
    /// With the fraction of a second and an offset, when the camera wrote a
    /// fraction: `2026-09-11T17:31:52.458+01:00`. Otherwise empty.
    #[serde(rename = "MicroDateTimeCreated", default)]
    micro_date_time_created: String,
    #[serde(rename = "Model", default)]
    model: String,
}

impl RawImageMetadata {
    fn captured_at(&self) -> Option<CaptureTime> {
        let micro = parse_local_time(&self.micro_date_time_created);
        let at = parse_local_time(&self.date_time_created)
            .or(micro)
            .map(|(at, _)| at)?;
        Some(CaptureTime::new(at, micro.and_then(|(_, millis)| millis)))
    }
}

/// The wall-clock part of an ISO 8601 time, ignoring any offset, and the
/// milliseconds when it has a fraction.
fn parse_local_time(value: &str) -> Option<(chrono::NaiveDateTime, Option<u16>)> {
    let value = value.trim();
    let seconds = value.get(..19)?;
    let at = chrono::NaiveDateTime::parse_from_str(seconds, "%Y-%m-%dT%H:%M:%S").ok()?;
    let millis = value[19..].strip_prefix('.').and_then(|rest| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        CaptureTime::millis_from_fraction(&digits)
    });
    Some((at, millis))
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
    /// Absent where a listing filters it out.
    #[serde(rename = "DateTimeUploaded", default)]
    date_time_uploaded: Option<String>,
    #[serde(rename = "Uris")]
    uris: AlbumImageUris,
}

/// The album-image URI in the payload's `Uri` is not the image URI; only this
/// one is accepted where an image is replaced in place.
#[derive(Debug, Deserialize)]
struct AlbumImageUris {
    #[serde(rename = "Image")]
    image: UriRef,
    #[serde(rename = "ImageMetadata", default)]
    image_metadata: Option<UriRef>,
    #[serde(rename = "ImageSizeDetails", default)]
    image_size_details: Option<UriRef>,
}

/// One page of an album's images, with each image's metadata where the
/// request expanded it.
pub fn parse_album_images(body: &str) -> Result<AlbumImagesPage, PublishError> {
    let AlbumImagesEnvelope {
        response,
        expansions,
    } = serde_json::from_str(body)
        .map_err(|e| PublishError::Rejected(format!("unexpected album images response: {e}")))?;
    Ok(AlbumImagesPage {
        images: response
            .album_image
            .into_iter()
            .map(|image| {
                let expansion =
                    |uri: &Option<UriRef>| uri.as_ref().and_then(|uri| expansions.get(&uri.uri));
                let metadata = expansion(&image.uris.image_metadata)
                    .and_then(|expansion| expansion.image_metadata.as_ref());
                let thumbnail_url = expansion(&image.uris.image_size_details)
                    .and_then(|expansion| expansion.image_size_details.as_ref())
                    .and_then(|sizes| sizes.image_size_small.as_ref())
                    .map(|size| size.url.clone())
                    .filter(|url| !url.is_empty());
                RemoteImageSummary {
                    file_name: image.file_name,
                    size_bytes: image.archived_size,
                    image_uri: RemoteImageId(image.uris.image.uri),
                    uploaded_at: image.date_time_uploaded,
                    captured_at: metadata.and_then(RawImageMetadata::captured_at),
                    camera_model: metadata
                        .map(|metadata| metadata.model.trim().to_string())
                        .filter(|model| !model.is_empty()),
                    thumbnail_url,
                }
            })
            .collect(),
        next_page: response.pages.next_page,
    })
}

/// `url` when SmugMug's photo host serves it, and `None` for any other URL,
/// which is never fetched.
///
/// A rendered size is served unsigned, because its URL carries an access
/// token of its own. That token is issued for one size: no other size's URL
/// can be derived from it, so a listing's URL is fetched exactly as given.
pub fn photo_host_url(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let (host, _) = rest.split_once('/')?;
    (host == "smugmug.com" || host.ends_with(".smugmug.com")).then_some(url)
}

/// What the upload host answered, once HTTP itself has succeeded.
///
/// The upload endpoint predates API v2 and does not use its envelope: it
/// answers `{"stat": "ok", "Image": {…}}`, and a refusal can arrive as a 200
/// carrying `"stat": "fail"`, so the status code alone does not say whether
/// the image landed.
#[derive(Debug, PartialEq, Eq)]
pub enum UploadOutcome {
    Uploaded(RemoteImageId),
    Refused(String),
}

#[derive(Debug, Deserialize)]
struct RawUploadResponse {
    stat: String,
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
    #[serde(rename = "Image", default)]
    image: Option<RawUploadedImage>,
}

#[derive(Debug, Deserialize)]
struct RawUploadedImage {
    #[serde(rename = "ImageUri")]
    image_uri: String,
}

/// An `Err` here means the body could not be read at all, which after a 2xx
/// is not the same as a refusal: the upload may well have committed.
pub fn parse_upload_response(body: &str) -> Result<UploadOutcome, PublishError> {
    let raw: RawUploadResponse = serde_json::from_str(body)
        .map_err(|e| PublishError::Rejected(format!("unexpected upload response: {e}")))?;

    match (raw.stat.as_str(), raw.image) {
        ("ok", Some(image)) => Ok(UploadOutcome::Uploaded(RemoteImageId(image.image_uri))),
        ("ok", None) => Err(PublishError::Rejected(
            "the upload response reported success but named no image".into(),
        )),
        _ => Ok(UploadOutcome::Refused(format!(
            "SmugMug refused the upload (code {}): {}",
            raw.code
                .map_or_else(|| "none".to_string(), |code| code.to_string()),
            raw.message.as_deref().unwrap_or("no message")
        ))),
    }
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
                    "UrlName": "Faroes-2025",
                    "WebUri": "https://somephotographer.smugmug.com/Faroes-2025",
                    "HasChildren": true
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
    fn reads_the_web_address_and_whether_a_node_has_children() {
        let page = parse_node_children(CHILDREN_BODY).unwrap();
        assert!(page.nodes[0].has_children);
        assert_eq!(
            page.nodes[0].web_uri.as_deref(),
            Some("https://somephotographer.smugmug.com/Faroes-2025")
        );
        assert!(
            !page.nodes[1].has_children,
            "absent is false, as on an album"
        );
        assert_eq!(page.nodes[1].web_uri, None);
    }

    #[test]
    fn reads_an_album_with_its_node() {
        let body = r#"{
            "Response": {
                "Uri": "/api/v2/album/AbCdEf",
                "Album": {
                    "Name": "Iceland 2026",
                    "Uri": "/api/v2/album/AbCdEf",
                    "WebUri": "https://somephotographer.smugmug.com/Iceland-2026",
                    "Privacy": "Unlisted",
                    "Uris": {
                        "Node": { "Uri": "/api/v2/node/1c3l4nd" },
                        "AlbumImages": { "Uri": "/api/v2/album/AbCdEf!images" }
                    }
                }
            },
            "Code": 200
        }"#;
        let album = parse_album(body).unwrap();
        assert_eq!(album.name, "Iceland 2026");
        assert_eq!(album.uri, "/api/v2/album/AbCdEf");
        assert_eq!(album.node_uri, "/api/v2/node/1c3l4nd");
        assert_eq!(
            album.web_uri.as_deref(),
            Some("https://somephotographer.smugmug.com/Iceland-2026")
        );
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
                uploaded_at: None,
                captured_at: None,
                camera_model: None,
                thumbnail_url: None,
            }]
        );
    }

    #[test]
    fn reads_capture_time_and_camera_from_the_metadata_expansion() {
        let body = r#"{
            "Response": {
                "AlbumImage": [
                    {
                        "FileName": "A67023312026-09-11.jpg",
                        "ArchivedSize": 4152509,
                        "DateTimeOriginal": "2026-09-12T01:18:07+00:00",
                        "Uris": {
                            "ImageMetadata": { "Uri": "/api/v2/image/RQHrBvT-0!metadata?_filter=Model" },
                            "ImageSizeDetails": { "Uri": "/api/v2/image/RQHrBvT-0!sizedetails" },
                            "Image": { "Uri": "/api/v2/image/RQHrBvT-0" }
                        }
                    },
                    {
                        "FileName": "PXL_20260911_163152458.jpg",
                        "Uris": {
                            "ImageMetadata": { "Uri": "/api/v2/image/Pxl-0!metadata" },
                            "Image": { "Uri": "/api/v2/image/Pxl-0" }
                        }
                    },
                    {
                        "FileName": "scan.jpg",
                        "Uris": {
                            "ImageSizeDetails": { "Uri": "/api/v2/image/Scan-0!sizedetails" },
                            "Image": { "Uri": "/api/v2/image/Scan-0" }
                        }
                    }
                ]
            },
            "Expansions": {
                "/api/v2/image/RQHrBvT-0!metadata?_filter=Model": {
                    "Locator": "ImageMetadata",
                    "ImageMetadata": {
                        "Model": "ILCE-6700",
                        "DateTimeCreated": "2026-09-11T18:18:07",
                        "MicroDateTimeCreated": ""
                    }
                },
                "/api/v2/image/Pxl-0!metadata": {
                    "ImageMetadata": {
                        "Model": "Pixel 8a",
                        "DateTimeCreated": "2026-09-11T17:31:52",
                        "MicroDateTimeCreated": "2026-09-11T17:31:52.458+01:00"
                    }
                },
                "/api/v2/image/RQHrBvT-0!sizedetails": {
                    "Locator": "ImageSizeDetails",
                    "ImageSizeDetails": {
                        "ImageUrlTemplate": "https://photos.smugmug.com/photos/i-RQHrBvT/0/#size#/i-RQHrBvT-#size#.jpg",
                        "ImageSizeSmall": {
                            "Url": "https://photos.smugmug.com/photos/i-RQHrBvT/0/MPcdXf4tdgrpc3pR2SV7B4Fhsr8mnL9djskBBPsgv/S/i-RQHrBvT-S.jpg",
                            "Width": 400,
                            "Height": 300
                        }
                    }
                },
                "/api/v2/image/Scan-0!sizedetails": {
                    "ImageSizeDetails": { "ImageSizeSmall": { "Url": "" } }
                }
            },
            "Code": 200
        }"#;
        let images = parse_album_images(body).unwrap().images;
        let time = |h, m, s| {
            chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
                .unwrap()
                .and_hms_opt(h, m, s)
                .unwrap()
        };

        assert_eq!(
            images[0].captured_at,
            Some(CaptureTime::new(time(18, 18, 7), None)),
            "the camera's clock, not DateTimeOriginal"
        );
        assert_eq!(images[0].camera_model.as_deref(), Some("ILCE-6700"));
        assert_eq!(
            images[0].thumbnail_url.as_deref(),
            Some(
                "https://photos.smugmug.com/photos/i-RQHrBvT/0/MPcdXf4tdgrpc3pR2SV7B4Fhsr8mnL9djskBBPsgv/S/i-RQHrBvT-S.jpg"
            ),
            "the S size the expansion named, which no other size's URL implies"
        );
        assert_eq!(
            images[1].captured_at,
            Some(CaptureTime::new(time(17, 31, 52), Some(458)))
        );
        assert_eq!(images[2].captured_at, None);
        assert_eq!(images[2].camera_model, None);
        assert_eq!(images[1].thumbnail_url, None, "no size expansion to read");
        assert_eq!(images[2].thumbnail_url, None, "an empty URL is no URL");
    }

    #[test]
    fn only_a_url_on_smugmugs_photo_host_is_fetched_and_it_is_fetched_as_given() {
        let url = "https://photos.smugmug.com/photos/i-R/0/MtW3WZ/S/i-R-S.jpg";
        assert_eq!(photo_host_url(url), Some(url));
        assert_eq!(
            photo_host_url("https://smugmug.com/photos/i-R/0/MtW3WZ/S/i-R-S.jpg"),
            Some("https://smugmug.com/photos/i-R/0/MtW3WZ/S/i-R-S.jpg")
        );
        for refused in [
            "http://photos.smugmug.com/i-R/0/K/S/a-S.jpg",
            "https://photos.smugmug.com.example.org/i-R/0/K/S/a-S.jpg",
            "https://example.org/i-R/0/K/S/a-S.jpg",
            "https://photos.smugmug.com",
        ] {
            assert_eq!(photo_host_url(refused), None, "{refused}");
        }
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

    /// Trimmed from a real upload response. `ImageUri` is what a later
    /// upload hands back in `X-Smug-ImageUri` to replace it.
    #[test]
    fn reads_the_image_uri_an_upload_returns() {
        let body = r#"{
            "stat": "ok",
            "method": "smugmug.images.upload",
            "Image": {
                "ImageUri": "/api/v2/image/XyZ123-0",
                "AlbumImageUri": "/api/v2/album/AbCdEf/image/XyZ123-0",
                "StatusImageReplaceUri": null,
                "URL": "https://somephotographer.smugmug.com/Iceland-2026/i-XyZ123"
            }
        }"#;
        assert_eq!(
            parse_upload_response(body).unwrap(),
            UploadOutcome::Uploaded(RemoteImageId("/api/v2/image/XyZ123-0".into()))
        );
    }

    #[test]
    fn a_failed_stat_is_a_refusal_not_a_parse_error() {
        let body = r#"{"stat": "fail", "method": "smugmug.images.upload", "code": 5, "message": "system error"}"#;
        match parse_upload_response(body).unwrap() {
            UploadOutcome::Refused(detail) => {
                assert!(detail.contains("system error"), "{detail}");
                assert!(detail.contains('5'), "{detail}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_unreadable_upload_response_is_an_error() {
        assert!(parse_upload_response("<html>502 Bad Gateway</html>").is_err());
        assert!(parse_upload_response(r#"{"stat": "ok"}"#).is_err());
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
