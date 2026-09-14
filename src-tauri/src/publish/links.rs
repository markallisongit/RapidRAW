//! Explicit links between RapidRAW albums and remote albums: linking to one
//! that already exists, creating one to link to, and listing what is linked.
//!
//! The rules themselves — one link per remote album, relinking dropping the
//! old album's image records — live in [`PublishState`]; this module decides
//! what to ask the destination and reports refusals in a shape the panel can
//! act on.

use serde::{Deserialize, Serialize};

use crate::file_management::AlbumItem;
use crate::publish::session::{check_account, find_album};
use crate::publish::state::{LinkRecord, LinkRefused, PublishState};
use crate::publish::{
    PublishContext, PublishDestination, PublishError, RemoteContainerId, RemoteNode,
};

/// What the user chose to link an album to.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum LinkTarget {
    /// A remote album that already exists, by its container URI. Never
    /// modified: no rename, no privacy change.
    Existing { remote_uri: String },
    /// A new album directly under the root, with the destination's
    /// configured privacy.
    CreateNew { name: String },
}

/// One link as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkInfo {
    pub album_id: String,
    /// `None` when the local album has been deleted.
    pub album_name: Option<String>,
    /// Group names, outermost first.
    pub album_path: Vec<String>,
    pub remote_uri: String,
    pub remote_name: Option<String>,
    pub web_url: Option<String>,
    pub last_published: Option<String>,
    pub broken: bool,
}

/// Why linking failed, tagged so the panel can offer the way out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum LinkError {
    /// Creating would have reused this same-named album. The panel offers to
    /// link to it instead.
    AlreadyExists {
        remote: RemoteNode,
    },
    /// Another RapidRAW album is linked to that remote album.
    AlreadyLinked {
        album_id: String,
        /// `None` when that album has since been deleted locally.
        album_name: Option<String>,
    },
    Failed {
        message: String,
    },
}

/// For the command layer, whose lookups report plain messages.
impl From<String> for LinkError {
    fn from(message: String) -> Self {
        Self::Failed { message }
    }
}

impl From<PublishError> for LinkError {
    fn from(error: PublishError) -> Self {
        Self::Failed {
            message: error.to_string(),
        }
    }
}

/// Links `album_id` to `target` in `state`, which the caller saves.
///
/// Checks the account before anything remote, as a publish does: the link
/// would otherwise record an album in an account the other links do not
/// belong to.
pub async fn link_album(
    destination: &dyn PublishDestination,
    ctx: &PublishContext,
    state: &mut PublishState,
    tree: &[AlbumItem],
    album_id: &str,
    target: LinkTarget,
) -> Result<LinkInfo, LinkError> {
    if find_album(tree, album_id).is_none() {
        return Err(PublishError::Rejected(format!("no album with id {album_id}")).into());
    }
    check_account(destination, ctx, state).await?;

    let remote = match target {
        LinkTarget::Existing { remote_uri } => {
            destination
                .container(&RemoteContainerId(remote_uri), ctx)
                .await?
        }
        LinkTarget::CreateNew { name } => {
            if let Some(remote) = destination.find_container(&name, ctx).await? {
                return Err(LinkError::AlreadyExists { remote });
            }
            destination.create_container(&name, ctx).await?
        }
    };

    match state.link_album(album_id, &remote) {
        Ok(()) => {}
        Err(LinkRefused::AlreadyLinked { album_id: other }) => {
            let album_name = find_album(tree, &other).map(|(album, _)| album.name);
            return Err(LinkError::AlreadyLinked {
                album_id: other,
                album_name,
            });
        }
        Err(LinkRefused::NotAContainer) => {
            return Err(PublishError::Rejected(format!(
                "\"{}\" is a folder, and photos are published into albums",
                remote.name
            ))
            .into());
        }
    }

    let link = state
        .link(album_id)
        .expect("link_album just recorded this link");
    Ok(link_info(album_id, link, tree))
}

/// Every link, named from the local album tree. Needs no connection.
pub fn list_links(state: &PublishState, tree: &[AlbumItem]) -> Vec<LinkInfo> {
    state
        .links()
        .map(|(album_id, link)| link_info(album_id, link, tree))
        .collect()
}

