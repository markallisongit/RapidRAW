//! Explicit links between RapidRAW albums and remote albums: linking to one
//! that already exists, creating one to link to, and listing what is linked.
//!
//! The rules themselves — one link per remote album, relinking dropping the
//! old album's image records — live in [`PublishState`]; this module decides
//! what to ask the destination and reports refusals in a shape the panel can
//! act on.
//!
//! Also adopting photos a linked remote album already holds, so linking to a
//! gallery filled by hand does not upload them all again: matched by the name
//! publishing would give them, and failing that by the source file's name,
//! the capture time and how they look. Only exact names are safe to adopt
//! unseen; the panel shows every other pair before it is recorded, because a
//! wrong pair would overwrite a different photo the next time one is edited.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::file_management::AlbumItem;
use crate::publish::session::{RenderPipeline, check_account, find_album, upload_file_names};
use crate::publish::state::{Fingerprints, LinkRecord, LinkRefused, PublishAction, PublishState};
use crate::publish::{
    CaptureTime, PublishContext, PublishDestination, PublishError, RemoteContainerId, RemoteImage,
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

/// A photo paired with an image its linked remote album already holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExistingPair {
    pub path: String,
    pub remote_id: RemoteImageId,
    pub remote_file_name: String,
    pub remote_thumbnail_url: Option<String>,
    pub reasons: Vec<MatchReason>,
    pub confidence: MatchConfidence,
}

/// What a linked album's remote album already holds of its photos.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExistingMatch {
    pub pairs: Vec<ExistingPair>,
    /// Every photo in the remote album, paired or not.
    pub remote_photos: usize,
    /// What publishing would name the album's first unpublished photo, to
    /// show why nothing matched. `None` when every photo is recorded already.
    pub example_file_name: Option<String>,
    /// A remote photo nothing was paired with, to set beside
    /// `example_file_name`.
    pub example_remote_name: Option<String>,
}

/// A pair the user chose to record.
#[derive(Debug, Clone, Deserialize)]
pub struct ChosenPair {
    pub path: String,
    pub remote_id: RemoteImageId,
}

/// What matching reads about a local photo beyond what publishing does.
pub trait PhotoFacts: Send + Sync {
    /// When the photo's source file was taken, and the camera model.
    fn capture(&self, virtual_path: &str) -> (Option<CaptureTime>, Option<String>);
}

