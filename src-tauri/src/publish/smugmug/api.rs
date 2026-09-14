//! SmugMug API v2 calls: the authenticated user, album lookup and creation,
//! and listing what an album already holds.
//!
//! One rule shapes every request here: the OAuth parameters travel in the
//! `Authorization` header, never the query string. Ordinary API endpoints
//! accept either, but the upload endpoint accepts only the header — a client
//! built on query-string OAuth works all the way through album creation and
//! then fails opaquely on the first upload. One code path, one failure mode.

use serde_json::json;

use crate::publish::oauth1::{self, Credentials};
use crate::publish::smugmug::auth::{body_or_error, client, transport};
use crate::publish::smugmug::model::{
    Album, AuthUser, ChildNode, RemoteImageSummary, parse_album, parse_album_images,
    parse_auth_user, parse_created_node, parse_node_children,
};
use crate::publish::{ContainerPrivacy, PublishError, RemoteContainerId};

/// Overridden only in tests, where a local mock server stands in.
pub const API_BASE: &str = "https://api.smugmug.com";

/// SmugMug clamps this to whatever it allows; a larger page is an
/// optimisation, and following `NextPage` is what makes paging correct.
const PAGE_SIZE: u32 = 100;

/// A `UrlName` can be taken by a sibling whose `Name` differs, so creation
/// retries with a distinct slug. A handful of attempts covers a real album
/// list; past that, something other than a slug collision is wrong.
const URL_NAME_ATTEMPTS: u32 = 5;

/// The SmugMug API v2 endpoints RapidRAW calls, all signed the same way.
pub struct SmugMugApi {
    base_url: String,
    client: reqwest::Client,
    creds: Credentials,
}

impl SmugMugApi {
    pub fn new(creds: Credentials) -> Result<Self, PublishError> {
        Self::with_base_url(API_BASE, creds)
    }

