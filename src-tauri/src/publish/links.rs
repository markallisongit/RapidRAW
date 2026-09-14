//! Explicit links between RapidRAW albums and remote albums: linking to one
//! that already exists, creating one to link to, and listing what is linked.
//!
//! The rules themselves — one link per remote album, relinking dropping the
//! old album's image records — live in [`PublishState`]; this module decides
//! what to ask the destination and reports refusals in a shape the panel can
//! act on.
//!
//! Also adopting photos a linked remote album already holds, so linking to a
//! gallery filled by hand does not upload them all again.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::file_management::AlbumItem;
use crate::publish::session::{RenderPipeline, check_account, find_album, upload_file_names};
use crate::publish::state::{Fingerprints, LinkRecord, LinkRefused, PublishAction, PublishState};
use crate::publish::{
    PublishContext, PublishDestination, PublishError, RemoteContainerId, RemoteImage,
    RemoteImageId, RemoteNode,
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

/// How many photos of a linked album its remote album already holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExistingMatch {
    pub matched: usize,
    /// Every photo in the remote album, matched or not.
    pub remote_photos: usize,
    /// What publishing would name the album's first unpublished photo, to
    /// show why nothing matched. `None` when every photo is recorded already.
    pub example_file_name: Option<String>,
}

/// The photos [`match_existing`] found, ready to record.
#[derive(Debug)]
pub struct Adoption {
    album_id: String,
    matches: Vec<(String, Fingerprints, RemoteImageId)>,
    summary: ExistingMatch,
}

impl Adoption {
    pub fn summary(&self) -> &ExistingMatch {
        &self.summary
    }

    /// Records each match as published with its current fingerprints, so it
    /// skips until edited and an edit replaces the remote image in place.
    /// Any match stamps the link published, as a publish would. Returns how
    /// many were recorded; the caller saves.
    pub fn record(self, state: &mut PublishState) -> Result<usize, PublishError> {
        for (path, fingerprints, id) in &self.matches {
            state.record_image(&self.album_id, path, id, fingerprints, None)?;
        }
        if !self.matches.is_empty() {
            state.mark_published(&self.album_id);
        }
        Ok(self.matches.len())
    }
}

/// The images in `album_id`'s remote album. Empty for a destination that
/// cannot list one, which then has nothing to adopt.
pub async fn linked_images(
    destination: &dyn PublishDestination,
    ctx: &PublishContext,
    state: &PublishState,
    album_id: &str,
) -> Result<Vec<RemoteImage>, PublishError> {
    let link = state
        .link(album_id)
        .ok_or_else(|| PublishError::Rejected(format!("album {album_id} is not linked")))?;
    if !destination.capabilities().supports_reconcile {
        return Ok(Vec::new());
    }
    destination
        .list_container_images(&RemoteContainerId(link.remote_uri.clone()), ctx)
        .await
}