/// What [`Candidates::to_look_at`] names, by the indices [`Looks`] keys on.
pub struct ToLookAt<'a> {
    /// Virtual paths.
    pub local: Vec<(usize, &'a str)>,
    pub remote: Vec<(usize, &'a RemoteImage)>,
}

/// A linked album's unpublished photos and the remote images no photo is
/// recorded against: what can still be paired.
#[derive(Debug)]
pub struct Candidates {
    album_id: String,
    /// Exactly the photos a publish would upload as new, with their
    /// fingerprints.
    photos: Vec<(String, Fingerprints)>,
    local: Vec<LocalPhoto>,
    remote: Vec<RemoteImage>,
    remote_photos: usize,
    example_file_name: Option<String>,
}

/// Reads the album's unpublished photos for matching against `remote`, the
/// listing of its linked remote album. Photos already recorded against the
/// link are not candidates, nor are the remote images they are recorded
/// against, compared by `identity`. Photos that cannot be fingerprinted are
/// left for publishing to report.
pub fn candidates(
    pipeline: &dyn RenderPipeline,
    facts: &dyn PhotoFacts,
    state: &PublishState,
    album_id: &str,
    paths: &[String],
    remote: Vec<RemoteImage>,
    identity: &dyn Fn(&RemoteImageId) -> String,
) -> Candidates {
    let remote_photos = remote.len();
    let recorded: HashSet<String> = state
        .link(album_id)
        .map(|link| {
            link.images
                .values()
                .map(|record| identity(&RemoteImageId(record.remote_uri.clone())))
                .collect()
        })
        .unwrap_or_default();
    let remote: Vec<RemoteImage> = remote
        .into_iter()
        .filter(|image| !recorded.contains(&identity(&image.id)))
        .collect();

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
    // Reading capture times costs a sidecar read a photo; skip it when the
    // listing has none to compare with.
    let compare_captures = remote.iter().any(|image| image.captured_at.is_some());

    let mut example_file_name = None;
    let mut photos = Vec::with_capacity(unpublished.len());
    let mut local = Vec::with_capacity(unpublished.len());
    for ((index, fingerprints), name) in unpublished.into_iter().zip(names) {
        let path = &paths[index];
        let publish_name = name.ok();
        if example_file_name.is_none() {
            example_file_name = publish_name.clone();
        }
        let (captured_at, camera_model) = if compare_captures {
            facts.capture(path)
        } else {
            (None, None)
        };
        local.push(LocalPhoto {
            publish_name,
            source_stem: source_stem(path),
            captured_at,
            camera_model,
        });
        photos.push((path.clone(), fingerprints));
    }

    Candidates {
        album_id: album_id.to_string(),
        photos,
        local,
        remote,
        remote_photos,
        example_file_name,
    }
}

impl Candidates {
    /// The photos and remote images whose looks are worth comparing.
    pub fn to_look_at(&self) -> ToLookAt<'_> {
        let (local, remote) = photos_to_look_at(&self.local, &self.remote);
        ToLookAt {
            local: local
                .into_iter()
                .map(|index| (index, self.photos[index].0.as_str()))
                .collect(),
            remote: remote
                .into_iter()
                .map(|index| (index, &self.remote[index]))
                .collect(),
        }
    }

    /// Pairs the candidates, with whatever `looks` could be read.
    pub fn pair(&self, looks: &Looks) -> ExistingMatch {
        let pairings = pair_photos(&self.local, &self.remote, looks);
        let paired: HashSet<usize> = pairings.iter().map(|pair| pair.remote).collect();
        let example_remote_name = self
            .remote
            .iter()
            .enumerate()
            .find(|(index, _)| !paired.contains(index))
            .map(|(_, image)| image.file_name.clone());
        ExistingMatch {
            pairs: pairings
                .into_iter()
                .map(|pairing| {
                    let image = &self.remote[pairing.remote];
                    ExistingPair {
                        path: self.photos[pairing.local].0.clone(),
                        remote_id: image.id.clone(),
                        remote_file_name: image.file_name.clone(),
                        remote_thumbnail_url: image.thumbnail_url.clone(),
                        reasons: pairing.reasons,
                        confidence: pairing.confidence,
                    }
                })
                .collect(),
            remote_photos: self.remote_photos,
            example_file_name: self.example_file_name.clone(),
            example_remote_name,
        }
    }

    /// Records each chosen pair that still holds as published, with the
    /// photo's current fingerprints, so it skips until edited and an edit
    /// replaces the remote image in place. A pair is skipped when its photo
    /// has been recorded since, its remote image is gone or recorded against
    /// another photo, or either side was already chosen. Any pair recorded
    /// stamps the link published, as a publish would. Returns how many were
    /// recorded; the caller saves.
    pub fn adopt(
        &self,
        state: &mut PublishState,
        chosen: &[ChosenPair],
        identity: &dyn Fn(&RemoteImageId) -> String,
    ) -> Result<usize, PublishError> {
        let photos: HashMap<&str, &Fingerprints> = self
            .photos
            .iter()
            .map(|(path, fingerprints)| (path.as_str(), fingerprints))
            .collect();
        let remote: HashMap<String, &RemoteImageId> = self
            .remote
            .iter()
            .map(|image| (identity(&image.id), &image.id))
            .collect();

        let mut used_paths = HashSet::new();
        let mut used_remote = HashSet::new();
        let mut recorded = 0;
        for pair in chosen {
            let key = identity(&pair.remote_id);
            let (Some(fingerprints), Some(id)) = (photos.get(pair.path.as_str()), remote.get(&key))
            else {
                continue;
            };
            if !used_paths.insert(pair.path.as_str()) || !used_remote.insert(key) {
                continue;
            }
            state.record_image(&self.album_id, &pair.path, id, fingerprints, None)?;
            recorded += 1;
        }
        if recorded > 0 {
            state.mark_published(&self.album_id);
        }
        Ok(recorded)
    }
}

/// The stem of a virtual path's source file, which its virtual copies share.
fn source_stem(virtual_path: &str) -> String {
    let (source, _) = crate::file_management::parse_virtual_path(virtual_path);
    source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
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

/// Why a local photo was paired with a remote image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MatchReason {
    /// The remote name is exactly what publishing would call the photo.
    PublishName,
    /// The remote name contains the stem of the photo's source file.
    OriginalFileName,
    /// Taken at the same moment on the same camera model.
    CaptureTime,
    /// The thumbnails are near-identical.
    LooksTheSame,
}

/// How sure a pair is: what the review ticks to begin with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MatchConfidence {
    /// A single signal other than the publish name.
    Possible,
    /// Two or more signals agree.
    Likely,
    /// The publish name, which is safe to adopt without review.
    Exact,
}

/// A local photo as the matcher sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalPhoto {
    /// `None` when the photo cannot be named, which leaves the other signals.
    pub publish_name: Option<String>,
    /// The stem of the source file, which virtual copies share.
    pub source_stem: String,
    pub captured_at: Option<CaptureTime>,
    pub camera_model: Option<String>,
}

/// One pair the matcher found, by index into its inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub local: usize,
    pub remote: usize,
    pub reasons: Vec<MatchReason>,
    pub confidence: MatchConfidence,
}

/// A perceptual hash, compared by the number of bits that differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualHash(Vec<u8>);