    /// `base_url` carries no trailing slash: every SmugMug URI a response
    /// hands back is an absolute path, and these are joined by concatenation
    /// rather than parsed.
    ///
    /// Fallible because the HTTP client is built once here rather than per
    /// request; a TLS backend that will not start is worth reporting at the
    /// point it is noticed.
    pub fn with_base_url(
        base_url: impl Into<String>,
        creds: Credentials,
    ) -> Result<Self, PublishError> {
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: client()?,
            creds,
        })
    }

    /// The account behind the token, and the root node album lookup starts
    /// from.
    pub async fn auth_user(&self) -> Result<AuthUser, PublishError> {
        let url = format!("{}/api/v2!authuser", self.base_url);
        parse_auth_user(&self.get_signed(&url).await?)
    }

    /// Every child of `parent_node`, across every page: folders, albums and
    /// pages alike, in SmugMug's order.
    pub async fn list_children(&self, parent_node: &str) -> Result<Vec<ChildNode>, PublishError> {
        let mut nodes = Vec::new();
        let mut next = Some(self.children_url(parent_node));

        while let Some(url) = next {
            let page = parse_node_children(&self.get_signed(&url).await?)?;
            nodes.extend(page.nodes);
            next = page
                .next_page
                .map(|path| format!("{}{path}", self.base_url));
        }

        Ok(nodes)
    }

    /// The album directly under `parent_node` whose name is exactly `name`.
    ///
    /// `parent_node` is a node *URI*, as [`AuthUser::node_uri`] hands back.
    /// Matching is exact and case-sensitive: SmugMug lets "Iceland 2026" and
    /// "iceland 2026" coexist, so treating them as one would publish into
    /// whichever happened to be listed first.
    pub async fn find_child_album(
        &self,
        parent_node: &str,
        name: &str,
    ) -> Result<Option<ChildNode>, PublishError> {
        let mut next = Some(self.children_url(parent_node));

        // Every page is searched, not just the first: an account whose albums
        // span several pages would otherwise gain a duplicate per publish.
        // Not `list_children`, so a match on an early page stops the paging.
        while let Some(url) = next {
            let page = parse_node_children(&self.get_signed(&url).await?)?;
            if let Some(found) = page
                .nodes
                .into_iter()
                .find(|node| node.is_album() && node.name == name)
            {
                return Ok(Some(found));
            }
            next = page
                .next_page
                .map(|path| format!("{}{path}", self.base_url));
        }

        Ok(None)
    }

    /// One album, read and never modified.
    pub async fn album(&self, album: &RemoteContainerId) -> Result<Album, PublishError> {
        let url = format!("{}{}", self.base_url, album.0);
        parse_album(&self.get_signed(&url).await?)
    }

    /// Creates an album under `parent_node`. Never reuses one of the same name:
    /// [`find_child_album`](Self::find_child_album) is asked first, so the
    /// user can link to it instead.
    ///
    /// Sends `Privacy` explicitly, which is what SmugMug's own Lightroom plugin
    /// does: `SmNode.create` sets `PUBLIC` when nothing else was chosen. Here
    /// the user chose it in the Publish Manager and sees it before the album
    /// is created, and SmugMug still caps an album at its parent's effective
    /// privacy, so it cannot publish more widely than the folder allows.
    pub async fn create_album(
        &self,
        parent_node: &str,
        name: &str,
        privacy: ContainerPrivacy,
    ) -> Result<ChildNode, PublishError> {
        let url = format!("{}{parent_node}!children", self.base_url);

        for attempt in 0..URL_NAME_ATTEMPTS {
            let body = json!({
                "Type": "Album",
                "Name": name,
                "UrlName": album_url_name(name, attempt),
                "Privacy": smugmug_privacy(privacy),
            });
            match self.post_create(&url, &body).await? {
                Created::Node(payload) => return parse_created_node(&payload),
                Created::UrlNameTaken => continue,
            }
        }

        Err(PublishError::Rejected(format!(
            "could not find a free SmugMug URL name for the album \"{name}\"              after {URL_NAME_ATTEMPTS} attempts"
        )))
    }

    /// Everything the album already holds, across every page.
    ///
    /// This is how an ambiguous upload is resolved: the same request shaping
    /// as the rest of this module, so it is built here rather than alongside
    /// the upload that consumes it.
    pub async fn list_album_images(
        &self,
        album: &RemoteContainerId,
    ) -> Result<Vec<RemoteImageSummary>, PublishError> {
        let mut images = Vec::new();
        let mut next = Some(format!(
            "{}{}!images?count={PAGE_SIZE}",
            self.base_url, album.0
        ));

        while let Some(url) = next {
            let page = parse_album_images(&self.get_signed(&url).await?)?;
            images.extend(page.images);
            next = page
                .next_page
                .map(|path| format!("{}{path}", self.base_url));
        }

        Ok(images)
    }

    fn children_url(&self, parent_node: &str) -> String {
        format!("{}{parent_node}!children?count={PAGE_SIZE}", self.base_url)
    }

    fn authorization(&self, method: &str, url: &str) -> String {
        // No `extra_params`: the query string on `url` is signed by
        // `authorization_header` itself, and a JSON body contributes nothing
        // to an OAuth signature — only form-encoded bodies do.
        oauth1::authorization_header(
            method,
            url,
            &[],
            &self.creds,
            &oauth1::nonce(),
            oauth1::timestamp(),
        )
    }

    async fn get_signed(&self, url: &str) -> Result<String, PublishError> {
        let response = self
            .client
            .get(url)
            .header("Authorization", self.authorization("GET", url))
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(transport)?;

        body_or_error(response).await
    }

    /// A 409 is handled here rather than by `body_or_error` because it is not
    /// a failure at this layer: it says the slug is taken, and the caller has
    /// another one to try.
    async fn post_create(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<Created, PublishError> {
        let response = self
            .client
            .post(url)
            .header("Authorization", self.authorization("POST", url))
            .header("Accept", "application/json")
            .json(body)
            .send()
            .await
            .map_err(transport)?;

        if response.status() == reqwest::StatusCode::CONFLICT {
            return Ok(Created::UrlNameTaken);
        }
        Ok(Created::Node(body_or_error(response).await?))
    }
}