fn link_info(album_id: &str, link: &LinkRecord, tree: &[AlbumItem]) -> LinkInfo {
    let (album_name, album_path) = match find_album(tree, album_id) {
        Some((album, _)) => (Some(album.name), album.parent_path),
        None => (None, Vec::new()),
    };
    LinkInfo {
        album_id: album_id.to_string(),
        album_name,
        album_path,
        remote_uri: link.remote_uri.clone(),
        remote_name: link.remote_name.clone(),
        web_url: link.web_url.clone(),
        last_published: link.last_published.clone(),
        broken: link.broken,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::file_management::AlbumItem;
    use crate::publish::state::PublishState;
    use crate::publish::{
        AuthChallenge, AuthStatus, ContainerPrivacy, DestinationCapabilities, PublishContext,
        PublishDestination, PublishError, PublishItem, RemoteContainerId, RemoteImageId,
        RemoteNode, RemoteNodeId, RemoteNodeKind,
    };

    fn album_node(key: &str, name: &str) -> RemoteNode {
        RemoteNode {
            id: RemoteNodeId(format!("/api/v2/node/{key}")),
            container: Some(RemoteContainerId(format!("/api/v2/album/{key}"))),
            kind: RemoteNodeKind::Album,
            name: name.into(),
            web_url: Some(format!("https://example.smugmug.com/{key}")),
            has_children: false,
        }
    }

    /// Answers from a fixed set of remote albums, and logs what it was asked.
    struct Stub {
        albums: Vec<RemoteNode>,
        account: String,
        log: Mutex<Vec<String>>,
    }

    impl Stub {
        fn with(albums: Vec<RemoteNode>) -> Self {
            Self {
                albums,
                account: "alice".into(),
                log: Mutex::new(Vec::new()),
            }
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PublishDestination for Stub {
        fn id(&self) -> &'static str {
            "stub"
        }

        fn display_name(&self) -> &'static str {
            "Stub"
        }

        fn capabilities(&self) -> DestinationCapabilities {
            DestinationCapabilities {
                supports_replace: true,
                supports_reconcile: true,
                supports_nested_containers: true,
                max_bytes: None,
                accepted_mime_types: &["image/jpeg"],
                supported_privacy: &[ContainerPrivacy::Public, ContainerPrivacy::Unlisted],
            }
        }

        async fn auth_status(&self, _ctx: &PublishContext) -> Result<AuthStatus, PublishError> {
            Ok(AuthStatus::Connected {
                account: self.account.clone(),
            })
        }

        async fn begin_auth(&self, _ctx: &PublishContext) -> Result<AuthChallenge, PublishError> {
            unimplemented!()
        }

        async fn complete_auth(
            &self,
            _verifier: &str,
            _ctx: &PublishContext,
        ) -> Result<(), PublishError> {
            unimplemented!()
        }

        async fn disconnect(&self, _ctx: &PublishContext) -> Result<(), PublishError> {
            unimplemented!()
        }

        async fn list_containers(
            &self,
            _parent: Option<&RemoteNodeId>,
            _ctx: &PublishContext,
        ) -> Result<Vec<RemoteNode>, PublishError> {
            unimplemented!()
        }

        async fn find_container(
            &self,
            name: &str,
            _ctx: &PublishContext,
        ) -> Result<Option<RemoteNode>, PublishError> {
            self.log.lock().unwrap().push(format!("find:{name}"));
            Ok(self.albums.iter().find(|album| album.name == name).cloned())
        }

        async fn create_container(
            &self,
            name: &str,
            ctx: &PublishContext,
        ) -> Result<RemoteNode, PublishError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("create:{name}:{:?}", ctx.new_container_privacy));
            Ok(album_node("Created", name))
        }

        async fn container(
            &self,
            id: &RemoteContainerId,
            _ctx: &PublishContext,
        ) -> Result<RemoteNode, PublishError> {
            self.log.lock().unwrap().push(format!("get:{}", id.0));
            self.albums
                .iter()
                .find(|album| album.container.as_ref() == Some(id))
                .cloned()
                .ok_or_else(|| PublishError::Rejected("HTTP 404 Not Found".into()))
        }

        async fn publish_image(
            &self,
            _item: &PublishItem<'_>,
            _ctx: &PublishContext,
        ) -> Result<RemoteImageId, PublishError> {
            unimplemented!()
        }

        async fn reconcile(
            &self,
            _container: &RemoteContainerId,
            _expected: &[PublishItem<'_>],
            _ctx: &PublishContext,
        ) -> Result<Vec<(String, RemoteImageId)>, PublishError> {
            unimplemented!()
        }
    }

    fn context(privacy: ContainerPrivacy) -> PublishContext {
        PublishContext {
            state_dir: std::env::temp_dir(),
            consumer: None,
            cancel: Arc::new(AtomicBool::new(false)),
            new_container_privacy: privacy,
        }
    }

    /// "Travel" › "Iceland", and "Best of" at the top level.
    fn tree() -> Vec<AlbumItem> {
        vec![
            AlbumItem::Group {
                id: "travel".into(),
                name: "Travel".into(),
                icon: None,
                children: vec![AlbumItem::Album {
                    id: "iceland".into(),
                    name: "Iceland".into(),
                    icon: None,
                    images: vec!["/p/a.raf".into()],
                }],
            },
            AlbumItem::Album {
                id: "best".into(),
                name: "Best of".into(),
                icon: None,
                images: vec![],
            },
        ]
    }

    fn existing(key: &str) -> LinkTarget {
        LinkTarget::Existing {
            remote_uri: format!("/api/v2/album/{key}"),
        }
    }

    #[tokio::test]
    async fn linking_an_existing_album_records_it_as_it_is() {
        let destination = Stub::with(vec![album_node("Ice", "Iceland on SmugMug")]);
        let mut state = PublishState::empty("stub");

        let info = link_album(
            &destination,
            &context(ContainerPrivacy::Unlisted),
            &mut state,
            &tree(),
            "iceland",
            existing("Ice"),
        )
        .await
        .unwrap();

        assert_eq!(
            info,
            LinkInfo {
                album_id: "iceland".into(),
                album_name: Some("Iceland".into()),
                album_path: vec!["Travel".into()],
                remote_uri: "/api/v2/album/Ice".into(),
                remote_name: Some("Iceland on SmugMug".into()),
                web_url: Some("https://example.smugmug.com/Ice".into()),
                last_published: None,
                broken: false,
            }
        );
        assert_eq!(
            destination.log(),
            ["get:/api/v2/album/Ice"],
            "read, never created or modified"
        );
        assert_eq!(
            state.link("iceland").unwrap().remote_uri,
            "/api/v2/album/Ice"
        );
    }

    #[tokio::test]
    async fn creating_a_new_album_uses_the_configured_privacy() {
        let destination = Stub::with(vec![]);
        let mut state = PublishState::empty("stub");

        let info = link_album(
            &destination,
            &context(ContainerPrivacy::Unlisted),
            &mut state,
            &tree(),
            "best",
            LinkTarget::CreateNew {
                name: "Best of 2026".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            destination.log(),
            ["find:Best of 2026", "create:Best of 2026:Unlisted"]
        );
        assert_eq!(info.remote_uri, "/api/v2/album/Created");
        assert_eq!(info.remote_name.as_deref(), Some("Best of 2026"));
        assert!(state.link("best").is_some());
    }

    #[tokio::test]
    async fn creating_never_silently_reuses_an_album_of_the_same_name() {
        let clash = album_node("Ice", "Iceland");
        let destination = Stub::with(vec![clash.clone()]);
        let mut state = PublishState::empty("stub");

        let error = link_album(
            &destination,
            &context(ContainerPrivacy::Public),
            &mut state,
            &tree(),
            "iceland",
            LinkTarget::CreateNew {
                name: "Iceland".into(),
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error, LinkError::AlreadyExists { remote: clash });
        assert_eq!(destination.log(), ["find:Iceland"]);
        assert!(state.link("iceland").is_none());
    }

    #[tokio::test]
    async fn a_remote_album_linked_to_another_album_is_refused_by_name() {
        let destination = Stub::with(vec![album_node("Ice", "Iceland")]);
        let mut state = PublishState::empty("stub");
        let ctx = context(ContainerPrivacy::Public);
        link_album(
            &destination,
            &ctx,
            &mut state,
            &tree(),
            "iceland",
            existing("Ice"),
        )
        .await
        .unwrap();

        let error = link_album(
            &destination,
            &ctx,
            &mut state,
            &tree(),
            "best",
            existing("Ice"),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error,
            LinkError::AlreadyLinked {
                album_id: "iceland".into(),
                album_name: Some("Iceland".into()),
            }
        );
        assert!(state.link("best").is_none());
    }

    #[tokio::test]
    async fn an_album_that_is_not_in_the_library_is_not_linked() {
        let destination = Stub::with(vec![album_node("Ice", "Iceland")]);
        let mut state = PublishState::empty("stub");

        let error = link_album(
            &destination,
            &context(ContainerPrivacy::Public),
            &mut state,
            &tree(),
            "travel",
            existing("Ice"),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, LinkError::Failed { .. }), "{error:?}");
        assert!(destination.log().is_empty(), "a group is not an album");
    }

    #[tokio::test]
    async fn linking_while_another_account_owns_the_links_is_refused() {
        let destination = Stub::with(vec![album_node("Ice", "Iceland")]);
        let mut state = PublishState::empty("stub");
        state.record_link(
            "best",
            &RemoteContainerId("/api/v2/album/Best".into()),
            None,
        );
        state.claim_account("bob").unwrap();

        let error = link_album(
            &destination,
            &context(ContainerPrivacy::Public),
            &mut state,
            &tree(),
            "iceland",
            existing("Ice"),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, LinkError::Failed { .. }), "{error:?}");
        assert!(destination.log().is_empty());
    }

    #[test]
    fn links_are_listed_with_local_names_and_a_deleted_album_has_none() {
        let mut state = PublishState::empty("stub");
        state
            .link_album("iceland", &album_node("Ice", "Iceland"))
            .unwrap();
        state
            .link_album("deleted", &album_node("Gone", "Gone"))
            .unwrap();

        let links = list_links(&state, &tree());

        let summary: Vec<(&str, Option<&str>, Vec<String>)> = links
            .iter()
            .map(|link| {
                (
                    link.album_id.as_str(),
                    link.album_name.as_deref(),
                    link.album_path.clone(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("deleted", None, vec![]),
                ("iceland", Some("Iceland"), vec!["Travel".to_string()]),
            ]
        );
        assert_eq!(links[1].remote_name.as_deref(), Some("Iceland"));
    }
}