impl VisualHash {
    /// As `culling.rs` hashes for similarity. Measured on real pairs (#27),
    /// RapidRAW's thumbnail of an edited photo is 0–4 bits from a hand export
    /// of the same edit, and different photos 36 or more apart.
    pub fn of(image: &image::DynamicImage) -> Self {
        let hasher = image_hasher::HasherConfig::new()
            .hash_alg(image_hasher::HashAlg::DoubleGradient)
            .hash_size(16, 16)
            .to_hasher();
        Self(hasher.hash_image(image).as_bytes().to_vec())
    }

    #[cfg(test)]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn distance(&self, other: &Self) -> u32 {
        self.0
            .iter()
            .zip(&other.0)
            .map(|(a, b)| (a ^ b).count_ones())
            .sum()
    }
}

/// Close enough to count as a signal beside another one, or to break a tie.
pub const LOOKS_CONFIRM_DISTANCE: u32 = 10;
/// Close enough to pair on looks alone, and then only as a possible pair: a
/// burst of similar frames can come this close.
pub const LOOKS_ALONE_DISTANCE: u32 = 6;
/// A shorter source stem (`1`, `IMG`) is contained in too many names to mean
/// anything.
const MIN_STEM_CHARS: usize = 4;

/// Hashes the matcher was given, by index into its inputs.
#[derive(Debug, Default)]
pub struct Looks {
    pub local: HashMap<usize, VisualHash>,
    pub remote: HashMap<usize, VisualHash>,
}

/// Pairs local photos with remote images, strongest evidence first. Each
/// photo and each remote image ends up in at most one pair, and where two
/// candidates are equally good nothing in that group is paired.
///
/// 1. The exact publish name. Always an exact pair.
/// 2. The source file's stem inside the remote name (the longest stem wins)
///    and the capture time with the camera model. A pair both agree on is
///    settled here, before looks are compared.
/// 3. Everything left, with `looks` added: they confirm a pair, break a tie
///    between candidates, and on their own pair only near-identical pictures.
pub fn pair_photos(local: &[LocalPhoto], remote: &[RemoteImage], looks: &Looks) -> Vec<Pairing> {
    let mut matching = settled(local, remote);
    matching.resolve(Some(looks), |_| true);
    let mut pairs = matching.pairs;
    pairs.sort_by_key(|pair| pair.local);
    pairs
}

/// The photos and remote images whose looks [`pair_photos`] would compare:
/// those not already settled by name, or by stem and capture time together.
/// Nothing when either side has nothing left.
pub fn photos_to_look_at(local: &[LocalPhoto], remote: &[RemoteImage]) -> (Vec<usize>, Vec<usize>) {
    let matching = settled(local, remote);
    let free_local = matching.free_local();
    let free_remote = matching.free_remote();
    if free_local.is_empty() || free_remote.is_empty() {
        return (Vec::new(), Vec::new());
    }
    (free_local, free_remote)
}

/// Passes 1 and 2.
fn settled<'a>(local: &'a [LocalPhoto], remote: &'a [RemoteImage]) -> Matching<'a> {
    let mut matching = Matching {
        local,
        remote,
        local_taken: vec![false; local.len()],
        remote_taken: vec![false; remote.len()],
        pairs: Vec::new(),
    };
    matching.pair_publish_names();
    matching.resolve(None, |reasons| reasons.len() >= 2);
    matching
}

struct Matching<'a> {
    local: &'a [LocalPhoto],
    remote: &'a [RemoteImage],
    local_taken: Vec<bool>,
    remote_taken: Vec<bool>,
    pairs: Vec<Pairing>,
}