/// Matches the album's unpublished photos to `remote` by the exact file name
/// a publish would upload each one as. A name more than one remote image
/// shares is ambiguous and never matched, and sizes are not compared: a
/// manual export's bytes differ from RapidRAW's render. Photos that cannot
/// be fingerprinted or named are left for publishing to report.
pub fn match_existing(
    pipeline: &dyn RenderPipeline,
    state: &PublishState,
    album_id: &str,
    paths: &[String],
    remote: &[RemoteImage],
) -> Adoption {
    let mut by_name: HashMap<&str, Option<&RemoteImageId>> = HashMap::new();
    for image in remote {
        by_name
            .entry(image.file_name.as_str())
            .and_modify(|found| *found = None)
            .or_insert(Some(&image.id));
    }

    // Exactly what a publish would upload as new, so that `{sequence}` and
    // repeated names come out as the upload's would.
    let unpublished: Vec<(usize, Fingerprints)> = paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            let fingerprints = pipeline.fingerprints(path).ok()?;
            (state.classify(album_id, path, &fingerprints) == PublishAction::New)
                .then_some((index, fingerprints))
        })
        .collect();
    let indices: Vec<usize> = unpublished.iter().map(|(index, _)| *index).collect();
    let names = upload_file_names(pipeline, paths, &indices);

    let mut example_file_name = None;
    let mut matches = Vec::new();
    for ((index, fingerprints), name) in unpublished.into_iter().zip(names) {
        let Ok(name) = name else { continue };
        if let Some(Some(id)) = by_name.get(name.as_str()) {
            matches.push((paths[index].clone(), fingerprints, (*id).clone()));
        }
        example_file_name.get_or_insert(name);
    }

    Adoption {
        album_id: album_id.to_string(),
        summary: ExistingMatch {
            matched: matches.len(),
            remote_photos: remote.len(),
            example_file_name,
        },
        matches,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::file_management::AlbumItem;
    use crate::publish::session::{PublishPreview, RenderPipeline, preview};
    use crate::publish::state::{Fingerprints, PublishAction, PublishState};
    use crate::publish::{
        AuthChallenge, AuthStatus, ContainerPrivacy, ContainerSnapshot, DestinationCapabilities,
        PublishContext, PublishDestination, PublishError, PublishItem, RemoteContainerId,
        RemoteImage, RemoteImageId, RemoteNode, RemoteNodeId, RemoteNodeKind,
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
        /// What every album holds.
        images: Vec<RemoteImage>,
        supports_reconcile: bool,
        account: String,
        log: Mutex<Vec<String>>,
    }

    impl Stub {
        fn with(albums: Vec<RemoteNode>) -> Self {
            Self {
                albums,
                images: Vec::new(),
                supports_reconcile: true,
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
                supports_reconcile: self.supports_reconcile,
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

        async fn inspect_container(
            &self,
            _container: &RemoteContainerId,
            _ctx: &PublishContext,
        ) -> Result<Option<ContainerSnapshot>, PublishError> {
            unimplemented!()
        }

        async fn list_container_images(
            &self,
            container: &RemoteContainerId,
            _ctx: &PublishContext,
        ) -> Result<Vec<RemoteImage>, PublishError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("images:{}", container.0));
            Ok(self.images.clone())
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

    /// Fingerprints from the path, an edit when listed in `edited`, and a
    /// name that is the photo's stem, `_VCnn` for a virtual copy, or its
    /// position with `sequence`.
    struct StubPipeline {
        sequence: bool,
        edited: Vec<String>,
    }

    impl StubPipeline {
        fn named_by_file() -> Self {
            Self {
                sequence: false,
                edited: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl RenderPipeline for StubPipeline {
        fn mime(&self) -> &'static str {
            "image/jpeg"
        }

        fn fingerprints(&self, virtual_path: &str) -> Result<Fingerprints, PublishError> {
            let edit = if self.edited.iter().any(|path| path == virtual_path) {
                "edited"
            } else {
                "original"
            };
            Ok(Fingerprints {
                edit_hash: format!("{edit}:{virtual_path}"),
                settings_hash: "settings".into(),
                legacy: String::new(),
            })
        }

        fn file_name(
            &self,
            virtual_path: &str,
            index: usize,
            _total: usize,
        ) -> Result<String, PublishError> {
            if self.sequence {
                return Ok(format!("Trip-{}.jpg", index + 1));
            }
            let (path, copy) = match virtual_path.split_once("?vc=") {
                Some((path, copy)) => (path, format!("_VC{copy:0>2}")),
                None => (virtual_path, String::new()),
            };
            let stem = std::path::Path::new(path).file_stem().unwrap();
            Ok(format!("{}{copy}.jpg", stem.to_str().unwrap()))
        }

        async fn render(
            &self,
            _virtual_paths: &[String],
            _out_dir: &std::path::Path,
        ) -> Result<Vec<Option<std::path::PathBuf>>, PublishError> {
            unimplemented!("matching never renders")
        }
    }

    fn remote_image(key: &str, file_name: &str) -> RemoteImage {
        RemoteImage {
            id: RemoteImageId(format!("/api/v2/image/{key}-0")),
            file_name: file_name.into(),
        }
    }

    fn photos(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| format!("/p/{name}")).collect()
    }

    /// "best" linked to a remote album holding `images`.
    fn linked_to(images: Vec<RemoteImage>) -> (Stub, PublishState) {
        let mut destination = Stub::with(vec![album_node("Best", "Silverstone")]);
        destination.images = images;
        let mut state = PublishState::empty("stub");
        state
            .link_album("best", &album_node("Best", "Silverstone"))
            .unwrap();
        (destination, state)
    }

    #[tokio::test]
    async fn photos_already_in_the_remote_album_are_adopted_by_their_publish_name() {
        let (destination, mut state) = linked_to(vec![
            remote_image("A", "a.jpg"),
            remote_image("Mine", "someone-elses.jpg"),
            remote_image("B", "b.jpg"),
        ]);
        let pipeline = StubPipeline::named_by_file();
        let paths = photos(&["a.raf", "b.raf", "c.raf"]);
        let remote = linked_images(
            &destination,
            &context(ContainerPrivacy::Public),
            &state,
            "best",
        )
        .await
        .unwrap();

        let adoption = match_existing(&pipeline, &state, "best", &paths, &remote);

        assert_eq!(
            adoption.summary(),
            &ExistingMatch {
                matched: 2,
                remote_photos: 3,
                example_file_name: Some("a.jpg".into()),
            }
        );
        assert_eq!(state.link("best").unwrap().last_published, None);
        assert_eq!(adoption.record(&mut state).unwrap(), 2);
        assert!(state.link("best").unwrap().last_published.is_some());
        assert_eq!(
            preview(&pipeline, &state, "best", &paths),
            PublishPreview {
                new: 1,
                skip: 2,
                ..PublishPreview::default()
            }
        );
        assert_eq!(
            state.image_for("best", &paths[1]).unwrap().remote_uri,
            "/api/v2/image/B-0"
        );
        assert_eq!(
            destination.log(),
            ["images:/api/v2/album/Best"],
            "listed, and nothing on the destination changed"
        );
    }

    #[test]
    fn a_name_on_more_than_one_remote_photo_is_never_adopted() {
        let (_, mut state) = linked_to(vec![]);
        let remote = [
            remote_image("A1", "a.jpg"),
            remote_image("A2", "a.jpg"),
            remote_image("B", "b.jpg"),
        ];
        let paths = photos(&["a.raf", "b.raf"]);

        let adoption = match_existing(
            &StubPipeline::named_by_file(),
            &state,
            "best",
            &paths,
            &remote,
        );

        assert_eq!(adoption.summary().matched, 1);
        adoption.record(&mut state).unwrap();
        assert!(state.image_for("best", &paths[0]).is_none());
        assert!(state.image_for("best", &paths[1]).is_some());
    }

    #[test]
    fn sequence_names_and_virtual_copies_match_what_a_publish_would_upload() {
        let (_, mut state) = linked_to(vec![]);
        let paths = photos(&["a.raf", "a.raf?vc=1", "b.raf"]);

        let sequenced = StubPipeline {
            sequence: true,
            edited: Vec::new(),
        };
        let remote = [remote_image("Two", "Trip-2.jpg")];
        let adoption = match_existing(&sequenced, &state, "best", &paths, &remote);
        assert_eq!(adoption.record(&mut state).unwrap(), 1);
        assert!(state.image_for("best", "/p/a.raf?vc=1").is_some());

        let remote = [remote_image("Copy", "a_VC01.jpg")];
        let (_, mut state) = linked_to(vec![]);
        let adoption = match_existing(
            &StubPipeline::named_by_file(),
            &state,
            "best",
            &paths,
            &remote,
        );
        assert_eq!(adoption.record(&mut state).unwrap(), 1);
        assert_eq!(
            state.image_for("best", "/p/a.raf?vc=1").unwrap().remote_uri,
            "/api/v2/image/Copy-0"
        );
    }

    #[test]
    fn editing_an_adopted_photo_replaces_its_remote_image() {
        let (_, mut state) = linked_to(vec![]);
        let paths = photos(&["a.raf"]);
        let remote = [remote_image("A", "a.jpg")];
        match_existing(
            &StubPipeline::named_by_file(),
            &state,
            "best",
            &paths,
            &remote,
        )
        .record(&mut state)
        .unwrap();

        let edited = StubPipeline {
            sequence: false,
            edited: paths.clone(),
        };

        assert_eq!(
            state.classify("best", &paths[0], &edited.fingerprints(&paths[0]).unwrap()),
            PublishAction::Update {
                replaces: RemoteImageId("/api/v2/image/A-0".into())
            }
        );
    }

    #[test]
    fn a_photo_already_recorded_is_left_as_it_is() {
        let (_, mut state) = linked_to(vec![]);
        let pipeline = StubPipeline::named_by_file();
        let paths = photos(&["a.raf"]);
        let recorded = RemoteImageId("/api/v2/image/Uploaded-0".into());
        state
            .record_image(
                "best",
                &paths[0],
                &recorded,
                &pipeline.fingerprints(&paths[0]).unwrap(),
                None,
            )
            .unwrap();

        let adoption = match_existing(
            &pipeline,
            &state,
            "best",
            &paths,
            &[remote_image("A", "a.jpg")],
        );

        assert_eq!(adoption.summary().matched, 0);
        assert_eq!(adoption.record(&mut state).unwrap(), 0);
        assert_eq!(
            state.image_for("best", &paths[0]).unwrap().remote_uri,
            recorded.0
        );
    }

    #[tokio::test]
    async fn listing_needs_a_link_and_is_empty_without_reconcile_support() {
        let (mut destination, state) = linked_to(vec![remote_image("A", "a.jpg")]);
        let ctx = context(ContainerPrivacy::Public);

        assert!(
            linked_images(&destination, &ctx, &state, "iceland")
                .await
                .is_err()
        );

        destination.supports_reconcile = false;
        assert!(
            linked_images(&destination, &ctx, &state, "best")
                .await
                .unwrap()
                .is_empty()
        );
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