/// The outcome of one creation attempt, which is not simply success or error.
enum Created {
    Node(String),
    UrlNameTaken,
}

/// SmugMug's name for each privacy level. Spelled out rather than borrowed
/// from `ContainerPrivacy`'s serde names, which belong to the settings file.
fn smugmug_privacy(privacy: ContainerPrivacy) -> &'static str {
    match privacy {
        ContainerPrivacy::Public => "Public",
        ContainerPrivacy::Unlisted => "Unlisted",
        ContainerPrivacy::Private => "Private",
    }
}

/// A `UrlName` SmugMug will accept: letters, digits and dashes only, starting
/// with an upper-case letter or a digit.
///
/// `attempt` disambiguates a slug a sibling already holds; attempt 0 carries
/// no suffix so the common case reads as the user named it.
fn album_url_name(name: &str, attempt: u32) -> String {
    let mut slug = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }

    let slug = slug.trim_matches('-');
    let mut slug = match slug.chars().next() {
        // A slug is required, and one starting with a lower-case letter is
        // rejected, so both are corrected rather than reported: the user
        // named an album, not a URL.
        None => "Album".to_string(),
        Some(first) => first.to_ascii_uppercase().to_string() + &slug[first.len_utf8()..],
    };

    if attempt > 0 {
        slug.push_str(&format!("-{}", attempt + 1));
    }
    slug
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wiremock::matchers::{
        body_partial_json, method, path, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::publish::oauth1::Credentials;
    use crate::publish::{ContainerPrivacy, RemoteImageId};

    const ROOT: &str = "/api/v2/node/rootnode";

    fn test_creds() -> Credentials {
        Credentials {
            consumer_key: "consumer-key".into(),
            consumer_secret: "consumer-secret".into(),
            token: Some("access-token".into()),
            token_secret: Some("access-secret".into()),
        }
    }

    fn api(server: &MockServer) -> SmugMugApi {
        SmugMugApi::with_base_url(server.uri(), test_creds()).unwrap()
    }

    /// SmugMug keys albums off an opaque id, so the fixtures derive one from
    /// the name rather than pretending the URI contains the name verbatim.
    fn key(name: &str) -> String {
        name.chars().filter(char::is_ascii_alphanumeric).collect()
    }

    fn album_uri(name: &str) -> String {
        format!("/api/v2/album/{}", key(name))
    }

    /// One child of a node. Only an album carries a `Uris.Album`, which is
    /// what makes a folder of the same name unusable as a publish target.
    fn child(name: &str, node_type: &str) -> Value {
        let mut node = json!({
            "Name": name,
            "Type": node_type,
            "Uri": format!("/api/v2/node/{}", key(name)),
            "UrlName": key(name),
        });
        if node_type == "Album" {
            node["Uris"] = json!({ "Album": { "Uri": album_uri(name) } });
        }
        node
    }

    fn children_page(nodes: Vec<Value>, next_page: Option<&str>) -> String {
        json!({
            "Response": {
                "Uri": format!("{ROOT}!children"),
                "Locator": "Node",
                "LocatorType": "Objects",
                "Node": nodes,
                "Pages": match next_page {
                    Some(next) => json!({ "Total": 99, "Start": 1, "NextPage": next }),
                    None => json!({ "Total": 1, "Start": 1 }),
                },
            },
            "Code": 200,
            "Message": "Ok"
        })
        .to_string()
    }

    fn created_node(name: &str) -> String {
        json!({
            "Response": { "Node": child(name, "Album") },
            "Code": 201,
            "Message": "Created"
        })
        .to_string()
    }

    fn ok(body: String) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body, "application/json")
    }

    async fn mount_children(server: &MockServer, body: String) {
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}!children")))
            .respond_with(ok(body))
            .mount(server)
            .await;
    }

    /// Mounted where a test's point is that nothing was created.
    async fn forbid_creation(server: &MockServer) {
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .named("album creation")
            .mount(server)
            .await;
    }

    /// An account whose albums span more than one page would otherwise be
    /// offered a duplicate of an album it already has.
    #[tokio::test]
    async fn an_album_on_a_later_page_is_still_found() {
        let server = MockServer::start().await;
        let next = format!("{ROOT}!children?start=3&count=2");
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}!children")))
            .and(query_param_is_missing("start"))
            .respond_with(ok(children_page(
                vec![child("Faroes 2025", "Album")],
                Some(&next),
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}!children")))
            .and(query_param("start", "3"))
            .respond_with(ok(children_page(
                vec![child("Iceland 2026", "Album")],
                None,
            )))
            .mount(&server)
            .await;

        let album = api(&server)
            .find_child_album(ROOT, "Iceland 2026")
            .await
            .unwrap()
            .expect("the album on the second page");

        assert_eq!(album.album_uri(), Some(album_uri("Iceland 2026").as_str()));
    }

    #[tokio::test]
    async fn list_children_reads_folders_and_albums_across_every_page() {
        let server = MockServer::start().await;
        let next = format!("{ROOT}!children?start=3&count=2");
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}!children")))
            .and(query_param_is_missing("start"))
            .respond_with(ok(children_page(
                vec![child("Travel", "Folder"), child("Faroes 2025", "Album")],
                Some(&next),
            )))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}!children")))
            .and(query_param("start", "3"))
            .respond_with(ok(children_page(
                vec![child("Iceland 2026", "Album")],
                None,
            )))
            .expect(1)
            .mount(&server)
            .await;

        let nodes = api(&server).list_children(ROOT).await.unwrap();

        let listed: Vec<(&str, bool)> = nodes
            .iter()
            .map(|node| (node.name.as_str(), node.is_album()))
            .collect();
        assert_eq!(
            listed,
            [
                ("Travel", false),
                ("Faroes 2025", true),
                ("Iceland 2026", true)
            ]
        );
    }

    #[tokio::test]
    async fn list_children_lists_a_folder() {
        let server = MockServer::start().await;
        let folder = "/api/v2/node/Travel";
        Mock::given(method("GET"))
            .and(path(format!("{folder}!children")))
            .respond_with(ok(children_page(
                vec![child("Iceland 2026", "Album")],
                None,
            )))
            .expect(1)
            .mount(&server)
            .await;

        let nodes = api(&server).list_children(folder).await.unwrap();

        assert_eq!(nodes.len(), 1);
        assert_eq!(
            nodes[0].album_uri(),
            Some(album_uri("Iceland 2026").as_str())
        );
    }

    #[tokio::test]
    async fn find_child_album_hands_back_the_node_it_matched_exactly() {
        let server = MockServer::start().await;
        mount_children(
            &server,
            children_page(
                vec![
                    child("Iceland 2026 Draft", "Album"),
                    child("iceland 2026", "Album"),
                    child("Iceland 2026", "Folder"),
                    child("Iceland 2026", "Album"),
                ],
                None,
            ),
        )
        .await;

        let found = api(&server)
            .find_child_album(ROOT, "Iceland 2026")
            .await
            .unwrap()
            .expect("the exact album");

        assert!(found.is_album());
        assert_eq!(found.album_uri(), Some(album_uri("Iceland 2026").as_str()));
        assert_eq!(
            api(&server)
                .find_child_album(ROOT, "Iceland")
                .await
                .unwrap()
                .map(|node| node.name),
            None
        );
    }

    #[tokio::test]
    async fn create_album_hands_back_the_created_node() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("{ROOT}!children")))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_raw(created_node("Iceland 2026"), "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let node = api(&server)
            .create_album(ROOT, "Iceland 2026", ContainerPrivacy::Unlisted)
            .await
            .unwrap();

        assert_eq!(node.name, "Iceland 2026");
        assert_eq!(node.album_uri(), Some(album_uri("Iceland 2026").as_str()));
    }

    #[tokio::test]
    async fn album_reads_one_album_without_changing_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/album/AbCdEf"))
            .respond_with(ok(json!({
                "Response": { "Album": {
                    "Name": "Iceland 2026",
                    "Uri": "/api/v2/album/AbCdEf",
                    "WebUri": "https://somephotographer.smugmug.com/Iceland-2026",
                    "Uris": { "Node": { "Uri": "/api/v2/node/1c3l4nd" } }
                }},
                "Code": 200
            })
            .to_string()))
            .expect(1)
            .mount(&server)
            .await;
        forbid_creation(&server).await;

        let album = api(&server)
            .album(&RemoteContainerId("/api/v2/album/AbCdEf".into()))
            .await
            .unwrap();

        assert_eq!(album.name, "Iceland 2026");
        assert_eq!(album.node_uri, "/api/v2/node/1c3l4nd");
        for request in server.received_requests().await.unwrap() {
            assert_eq!(request.method.as_str(), "GET", "{}", request.url);
        }
    }

    #[tokio::test]
    async fn oauth_goes_in_the_authorization_header_not_the_query_string() {
        let server = MockServer::start().await;
        mount_children(&server, children_page(vec![], None)).await;

        api(&server)
            .find_child_album(ROOT, "Iceland 2026")
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let request = requests.first().expect("the children endpoint was called");
        let authorization = request
            .headers
            .get("authorization")
            .expect("every API call is signed")
            .to_str()
            .unwrap();

        assert!(
            authorization.starts_with("OAuth "),
            "expected an OAuth header, got {authorization}"
        );
        assert!(authorization.contains("oauth_signature="));
        assert!(
            !request.url.query().unwrap_or_default().contains("oauth_"),
            "query-string OAuth works here and fails opaquely on upload: {}",
            request.url
        );
        assert_eq!(
            request.headers.get("accept").unwrap().to_str().unwrap(),
            "application/json"
        );
    }

    /// SmugMug rejects a `UrlName` already taken by a sibling, which can
    /// happen even though no sibling carries the wanted `Name` — "Iceland
    /// 2026" and "Iceland-2026" collapse to the same URL slug.
    #[tokio::test]
    async fn a_taken_url_name_is_retried_with_a_distinct_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "UrlName": "Iceland-2026" })))
            .respond_with(ResponseTemplate::new(409).set_body_string("UrlName already taken"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "UrlName": "Iceland-2026-2" })))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_raw(created_node("Iceland 2026"), "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let album = api(&server)
            .create_album(ROOT, "Iceland 2026", ContainerPrivacy::Public)
            .await
            .unwrap();

        assert_eq!(album.album_uri(), Some(album_uri("Iceland 2026").as_str()));
    }

    #[tokio::test]
    async fn a_new_album_is_created_with_the_configured_privacy() {
        for (privacy, expected) in [
            (ContainerPrivacy::Public, "Public"),
            (ContainerPrivacy::Unlisted, "Unlisted"),
            (ContainerPrivacy::Private, "Private"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path(format!("{ROOT}!children")))
                .and(body_partial_json(
                    json!({ "Name": "Iceland 2026", "Privacy": expected }),
                ))
                .respond_with(
                    ResponseTemplate::new(201)
                        .set_body_raw(created_node("Iceland 2026"), "application/json"),
                )
                .expect(1)
                .mount(&server)
                .await;

            api(&server)
                .create_album(ROOT, "Iceland 2026", privacy)
                .await
                .unwrap_or_else(|e| panic!("{expected}: {e}"));
        }
    }

    #[tokio::test]
    async fn list_album_images_reads_every_page() {
        let server = MockServer::start().await;
        let album = RemoteContainerId("/api/v2/album/AbCdEf".into());
        let images_path = "/api/v2/album/AbCdEf!images";
        let next = format!("{images_path}?start=2&count=1");
        Mock::given(method("GET"))
            .and(path(images_path))
            .and(query_param_is_missing("start"))
            .respond_with(ok(images_page(&["DSC_0001.jpg"], Some(&next))))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(images_path))
            .and(query_param("start", "2"))
            .respond_with(ok(images_page(&["DSC_0002.jpg"], None)))
            .mount(&server)
            .await;

        let images = api(&server).list_album_images(&album).await.unwrap();

        let names: Vec<&str> = images.iter().map(|i| i.file_name.as_str()).collect();
        assert_eq!(names, ["DSC_0001.jpg", "DSC_0002.jpg"]);
        assert_eq!(images[0].size_bytes, 4_194_304);
        assert_eq!(
            images[0].image_uri,
            RemoteImageId("/api/v2/image/DSC0001jpg-0".into())
        );
    }

    fn images_page(file_names: &[&str], next_page: Option<&str>) -> String {
        let images: Vec<Value> = file_names
            .iter()
            .map(|name| {
                json!({
                    "FileName": name,
                    "ArchivedSize": 4_194_304,
                    "Uri": format!("/api/v2/album/AbCdEf/image/{}-0", key(name)),
                    "Uris": { "Image": { "Uri": format!("/api/v2/image/{}-0", key(name)) } }
                })
            })
            .collect();
        json!({
            "Response": {
                "AlbumImage": images,
                "Pages": match next_page {
                    Some(next) => json!({ "Total": 2, "Start": 1, "NextPage": next }),
                    None => json!({ "Total": 1, "Start": 1 }),
                },
            },
            "Code": 200,
            "Message": "Ok"
        })
        .to_string()
    }

    #[tokio::test]
    async fn auth_user_reads_the_nickname_and_the_root_node() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2!authuser"))
            .respond_with(ok(json!({
                "Response": { "User": {
                    "NickName": "somephotographer",
                    "Uri": "/api/v2/user/somephotographer",
                    "Uris": { "Node": { "Uri": "/api/v2/node/rootnode" } }
                }},
                "Code": 200
            })
            .to_string()))
            .mount(&server)
            .await;

        let user = api(&server).auth_user().await.unwrap();

        assert_eq!(user.nick_name, "somephotographer");
        assert_eq!(user.node_uri, "/api/v2/node/rootnode");
    }

    #[tokio::test]
    async fn a_rejected_call_reports_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Invalid signature"))
            .mount(&server)
            .await;

        let error = api(&server)
            .find_child_album(ROOT, "Iceland 2026")
            .await
            .unwrap_err();

        assert!(
            matches!(error, PublishError::NotAuthorised(_)),
            "a 401 is not a transport failure: {error}"
        );
    }

    #[test]
    fn a_url_name_keeps_only_what_smugmug_allows() {
        assert_eq!(album_url_name("Iceland 2026", 0), "Iceland-2026");
        assert_eq!(album_url_name("kalsoy / trøllanes", 0), "Kalsoy-tr-llanes");
        assert_eq!(album_url_name("  spaced  out  ", 0), "Spaced-out");
    }

    #[test]
    fn a_url_name_that_would_be_empty_falls_back_to_a_usable_one() {
        assert_eq!(album_url_name("...", 0), "Album");
        assert_eq!(album_url_name("2026", 0), "2026");
    }

    #[test]
    fn each_url_name_attempt_is_distinct() {
        let attempts: Vec<String> = (0..3).map(|n| album_url_name("Iceland 2026", n)).collect();
        assert_eq!(
            attempts,
            ["Iceland-2026", "Iceland-2026-2", "Iceland-2026-3"]
        );
    }
}