impl Matching<'_> {
    fn free_local(&self) -> Vec<usize> {
        (0..self.local.len())
            .filter(|&i| !self.local_taken[i])
            .collect()
    }

    fn free_remote(&self) -> Vec<usize> {
        (0..self.remote.len())
            .filter(|&i| !self.remote_taken[i])
            .collect()
    }

    fn take(&mut self, local: usize, remote: usize, reasons: Vec<MatchReason>) {
        let confidence = if reasons.contains(&MatchReason::PublishName) {
            MatchConfidence::Exact
        } else if reasons.len() >= 2 {
            MatchConfidence::Likely
        } else {
            MatchConfidence::Possible
        };
        self.local_taken[local] = true;
        self.remote_taken[remote] = true;
        self.pairs.push(Pairing {
            local,
            remote,
            reasons,
            confidence,
        });
    }

    /// A name on more than one remote image, or claimed by more than one
    /// photo, is ambiguous and pairs nothing.
    fn pair_publish_names(&mut self) {
        let mut remote_by_name: HashMap<&str, Option<usize>> = HashMap::new();
        for (index, image) in self.remote.iter().enumerate() {
            remote_by_name
                .entry(image.file_name.as_str())
                .and_modify(|found| *found = None)
                .or_insert(Some(index));
        }
        let mut claims: HashMap<usize, Option<usize>> = HashMap::new();
        for (index, photo) in self.local.iter().enumerate() {
            let Some(name) = photo.publish_name.as_deref() else {
                continue;
            };
            if let Some(Some(remote)) = remote_by_name.get(name) {
                claims
                    .entry(*remote)
                    .and_modify(|found| *found = None)
                    .or_insert(Some(index));
            }
        }
        let mut found: Vec<(usize, usize)> = claims
            .into_iter()
            .filter_map(|(remote, local)| Some((local?, remote)))
            .collect();
        found.sort_unstable();
        for (local, remote) in found {
            self.take(local, remote, vec![MatchReason::PublishName]);
        }
    }

    /// Repeatedly pairs each photo and remote image whose best candidate is
    /// the other and better than any rival, while `accept` agrees. A group
    /// whose best candidates tie never pairs: its members stay each other's
    /// best, so no round takes them.
    fn resolve(&mut self, looks: Option<&Looks>, accept: impl Fn(&[MatchReason]) -> bool) {
        let edges = self.edges(looks);
        loop {
            let mut best_local: HashMap<usize, (usize, bool)> = HashMap::new();
            let mut best_remote: HashMap<usize, (usize, bool)> = HashMap::new();
            let free = |(local, remote, _): &&(usize, usize, Vec<MatchReason>)| {
                !self.local_taken[*local] && !self.remote_taken[*remote]
            };
            for (local, remote, reasons) in edges.iter().filter(free) {
                for (best, key) in [(&mut best_local, *local), (&mut best_remote, *remote)] {
                    let score = reasons.len();
                    best.entry(key)
                        .and_modify(|(top, unique)| {
                            if score > *top {
                                *top = score;
                                *unique = true;
                            } else if score == *top {
                                *unique = false;
                            }
                        })
                        .or_insert((score, true));
                }
            }
            let is_best = |best: &HashMap<usize, (usize, bool)>, key: usize, score: usize| {
                best.get(&key) == Some(&(score, true))
            };
            let chosen: Vec<(usize, usize, Vec<MatchReason>)> = edges
                .iter()
                .filter(free)
                .filter(|(local, remote, reasons)| {
                    is_best(&best_local, *local, reasons.len())
                        && is_best(&best_remote, *remote, reasons.len())
                        && accept(reasons)
                })
                .cloned()
                .collect();
            if chosen.is_empty() {
                return;
            }
            for (local, remote, reasons) in chosen {
                self.take(local, remote, reasons);
            }
        }
    }

    /// Every candidate pair among the free photos and remote images, with its
    /// signals.
    fn edges(&self, looks: Option<&Looks>) -> Vec<(usize, usize, Vec<MatchReason>)> {
        let free_local = self.free_local();
        let stems: Vec<String> = self
            .local
            .iter()
            .map(|photo| photo.source_stem.to_lowercase())
            .collect();

        let mut edges = Vec::new();
        for remote in self.free_remote() {
            let image = &self.remote[remote];
            let remote_stem = file_stem(&image.file_name).to_lowercase();
            let longest_stem = free_local
                .iter()
                .map(|&local| &stems[local])
                .filter(|stem| {
                    stem.chars().count() >= MIN_STEM_CHARS && remote_stem.contains(*stem)
                })
                .map(|stem| stem.chars().count())
                .max();

            for &local in &free_local {
                let photo = &self.local[local];
                let mut reasons = Vec::new();
                let stem = &stems[local];
                if Some(stem.chars().count()) == longest_stem && remote_stem.contains(stem.as_str())
                {
                    reasons.push(MatchReason::OriginalFileName);
                }
                if same_capture(photo, image) {
                    reasons.push(MatchReason::CaptureTime);
                }
                let distance = looks.and_then(|looks| {
                    Some(
                        looks
                            .local
                            .get(&local)?
                            .distance(looks.remote.get(&remote)?),
                    )
                });
                match distance {
                    Some(d) if d <= LOOKS_ALONE_DISTANCE => reasons.push(MatchReason::LooksTheSame),
                    Some(d) if d <= LOOKS_CONFIRM_DISTANCE && !reasons.is_empty() => {
                        reasons.push(MatchReason::LooksTheSame)
                    }
                    _ => {}
                }
                if !reasons.is_empty() {
                    edges.push((local, remote, reasons));
                }
            }
        }
        edges
    }
}

fn same_capture(photo: &LocalPhoto, image: &RemoteImage) -> bool {
    let same_camera = match (&photo.camera_model, &image.camera_model) {
        (Some(a), Some(b)) => !a.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim()),
        _ => false,
    };
    same_camera
        && matches!((&photo.captured_at, &image.captured_at), (Some(a), Some(b)) if a.same_moment(b))
}

/// Everything before the last `.`, or the whole name without one.
fn file_stem(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
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

    /// Capture times by virtual path, all on one camera; none for the rest.
    #[derive(Default)]
    struct StubFacts(HashMap<String, CaptureTime>);

    impl PhotoFacts for StubFacts {
        fn capture(&self, virtual_path: &str) -> (Option<CaptureTime>, Option<String>) {
            match self.0.get(virtual_path) {
                Some(at) => (Some(*at), Some("ILCE-6700".into())),
                None => (None, None),
            }
        }
    }

    fn same_image(id: &RemoteImageId) -> String {
        id.0.clone()
    }

    /// `second` seconds past 18:18 on the day of the Silverstone photos.
    fn shot(second: u32, millis: Option<u16>) -> CaptureTime {
        let at = chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
            .unwrap()
            .and_hms_opt(18, 18, second)
            .unwrap();
        CaptureTime::new(at, millis)
    }

    fn remote_image(key: &str, file_name: &str) -> RemoteImage {
        RemoteImage::named(RemoteImageId(format!("/api/v2/image/{key}-0")), file_name)
    }

    fn shot_remote(key: &str, file_name: &str, at: CaptureTime) -> RemoteImage {
        RemoteImage {
            captured_at: Some(at),
            camera_model: Some("ILCE-6700".into()),
            ..remote_image(key, file_name)
        }
    }

    fn photos(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| format!("/p/{name}")).collect()
    }

    fn photo(stem: &str) -> LocalPhoto {
        LocalPhoto {
            publish_name: Some(format!("{stem}-published.jpg")),
            source_stem: stem.into(),
            ..LocalPhoto::default()
        }
    }

    fn shot_photo(stem: &str, at: CaptureTime) -> LocalPhoto {
        LocalPhoto {
            captured_at: Some(at),
            camera_model: Some("ilce-6700 ".into()),
            ..photo(stem)
        }
    }

    fn looks(local: &[(usize, u16)], remote: &[(usize, u16)]) -> Looks {
        // Sixteen bits a hash: distance is the number of differing bits.
        let hash = |bits: u16| VisualHash::from_bytes(bits.to_be_bytes().to_vec());
        Looks {
            local: local.iter().map(|&(i, b)| (i, hash(b))).collect(),
            remote: remote.iter().map(|&(i, b)| (i, hash(b))).collect(),
        }
    }

    /// (local, remote, confidence) for each pair.
    fn summary(pairs: &[Pairing]) -> Vec<(usize, usize, MatchConfidence)> {
        pairs
            .iter()
            .map(|pair| (pair.local, pair.remote, pair.confidence))
            .collect()
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

    fn candidates_for(
        pipeline: &StubPipeline,
        facts: &StubFacts,
        state: &PublishState,
        paths: &[String],
        remote: &[RemoteImage],
    ) -> Candidates {
        candidates(
            pipeline,
            facts,
            state,
            "best",
            paths,
            remote.to_vec(),
            &same_image,
        )
    }

    fn choose_all(found: &ExistingMatch) -> Vec<ChosenPair> {
        found
            .pairs
            .iter()
            .map(|pair| ChosenPair {
                path: pair.path.clone(),
                remote_id: pair.remote_id.clone(),
            })
            .collect()
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

        let found = candidates_for(&pipeline, &StubFacts::default(), &state, &paths, &remote);
        let matched = found.pair(&Looks::default());

        assert_eq!(matched.remote_photos, 3);
        assert_eq!(matched.example_file_name.as_deref(), Some("a.jpg"));
        assert_eq!(
            matched.example_remote_name.as_deref(),
            Some("someone-elses.jpg")
        );
        let pairs: Vec<(&str, &str, MatchConfidence)> = matched
            .pairs
            .iter()
            .map(|p| (p.path.as_str(), p.remote_file_name.as_str(), p.confidence))
            .collect();
        assert_eq!(
            pairs,
            [
                ("/p/a.raf", "a.jpg", MatchConfidence::Exact),
                ("/p/b.raf", "b.jpg", MatchConfidence::Exact)
            ]
        );
        assert_eq!(matched.pairs[0].reasons, [MatchReason::PublishName]);

        assert_eq!(state.link("best").unwrap().last_published, None);
        assert_eq!(
            found
                .adopt(&mut state, &choose_all(&matched), &same_image)
                .unwrap(),
            2
        );
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
        let (_, state) = linked_to(vec![]);
        let remote = [
            remote_image("A1", "a.jpg"),
            remote_image("A2", "a.jpg"),
            remote_image("B", "b.jpg"),
        ];
        let paths = photos(&["a.raf", "b.raf"]);

        let matched = candidates_for(
            &StubPipeline::named_by_file(),
            &StubFacts::default(),
            &state,
            &paths,
            &remote,
        )
        .pair(&Looks::default());

        let paired: Vec<&str> = matched.pairs.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paired, ["/p/b.raf"]);
    }

    #[test]
    fn sequence_names_and_virtual_copies_match_what_a_publish_would_upload() {
        let (_, state) = linked_to(vec![]);
        let paths = photos(&["a.raf", "a.raf?vc=1", "b.raf"]);

        let sequenced = StubPipeline {
            sequence: true,
            edited: Vec::new(),
        };
        let remote = [remote_image("Two", "Trip-2.jpg")];
        let matched = candidates_for(&sequenced, &StubFacts::default(), &state, &paths, &remote)
            .pair(&Looks::default());
        assert_eq!(matched.pairs.len(), 1);
        assert_eq!(matched.pairs[0].path, "/p/a.raf?vc=1");

        let remote = [remote_image("Copy", "a_VC01.jpg")];
        let matched = candidates_for(
            &StubPipeline::named_by_file(),
            &StubFacts::default(),
            &state,
            &paths,
            &remote,
        )
        .pair(&Looks::default());
        assert_eq!(matched.pairs.len(), 1);
        assert_eq!(matched.pairs[0].path, "/p/a.raf?vc=1");
        assert_eq!(matched.pairs[0].confidence, MatchConfidence::Exact);
    }

    #[test]
    fn editing_an_adopted_photo_replaces_its_remote_image() {
        let (_, mut state) = linked_to(vec![]);
        let paths = photos(&["a.raf"]);
        let remote = [remote_image("A", "a.jpg")];
        let found = candidates_for(
            &StubPipeline::named_by_file(),
            &StubFacts::default(),
            &state,
            &paths,
            &remote,
        );
        let chosen = choose_all(&found.pair(&Looks::default()));
        found.adopt(&mut state, &chosen, &same_image).unwrap();

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
    fn recorded_photos_and_the_images_they_are_recorded_against_are_not_offered_again() {
        let (_, mut state) = linked_to(vec![]);
        let pipeline = StubPipeline::named_by_file();
        let paths = photos(&["a.raf", "b.raf"]);
        let uploaded = RemoteImageId("/api/v2/image/Uploaded-0".into());
        state
            .record_image(
                "best",
                &paths[0],
                &uploaded,
                &pipeline.fingerprints(&paths[0]).unwrap(),
                None,
            )
            .unwrap();
        let remote = [
            remote_image("A", "a.jpg"),
            RemoteImage::named(uploaded.clone(), "b.jpg"),
        ];

        let found = candidates_for(&pipeline, &StubFacts::default(), &state, &paths, &remote);
        let matched = found.pair(&Looks::default());

        assert!(matched.pairs.is_empty(), "{:?}", matched.pairs);
        assert_eq!(matched.remote_photos, 2);
        assert_eq!(matched.example_file_name.as_deref(), Some("b.jpg"));
        let chosen = [
            ChosenPair {
                path: paths[0].clone(),
                remote_id: RemoteImageId("/api/v2/image/A-0".into()),
            },
            ChosenPair {
                path: paths[1].clone(),
                remote_id: uploaded.clone(),
            },
        ];
        assert_eq!(found.adopt(&mut state, &chosen, &same_image).unwrap(), 0);
        assert_eq!(
            state.image_for("best", &paths[0]).unwrap().remote_uri,
            uploaded.0
        );
        assert!(state.image_for("best", &paths[1]).is_none());
    }

    #[test]
    fn a_renamed_photo_pairs_by_its_original_file_name_and_is_likely_once_confirmed() {
        let remote = [remote_image("A", "A67023252026-09-11.jpg")];

        let alone = pair_photos(&[photo("a6702325")], &remote, &Looks::default());
        assert_eq!(summary(&alone), [(0, 0, MatchConfidence::Possible)]);
        assert_eq!(alone[0].reasons, [MatchReason::OriginalFileName]);

        let remote = [shot_remote("A", "A67023252026-09-11.jpg", shot(7, None))];
        let confirmed = pair_photos(
            &[shot_photo("A6702325", shot(7, Some(120)))],
            &remote,
            &Looks::default(),
        );
        assert_eq!(summary(&confirmed), [(0, 0, MatchConfidence::Likely)]);
        assert_eq!(
            confirmed[0].reasons,
            [MatchReason::OriginalFileName, MatchReason::CaptureTime]
        );
    }

    #[test]
    fn the_longest_original_stem_wins_and_short_stems_mean_nothing() {
        let remote = [remote_image("A", "A67023252026-09-11.jpg")];
        let local = [photo("A670232"), photo("A6702325"), photo("A67")];

        assert_eq!(
            summary(&pair_photos(&local, &remote, &Looks::default())),
            [(1, 0, MatchConfidence::Possible)]
        );
        assert!(pair_photos(&[photo("A67")], &remote, &Looks::default()).is_empty());
    }

    #[test]
    fn two_virtual_copies_of_one_source_and_one_remote_image_are_not_paired() {
        let remote = [remote_image("A", "A6702325-edit.jpg")];
        let copies = [photo("A6702325"), photo("A6702325")];

        assert!(pair_photos(&copies, &remote, &Looks::default()).is_empty());

        // The copy that looks like the remote image separates them.
        let pairs = pair_photos(
            &copies,
            &remote,
            &looks(&[(0, 0x000f), (1, 0xfff0)], &[(0, 0x000e)]),
        );
        assert_eq!(summary(&pairs), [(0, 0, MatchConfidence::Likely)]);
    }

    #[test]
    fn sequence_renamed_photos_pair_by_capture_time_and_camera_model() {
        let remote = [
            shot_remote("One", "Trip-01.jpg", shot(1, None)),
            shot_remote("Two", "Trip-02.jpg", shot(2, None)),
        ];
        let local = [
            shot_photo("DSC0002", shot(2, None)),
            shot_photo("DSC0001", shot(1, None)),
        ];

        let pairs = pair_photos(&local, &remote, &Looks::default());
        assert_eq!(
            summary(&pairs),
            [
                (0, 1, MatchConfidence::Possible),
                (1, 0, MatchConfidence::Possible)
            ]
        );
        assert_eq!(pairs[0].reasons, [MatchReason::CaptureTime]);

        let other_camera = [RemoteImage {
            camera_model: Some("Pixel 8a".into()),
            ..remote[0].clone()
        }];
        assert!(pair_photos(&local, &other_camera, &Looks::default()).is_empty());
    }

    #[test]
    fn burst_frames_in_the_same_second_pair_only_when_something_separates_them() {
        let remote = [
            shot_remote("One", "Trip-01.jpg", shot(5, None)),
            shot_remote("Two", "Trip-02.jpg", shot(5, None)),
        ];
        let local = [
            shot_photo("DSC0001", shot(5, None)),
            shot_photo("DSC0002", shot(5, None)),
        ];

        assert!(pair_photos(&local, &remote, &Looks::default()).is_empty());

        let clear = looks(&[(0, 0x0000), (1, 0xffff)], &[(0, 0x0001), (1, 0xfffe)]);
        assert_eq!(
            summary(&pair_photos(&local, &remote, &clear)),
            [
                (0, 0, MatchConfidence::Likely),
                (1, 1, MatchConfidence::Likely)
            ]
        );

        let unclear = looks(&[(0, 0x0000), (1, 0x0003)], &[(0, 0x0001), (1, 0x0002)]);
        assert!(pair_photos(&local, &remote, &unclear).is_empty());

        let remote = [
            shot_remote("One", "Trip-01.jpg", shot(5, Some(100))),
            shot_remote("Two", "Trip-02.jpg", shot(5, Some(350))),
        ];
        let local = [
            shot_photo("DSC0001", shot(5, Some(350))),
            shot_photo("DSC0002", shot(5, Some(100))),
        ];
        assert_eq!(
            summary(&pair_photos(&local, &remote, &Looks::default())),
            [
                (0, 1, MatchConfidence::Possible),
                (1, 0, MatchConfidence::Possible)
            ],
            "sub-seconds separate them"
        );
    }

    #[test]
    fn one_remote_image_claimed_by_two_photos_pairs_neither() {
        let remote = [shot_remote("One", "Trip-01.jpg", shot(5, None))];
        let local = [
            shot_photo("DSC0001", shot(5, None)),
            shot_photo("DSC0002", shot(5, None)),
        ];

        assert!(pair_photos(&local, &remote, &Looks::default()).is_empty());
        let stronger = [shot_photo("Trip", shot(5, None)), local[1].clone()];
        assert_eq!(
            summary(&pair_photos(&stronger, &remote, &Looks::default())),
            [(0, 0, MatchConfidence::Likely)],
            "a better-evidenced claim wins"
        );
    }

    #[test]
    fn looks_alone_pair_only_near_identical_pictures() {
        let remote = [remote_image("One", "Trip-01.jpg")];
        let local = [photo("DSC0001")];

        let cropped = looks(&[(0, 0x0000)], &[(0, 0xffff)]);
        assert!(pair_photos(&local, &remote, &cropped).is_empty());

        // Seven bits apart would confirm another signal, but pairs nothing alone.
        let confirming_only = looks(&[(0, 0x0000)], &[(0, 0x007f)]);
        assert!(pair_photos(&local, &remote, &confirming_only).is_empty());

        let near = looks(&[(0, 0x0000)], &[(0, 0x0003)]);
        let pairs = pair_photos(&local, &remote, &near);
        assert_eq!(summary(&pairs), [(0, 0, MatchConfidence::Possible)]);
        assert_eq!(pairs[0].reasons, [MatchReason::LooksTheSame]);
    }

    #[test]
    fn looks_are_read_only_for_what_name_stem_and_capture_time_leave_unsettled() {
        let remote = [
            remote_image("Exact", "DSC0001-published.jpg"),
            shot_remote("Both", "DSC0002-renamed.jpg", shot(2, None)),
            remote_image("Stem", "DSC0003-renamed.jpg"),
            remote_image("Nothing", "Trip-09.jpg"),
        ];
        let local = [
            photo("DSC0001"),
            shot_photo("DSC0002", shot(2, None)),
            photo("DSC0003"),
            photo("DSC0004"),
        ];

        assert_eq!(photos_to_look_at(&local, &remote), (vec![2, 3], vec![2, 3]));
        assert_eq!(
            photos_to_look_at(&local[..3], &remote[..3]),
            (vec![2], vec![2])
        );
        assert_eq!(
            photos_to_look_at(&local[..2], &remote),
            (Vec::new(), Vec::new()),
            "no photo left to compare"
        );
    }

    #[test]
    fn a_remote_image_gone_or_a_photo_recorded_since_the_review_is_skipped() {
        let (_, mut state) = linked_to(vec![]);
        let pipeline = StubPipeline::named_by_file();
        let facts = StubFacts::default();
        let paths = photos(&["A6702325.arw", "A6702326.arw", "A6702327.arw"]);
        let listed = [
            remote_image("One", "A67023252026-09-11.jpg"),
            remote_image("Two", "A67023262026-09-11.jpg"),
            remote_image("Three", "A67023272026-09-11.jpg"),
        ];
        let reviewed =
            candidates_for(&pipeline, &facts, &state, &paths, &listed).pair(&Looks::default());
        assert_eq!(reviewed.pairs.len(), 3);
        let mut chosen = choose_all(&reviewed);
        chosen.push(chosen[0].clone());

        // Since the review: "Two" was deleted, and the third photo published.
        state
            .record_image(
                "best",
                &paths[2],
                &RemoteImageId("/api/v2/image/Uploaded-0".into()),
                &pipeline.fingerprints(&paths[2]).unwrap(),
                None,
            )
            .unwrap();
        let now = [listed[0].clone(), listed[2].clone()];
        let fresh = candidates_for(&pipeline, &facts, &state, &paths, &now);

        assert_eq!(fresh.adopt(&mut state, &chosen, &same_image).unwrap(), 1);
        assert_eq!(
            state.image_for("best", &paths[0]).unwrap().remote_uri,
            "/api/v2/image/One-0"
        );
        assert!(state.image_for("best", &paths[1]).is_none());
        assert_eq!(
            state.image_for("best", &paths[2]).unwrap().remote_uri,
            "/api/v2/image/Uploaded-0"
        );
    }

    #[test]
    fn capture_times_are_read_only_when_the_listing_has_some() {
        struct Counting(Mutex<usize>);
        impl PhotoFacts for Counting {
            fn capture(&self, _virtual_path: &str) -> (Option<CaptureTime>, Option<String>) {
                *self.0.lock().unwrap() += 1;
                (Some(shot(1, None)), Some("ILCE-6700".into()))
            }
        }
        let (_, state) = linked_to(vec![]);
        let pipeline = StubPipeline::named_by_file();
        let paths = photos(&["DSC0001.arw", "DSC0002.arw"]);
        let facts = Counting(Mutex::new(0));

        candidates(
            &pipeline,
            &facts,
            &state,
            "best",
            &paths,
            vec![remote_image("A", "Trip.jpg")],
            &same_image,
        );
        assert_eq!(*facts.0.lock().unwrap(), 0);

        let remote = vec![shot_remote("A", "Trip.jpg", shot(1, None))];
        let matched = candidates(
            &pipeline,
            &facts,
            &state,
            "best",
            &paths[..1],
            remote,
            &same_image,
        )
        .pair(&Looks::default());
        assert_eq!(*facts.0.lock().unwrap(), 1);
        assert_eq!(matched.pairs[0].reasons, [MatchReason::CaptureTime]);
    }

    #[test]
    fn capture_times_compare_fractions_only_when_both_sides_have_one() {
        assert_eq!(CaptureTime::millis_from_fraction("45"), Some(450));
        assert_eq!(CaptureTime::millis_from_fraction("4587"), Some(458));
        assert_eq!(CaptureTime::millis_from_fraction(" 007 "), Some(7));
        assert_eq!(CaptureTime::millis_from_fraction(""), None);
        assert_eq!(CaptureTime::millis_from_fraction("4a"), None);

        assert!(shot(1, None).same_moment(&shot(1, Some(200))));
        assert!(shot(1, Some(200)).same_moment(&shot(1, Some(200))));
        assert!(!shot(1, Some(200)).same_moment(&shot(1, Some(210))));
        assert!(!shot(1, None).same_moment(&shot(2, None)));
    }

    #[test]
    fn a_picture_hashes_close_to_itself_resized_and_far_from_another() {
        let gradient =
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(300, 200, |x, y| {
                image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
            }));
        let checks = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(300, 200, |x, y| {
            let on = ((x / 25) + (y / 25)) % 2 == 0;
            image::Rgb(if on { [240, 240, 240] } else { [20, 20, 20] })
        }));

        let original = VisualHash::of(&gradient);
        let smaller =
            VisualHash::of(&gradient.resize(150, 100, image::imageops::FilterType::Triangle));
        assert!(original.distance(&smaller) <= LOOKS_ALONE_DISTANCE);
        assert!(original.distance(&VisualHash::of(&checks)) > LOOKS_CONFIRM_DISTANCE);
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
