//! Publish state: which RapidRAW album is linked to which remote album, what
//! has already been published into each, and whether it has changed since.
//!
//! Two jobs. The remote-ID map is what makes a republish *replace* rather than
//! duplicate — without it every run would add a second copy of every photo.
//! The fingerprints are what make an unchanged album cost nothing: they are
//! compared before rendering, so a skip costs one `stat` and no GPU time.
//!
//! Image records live under their link, not at the top level: a photo in two
//! RapidRAW albums is two uploads, one into each remote album, and each is
//! replaced or skipped on its own.
//!
//! Lives in `app_data_dir/publish/<destination_id>.json`, mirroring
//! `albums/albums.json` (`file_management.rs:838-847`) — unlike the spool this
//! is not regenerable, and losing it means re-uploading the user's library.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::export_processing::{ExportSettings, ResizeOptions, WatermarkSettings};
use crate::file_management::AlbumItem;
use crate::publish::{
    ContainerSnapshot, PublishError, RemoteContainerId, RemoteImageId, RemoteNode,
};

/// Subdirectory of `app_data_dir` holding one file per destination.
const STATE_DIR_NAME: &str = "publish";

/// Bumped only for a format change that an older build could misread. An
/// unrecognised value is an error, never a reset — see [`PublishState::load_from`].
const STATE_VERSION: u32 = 2;

/// Phase 1's format: one container per album, image records keyed on the
/// virtual path alone. Read only to migrate it.
const STATE_VERSION_V1: u32 = 1;

/// A RapidRAW album's link to one remote album (album, gallery, set).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkRecord {
    pub remote_uri: String,
    /// `None` when unknown, as for a link migrated from v1.
    pub remote_name: Option<String>,
    pub web_url: Option<String>,
    /// RFC 3339. A string rather than a `DateTime<Utc>` because chrono's
    /// `serde` feature is not enabled in this tree, as in `spool.rs`.
    pub linked_at: String,
    /// `None` until the first publish through this link.
    pub last_published: Option<String>,
    /// The remote album no longer exists.
    #[serde(default)]
    pub broken: bool,
    /// Keyed on the **full virtual path**, never the source path: virtual
    /// copies are distinct publishable photos, distinguished by a `vc=`
    /// suffix. Keying on the source would collapse every copy of an image onto
    /// one remote image, each republish overwriting the last.
    pub images: BTreeMap<String, ImageRecord>,
}

/// Where one publishable photo ended up, and what it looked like at the time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageRecord {
    pub remote_uri: String,
    pub web_url: Option<String>,
    /// [`Fingerprints::edit_hash`] as of the last successful upload. `None`
    /// only for a record migrated from v1 and not yet confirmed since.
    pub edit_hash: Option<String>,
    /// [`Fingerprints::settings_hash`], and `None` under the same condition.
    pub settings_hash: Option<String>,
    /// A v1 record's combined [`fingerprint`], kept until the split hashes
    /// replace it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_fingerprint: Option<String>,
    pub last_published: String,
}

/// Everything published to one destination.
///
/// `BTreeMap` rather than `HashMap` so the file has a stable key order and a
/// republish produces a minimal diff for anything backing it up.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublishState {
    version: u32,
    destination: String,
    /// The account the ids below belong to. `None` before the first publish.
    account: Option<String>,
    /// Keyed on the RapidRAW album id.
    links: BTreeMap<String, LinkRecord>,
}

/// Why [`PublishState::link_album`] would not link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRefused {
    /// Another RapidRAW album is linked to that remote album.
    AlreadyLinked { album_id: String },
    /// A folder, which photos cannot be published into.
    NotAContainer,
}

/// What a republish would do to one photo, and what the panel previews.
#[derive(Debug, PartialEq, Eq)]
pub enum PublishAction {
    New,
    /// The photo itself changed: its source file or its adjustments.
    Update {
        replaces: RemoteImageId,
    },
    /// Only the output settings changed. Re-uploading is the user's choice,
    /// not a consequence of the edit.
    SettingsChanged {
        replaces: RemoteImageId,
    },
    Skip,
}

/// Published photos a change of output settings would affect.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SettingsImpact {
    /// Uploads, so a photo published into two albums counts twice.
    pub photos: usize,
    /// Links holding at least one of them.
    pub albums: usize,
}

/// Changes made by a read-only refresh from a destination.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RefreshReport {
    pub links_checked: usize,
    pub renamed: usize,
    pub broken: usize,
    pub restored: usize,
    pub images_missing: usize,
}

impl RefreshReport {
    pub fn include(&mut self, other: Self) {
        self.links_checked += other.links_checked;
        self.renamed += other.renamed;
        self.broken += other.broken;
        self.restored += other.restored;
        self.images_missing += other.images_missing;
    }
}

/// A photo's publish inputs, hashed apart so a settings change can be told
/// from an edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprints {
    /// The source file's mtime and size, and the adjustments.
    pub edit_hash: String,
    /// The [`RelevantExportSettings`]. The same for every photo in a session.
    pub settings_hash: String,
    /// The v1 [`fingerprint`] of the same inputs, which is all a record
    /// migrated from v1 can be compared with.
    pub legacy: String,
}

/// The subset of [`ExportSettings`] that changes the bytes that get uploaded.
///
/// Everything left out — `destination_type`, `subfolder`, `preserve_folders`,
/// `filename_template` — decides where a file lands and what it is called, not
/// what is in it. Including them would make renaming the output template
/// re-upload the user's whole library for no visible change.
///
/// `preserve_timestamps` is also left out: it sets the *local* file's mtime,
/// and nothing about that reaches the destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelevantExportSettings {
    pub output_format: String,
    pub jpeg_quality: u8,
    pub resize: Option<ResizeOptions>,
    pub keep_metadata: bool,
    pub strip_gps: bool,
    pub watermark: Option<WatermarkSettings>,
    pub export_masks: bool,
}

impl RelevantExportSettings {
    pub fn from_export_settings(settings: &ExportSettings, output_format: &str) -> Self {
        Self {
            output_format: output_format.to_string(),
            jpeg_quality: settings.jpeg_quality,
            resize: settings.resize.clone(),
            keep_metadata: settings.keep_metadata,
            strip_gps: settings.strip_gps,
            watermark: settings.watermark.clone(),
            export_masks: settings.export_masks,
        }
    }
}

/// Which photos each RapidRAW album holds. Migration needs it to decide which
/// link a v1 image record belongs to; the state file cannot say.
#[derive(Debug, Clone, Default)]
pub struct AlbumMembership(HashMap<String, HashSet<String>>);

impl AlbumMembership {
    /// Every album in the tree, at any depth. Groups hold no photos.
    pub fn from_tree(tree: &[AlbumItem]) -> Self {
        fn collect(items: &[AlbumItem], into: &mut HashMap<String, HashSet<String>>) {
            for item in items {
                match item {
                    AlbumItem::Album { id, images, .. } => {
                        into.entry(id.clone())
                            .or_default()
                            .extend(images.iter().cloned());
                    }
                    AlbumItem::Group { children, .. } => collect(children, into),
                }
            }
        }
        let mut albums = HashMap::new();
        collect(tree, &mut albums);
        Self(albums)
    }

    fn contains(&self, album_id: &str, virtual_path: &str) -> bool {
        self.0
            .get(album_id)
            .is_some_and(|paths| paths.contains(virtual_path))
    }
}

impl PublishState {
    pub fn empty(destination_id: &str) -> Self {
        Self {
            version: STATE_VERSION,
            destination: destination_id.to_string(),
            account: None,
            links: BTreeMap::new(),
        }
    }

    /// Loads `<dir>/<destination_id>.json`, where `dir` is what [`state_dir`]
    /// resolves to — or a temp directory under test.
    pub fn load_in(
        dir: &Path,
        destination_id: &str,
        albums: &AlbumMembership,
    ) -> Result<Self, PublishError> {
        Self::load_from(&state_file(dir, destination_id)?, destination_id, albums)
    }

    /// [`Self::load_in`] with the file named directly, so persistence is testable
    /// against a plain temp directory.
    ///
    /// A missing file is an empty state — nothing has been published yet. A
    /// file that is present but unreadable is an error: silently starting from
    /// empty would re-upload the entire library as duplicates.
    ///
    /// A v1 file is migrated in memory, with `albums` deciding which link each
    /// image record belongs to, and written as v2 by the next save.
    pub fn load_from(
        path: &Path,
        destination_id: &str,
        albums: &AlbumMembership,
    ) -> Result<Self, PublishError> {
        let Some((version, bytes)) = read_versioned(path)? else {
            return Ok(Self::empty(destination_id));
        };
        if version == STATE_VERSION_V1 {
            let v1: v1::State = decode(path, &bytes)?;
            return Ok(Self::migrate(v1, albums));
        }
        decode(path, &bytes)
    }

    pub fn save_in(&self, dir: &Path) -> Result<(), PublishError> {
        self.save_to(&state_file(dir, &self.destination)?)
    }

    /// Atomic — see [`write_atomically`].
    pub fn save_to(&self, path: &Path) -> Result<(), PublishError> {
        let encoded = serde_json::to_vec_pretty(self)
            .map_err(|e| PublishError::Io(format!("encoding publish state: {e}")))?;
        write_atomically(path, &encoded)
    }

    /// The recorded account, without loading or migrating anything else.
    ///
    /// Reading the account needs no album tree, so checking a connection never
    /// has to load — and could never mis-migrate — the image records.
    pub fn account_in(dir: &Path, destination_id: &str) -> Result<Option<String>, PublishError> {
        #[derive(Deserialize)]
        struct AccountProbe {
            account: Option<String>,
        }
        let path = state_file(dir, destination_id)?;
        match read_versioned(&path)? {
            None => Ok(None),
            Some((_, bytes)) => Ok(decode::<AccountProbe>(&path, &bytes)?.account),
        }
    }

    /// Checks that `connected` is the account the links belong to, and records
    /// it when nothing is linked yet.
    ///
    /// Refused when the links belong to someone else: their remote ids name
    /// albums and images in that other account, so publishing would try to
    /// replace images in albums the connected account does not own.
    pub fn claim_account(&mut self, connected: &str) -> Result<(), PublishError> {
        match self.account.as_deref() {
            Some(recorded) if recorded != connected && !self.links.is_empty() => {
                Err(PublishError::NotAuthorised(format!(
                    "the linked albums belong to the account \"{recorded}\", but \
                     \"{connected}\" is connected. Reconnect as {recorded} to publish them"
                )))
            }
            _ => {
                self.account = Some(connected.to_string());
                Ok(())
            }
        }
    }

    #[cfg(test)]
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    pub fn link(&self, album_id: &str) -> Option<&LinkRecord> {
        self.links.get(album_id)
    }

    /// Every link, keyed on the RapidRAW album id, in id order.
    pub fn links(&self) -> impl Iterator<Item = (&String, &LinkRecord)> {
        self.links.iter()
    }

    #[cfg(test)]
    pub fn image_for(&self, album_id: &str, virtual_path: &str) -> Option<&ImageRecord> {
        self.links.get(album_id)?.images.get(virtual_path)
    }

    /// Links `album_id` to `id`. Relinking to the same remote album keeps its
    /// image records; linking to a different one starts afresh, because the
    /// records describe images inside the old album and replacing them would
    /// edit that album rather than fill the new one.
    pub fn record_link(&mut self, album_id: &str, id: &RemoteContainerId, web_url: Option<String>) {
        match self.links.get_mut(album_id) {
            Some(link) if link.remote_uri == id.0 => {
                if web_url.is_some() {
                    link.web_url = web_url;
                }
            }
            _ => {
                self.links.insert(
                    album_id.to_string(),
                    LinkRecord {
                        remote_uri: id.0.clone(),
                        remote_name: None,
                        web_url,
                        linked_at: now_rfc3339(),
                        last_published: None,
                        broken: false,
                        images: BTreeMap::new(),
                    },
                );
            }
        }
    }

    /// Links `album_id` to the remote album `remote`, as the user chose it,
    /// recording its name and address. Relinking keeps or drops the image
    /// records as [`Self::record_link`] does.
    ///
    /// Refused when another RapidRAW album is already linked to `remote`: two
    /// albums publishing into one would replace each other's photos.
    pub fn link_album(&mut self, album_id: &str, remote: &RemoteNode) -> Result<(), LinkRefused> {
        let Some(container) = &remote.container else {
            return Err(LinkRefused::NotAContainer);
        };
        if let Some((other, _)) = self
            .links
            .iter()
            .find(|(id, link)| *id != album_id && link.remote_uri == container.0)
        {
            return Err(LinkRefused::AlreadyLinked {
                album_id: other.clone(),
            });
        }

        self.record_link(album_id, container, remote.web_url.clone());
        if let Some(link) = self.links.get_mut(album_id) {
            link.remote_name = Some(remote.name.clone());
            link.web_url = remote.web_url.clone();
            link.broken = false;
        }
        Ok(())
    }

    /// Forgets the link and what was published through it. Nothing remote is
    /// touched. `false` when the album was not linked.
    pub fn unlink_album(&mut self, album_id: &str) -> bool {
        self.links.remove(album_id).is_some()
    }

    /// Applies one destination snapshot without ever inventing local records.
    /// A missing container keeps its image records in case the link is
    /// restored; missing images in a live container are forgotten so the next
    /// publish adds those photos again rather than replacing deleted ids.
    pub fn apply_snapshot<F>(
        &mut self,
        album_id: &str,
        snapshot: Option<&ContainerSnapshot>,
        image_identity: F,
    ) -> Result<RefreshReport, PublishError>
    where
        F: Fn(&RemoteImageId) -> String,
    {
        let link = self
            .links
            .get_mut(album_id)
            .ok_or_else(|| PublishError::Rejected(format!("album {album_id} is not linked")))?;
        let mut report = RefreshReport {
            links_checked: 1,
            ..RefreshReport::default()
        };

        let Some(snapshot) = snapshot else {
            if !link.broken {
                link.broken = true;
                report.broken = 1;
            }
            return Ok(report);
        };

        if link
            .remote_name
            .as_deref()
            .is_some_and(|name| name != snapshot.name)
        {
            report.renamed = 1;
        }
        link.remote_name = Some(snapshot.name.clone());
        link.web_url = snapshot.web_url.clone();
        if link.broken {
            link.broken = false;
            report.restored = 1;
        }

        let remote_images: HashMap<String, &RemoteImageId> = snapshot
            .images
            .iter()
            .map(|image| (image_identity(image), image))
            .collect();
        let before = link.images.len();
        link.images.retain(|_, image| {
            let recorded = RemoteImageId(image.remote_uri.clone());
            let Some(current) = remote_images.get(&image_identity(&recorded)) else {
                return false;
            };
            // Keep the freshest URI for replacement even when the stable
            // identity matched across a destination-specific revision.
            image.remote_uri.clone_from(&current.0);
            true
        });
        report.images_missing = before - link.images.len();
        Ok(report)
    }

    /// Stamps the link's `last_published`. A no-op for an unlinked album.
    pub fn mark_published(&mut self, album_id: &str) {
        if let Some(link) = self.links.get_mut(album_id) {
            link.last_published = Some(now_rfc3339());
        }
    }

    /// Records a successful upload into `album_id`'s linked album. An error
    /// when the album is not linked: an upload always goes through a link,
    /// and a record with nowhere to live would be silently lost.
    pub fn record_image(
        &mut self,
        album_id: &str,
        virtual_path: &str,
        id: &RemoteImageId,
        fingerprints: &Fingerprints,
        web_url: Option<String>,
    ) -> Result<(), PublishError> {
        let link = self.links.get_mut(album_id).ok_or_else(|| {
            PublishError::Io(format!(
                "recording {virtual_path}: album {album_id} is not linked"
            ))
        })?;
        link.images.insert(
            virtual_path.to_string(),
            ImageRecord {
                remote_uri: id.0.clone(),
                web_url,
                edit_hash: Some(fingerprints.edit_hash.clone()),
                settings_hash: Some(fingerprints.settings_hash.clone()),
                legacy_fingerprint: None,
                last_published: now_rfc3339(),
            },
        );
        Ok(())
    }

    /// New / Update / SettingsChanged / Skip — drives the panel's preview
    /// counts, and is checked before rendering so a `Skip` costs no GPU time.
    ///
    /// A record migrated from v1 has only the combined fingerprint, so an edit
    /// and a settings change look the same: both are `Update`, as in v1.
    pub fn classify(
        &self,
        album_id: &str,
        virtual_path: &str,
        fingerprints: &Fingerprints,
    ) -> PublishAction {
        let Some(record) = self
            .links
            .get(album_id)
            .and_then(|link| link.images.get(virtual_path))
        else {
            return PublishAction::New;
        };
        let replaces = || RemoteImageId(record.remote_uri.clone());

        match (&record.edit_hash, &record.settings_hash) {
            (Some(edit), Some(settings)) => {
                if *edit != fingerprints.edit_hash {
                    PublishAction::Update {
                        replaces: replaces(),
                    }
                } else if *settings != fingerprints.settings_hash {
                    PublishAction::SettingsChanged {
                        replaces: replaces(),
                    }
                } else {
                    PublishAction::Skip
                }
            }
            _ if record.legacy_fingerprint.as_deref() == Some(fingerprints.legacy.as_str()) => {
                PublishAction::Skip
            }
            _ => PublishAction::Update {
                replaces: replaces(),
            },
        }
    }

    /// Called for a photo [`Self::classify`] skipped. A record migrated from
    /// v1 takes the split hashes of what it was just confirmed to match, so a
    /// later settings change can be told from an edit. `true` when the record
    /// changed.
    pub fn confirm_unchanged(
        &mut self,
        album_id: &str,
        virtual_path: &str,
        fingerprints: &Fingerprints,
    ) -> bool {
        let Some(record) = self
            .links
            .get_mut(album_id)
            .and_then(|link| link.images.get_mut(virtual_path))
        else {
            return false;
        };
        if record.legacy_fingerprint.is_none() {
            return false;
        }
        record.edit_hash = Some(fingerprints.edit_hash.clone());
        record.settings_hash = Some(fingerprints.settings_hash.clone());
        record.legacy_fingerprint = None;
        true
    }

    /// Rewrites `settings_hash` without an upload, for keeping the existing
    /// uploads after a settings change — in one album, or in every album when
    /// `album_id` is `None`. Returns how many records changed.
    ///
    /// Records still awaiting their first confirmation after migration are
    /// left alone: without an edit hash, marking their settings current would
    /// also hide any edit made since.
    pub fn mark_settings_current(&mut self, album_id: Option<&str>, settings_hash: &str) -> usize {
        let mut changed = 0;
        for (id, link) in &mut self.links {
            if album_id.is_some_and(|wanted| wanted != id) {
                continue;
            }
            for record in link.images.values_mut() {
                if record.edit_hash.is_some()
                    && record.settings_hash.as_deref() != Some(settings_hash)
                {
                    record.settings_hash = Some(settings_hash.to_string());
                    changed += 1;
                }
            }
        }
        changed
    }

    /// [`Self::mark_settings_current`] for one photo, which a session keeping
    /// existing uploads calls for each settings-only change it leaves alone.
    /// `true` when the record changed.
    pub fn mark_image_settings_current(
        &mut self,
        album_id: &str,
        virtual_path: &str,
        settings_hash: &str,
    ) -> bool {
        let Some(record) = self
            .links
            .get_mut(album_id)
            .and_then(|link| link.images.get_mut(virtual_path))
        else {
            return false;
        };
        if record.edit_hash.is_none() || record.settings_hash.as_deref() == Some(settings_hash) {
            return false;
        }
        record.settings_hash = Some(settings_hash.to_string());
        true
    }

    /// Published photos whose `settings_hash` differs from `settings_hash`.
    /// Compares hashes only, so it needs no photo I/O. Records migrated from
    /// v1 are not counted: their settings are unknown.
    pub fn settings_impact(&self, settings_hash: &str) -> SettingsImpact {
        let mut impact = SettingsImpact::default();
        for link in self.links.values() {
            let photos = link
                .images
                .values()
                .filter(|record| {
                    record
                        .settings_hash
                        .as_deref()
                        .is_some_and(|hash| hash != settings_hash)
                })
                .count();
            if photos > 0 {
                impact.photos += photos;
                impact.albums += 1;
            }
        }
        impact
    }

    /// v1 → v2. Nothing here may cause a re-upload of an unchanged photo.
    ///
    /// Each container becomes a link. A v1 image record has no album, so it
    /// is copied into every migrated link whose RapidRAW album holds that
    /// photo: where a photo sits in several albums only one really has it
    /// remotely, but copying keeps v1's behaviour, and a refresh from the
    /// destination finds the missing ones. A record in no linked album has
    /// nowhere to go and is dropped.
    fn migrate(v1: v1::State, albums: &AlbumMembership) -> Self {
        let mut links: BTreeMap<String, LinkRecord> = v1
            .containers
            .into_iter()
            .map(|(album_id, container)| {
                let link = LinkRecord {
                    remote_uri: container.remote_uri,
                    remote_name: None,
                    web_url: container.web_url,
                    linked_at: container.last_published.clone(),
                    last_published: Some(container.last_published),
                    broken: false,
                    images: BTreeMap::new(),
                };
                (album_id, link)
            })
            .collect();

        let mut dropped = 0;
        for (path, image) in v1.images {
            let mut placed = false;
            for (album_id, link) in &mut links {
                if !albums.contains(album_id, &path) {
                    continue;
                }
                link.images.insert(
                    path.clone(),
                    ImageRecord {
                        remote_uri: image.remote_uri.clone(),
                        web_url: image.web_url.clone(),
                        edit_hash: None,
                        settings_hash: None,
                        legacy_fingerprint: Some(image.fingerprint.clone()),
                        last_published: image.last_published.clone(),
                    },
                );
                placed = true;
            }
            if !placed {
                dropped += 1;
            }
        }

        log::info!(
            "Migrated publish state for {} to version {STATE_VERSION}: {} link(s)",
            v1.destination,
            links.len()
        );
        if dropped > 0 {
            log::warn!(
                "Dropped {dropped} published image record(s) for {} that are in no linked album",
                v1.destination
            );
        }

        Self {
            version: STATE_VERSION,
            destination: v1.destination,
            account: v1.account,
            links,
        }
    }
}

/// Phase 1's file format, kept only so [`PublishState::migrate`] can read it.
mod v1 {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    #[derive(Deserialize)]
    pub struct State {
        pub destination: String,
        pub account: Option<String>,
        pub containers: BTreeMap<String, Container>,
        pub images: BTreeMap<String, Image>,
    }

    #[derive(Deserialize)]
    pub struct Container {
        pub remote_uri: String,
        pub web_url: Option<String>,
        pub last_published: String,
    }

    #[derive(Deserialize)]
    pub struct Image {
        pub remote_uri: String,
        pub web_url: Option<String>,
        pub fingerprint: String,
        pub last_published: String,
    }
}

/// Just enough of the file to decide whether the rest can be trusted.
#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

/// The file's version and contents, or `None` when there is no file.
///
/// Reads the version before the body: a future format may not fit any struct
/// here at all, and the point is to say so rather than to guess.
fn read_versioned(path: &Path) -> Result<Option<(u32, Vec<u8>)>, PublishError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(PublishError::Io(format!(
                "reading publish state {}: {e}",
                path.display()
            )));
        }
    };
    let version = decode::<VersionProbe>(path, &bytes)?.version;
    if version != STATE_VERSION && version != STATE_VERSION_V1 {
        return Err(PublishError::Io(format!(
            "publish state {} has version {version}, expected {STATE_VERSION}: \
             it was written by a newer build of RapidRAW",
            path.display()
        )));
    }
    Ok(Some((version, bytes)))
}

fn decode<T: serde::de::DeserializeOwned>(path: &Path, bytes: &[u8]) -> Result<T, PublishError> {
    serde_json::from_slice(bytes).map_err(|e| {
        PublishError::Io(format!(
            "publish state {} is not readable: {e}",
            path.display()
        ))
    })
}

/// Writes `<path>.tmp`, flushes it to disk, then renames over the target. A
/// kill at any point leaves either the previous file or the new one, never a
/// truncated file — and a stranded `.tmp` is ignored by
/// [`PublishState::load_from`] and overwritten by the next save.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), PublishError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| PublishError::Io(format!("creating {}: {e}", parent.display())))?;
    }

    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)
            .map_err(|e| PublishError::Io(format!("creating {}: {e}", tmp.display())))?;
        file.write_all(bytes)
            .map_err(|e| PublishError::Io(format!("writing {}: {e}", tmp.display())))?;
        // Without this the rename can land before the contents do, and a
        // power loss leaves an empty file where the state used to be.
        file.sync_all()
            .map_err(|e| PublishError::Io(format!("flushing {}: {e}", tmp.display())))?;
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        PublishError::Io(format!(
            "replacing {} with {}: {e}",
            path.display(),
            tmp.display()
        ))
    })
}

/// What has to change about a photo before it is worth uploading again —
/// Lightroom's `metadataThatTriggersRepublish` — hashed as three values: the
/// edit, the output settings, and v1's combination of both.
///
/// The adjustments and settings are canonicalised once for all three.
pub fn fingerprints(
    source_mtime: SystemTime,
    source_size: u64,
    adjustments_json: &str,
    export_settings: &RelevantExportSettings,
) -> Fingerprints {
    let source = source_bytes(source_mtime, source_size);
    let settings = canonical_settings(export_settings);
    let (adjustments, edit_adjustments) = match serde_json::from_str::<Value>(adjustments_json) {
        Ok(mut value) => {
            let all = canonical_json(&value);
            strip_view_only(&mut value);
            (all, canonical_json(&value))
        }
        // Unparseable text still hashes stably, as in `canonical_json_str`.
        Err(_) => (adjustments_json.to_string(), adjustments_json.to_string()),
    };

    let mut edit = blake3::Hasher::new();
    edit.update(&source);
    update_field(&mut edit, edit_adjustments.as_bytes());

    Fingerprints {
        edit_hash: finish(edit),
        settings_hash: hash_settings(&settings),
        // v1 hashed every field, so migrated records are compared with them all.
        legacy: combined_hash(&source, &adjustments, &settings),
    }
}

/// Removes the sidecar fields that are screen state rather than edits, so
/// looking at a photo never makes it upload again:
///
/// - `showClipping`: the clipping overlay. Export always renders without it.
/// - `aiPatches[].isLoading`: set while a patch generates.
///
/// `sectionVisibility` and mask / patch `visible`, `opacity` and `invert` stay:
/// they change the rendered pixels.
fn strip_view_only(adjustments: &mut Value) {
    let Some(map) = adjustments.as_object_mut() else {
        return;
    };
    map.remove("showClipping");
    if let Some(Value::Array(patches)) = map.get_mut("aiPatches") {
        for patch in patches.iter_mut().filter_map(Value::as_object_mut) {
            patch.remove("isLoading");
        }
    }
}

/// [`Fingerprints::settings_hash`] alone, for comparing candidate settings
/// against what is published without touching any photo.
pub fn settings_hash(export_settings: &RelevantExportSettings) -> String {
    hash_settings(&canonical_settings(export_settings))
}

/// The v1 fingerprint: source, adjustments and export settings in one hash.
/// Only records migrated from v1 are compared with it, through
/// [`Fingerprints::legacy`], which computes the same value without
/// canonicalising the inputs twice; this is the reference it is tested against.
///
/// Fields are length-prefixed rather than concatenated, so no rearrangement of
/// one field's contents can imitate another's ("ab" + "c" and "a" + "bc" hash
/// differently).
#[cfg(test)]
pub fn fingerprint(
    source_mtime: SystemTime,
    source_size: u64,
    adjustments_json: &str,
    export_settings: &RelevantExportSettings,
) -> String {
    combined_hash(
        &source_bytes(source_mtime, source_size),
        &canonical_json_str(adjustments_json),
        &canonical_settings(export_settings),
    )
}

fn combined_hash(source: &[u8], adjustments: &str, settings: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(source);
    update_field(&mut hasher, adjustments.as_bytes());
    update_field(&mut hasher, settings.as_bytes());
    finish(hasher)
}

fn hash_settings(canonical_settings: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    update_field(&mut hasher, canonical_settings.as_bytes());
    finish(hasher)
}

/// The source file's mtime and size as fixed-width bytes. Signed nanoseconds
/// from the epoch, so a pre-1970 mtime stays distinct rather than saturating
/// to zero with every other pre-epoch timestamp.
fn source_bytes(source_mtime: SystemTime, source_size: u64) -> [u8; 24] {
    let mtime_nanos = match source_mtime.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => since.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    let mut bytes = [0u8; 24];
    bytes[..16].copy_from_slice(&mtime_nanos.to_le_bytes());
    bytes[16..].copy_from_slice(&source_size.to_le_bytes());
    bytes
}

/// Through `to_value` and back so the settings hash the same way the
/// adjustments do: sorted keys, no dependence on field declaration order.
fn canonical_settings(export_settings: &RelevantExportSettings) -> String {
    serde_json::to_value(export_settings)
        .map(|value| canonical_json(&value))
        .unwrap_or_default()
}

fn update_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn finish(hasher: blake3::Hasher) -> String {
    format!("b3:{}", hasher.finalize().to_hex())
}

/// Canonicalises a JSON document, falling back to the raw text when it is not
/// JSON at all — an unparseable adjustments blob should still produce a stable
/// fingerprint rather than collapsing every such image onto one hash.
/// [`fingerprints`] does the same inline, keeping the parsed value to strip.
#[cfg(test)]
fn canonical_json_str(json: &str) -> String {
    match serde_json::from_str::<Value>(json) {
        Ok(value) => canonical_json(&value),
        Err(_) => json.to_string(),
    }
}

/// Re-renders JSON with every object's keys sorted.
///
/// `serde_json::Map` is a `BTreeMap` in this tree, so `to_string` already emits
/// sorted keys — but only until some dependency turns on the `preserve_order`
/// feature, at which point key order would silently become insertion order and
/// every fingerprint would start changing between runs. Sorting explicitly
/// costs one pass and does not depend on that.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        Value::String(key.clone()),
                        canonical_json(&map[key])
                    )
                })
                .collect();
            format!("{{{}}}", body.join(","))
        }
        // Arrays are ordered by definition: a mask list in a different order is
        // a different edit.
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", body.join(","))
        }
        scalar => scalar.to_string(),
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// `app_data_dir/publish`, creating it on demand exactly as `get_albums_path`
/// does for albums.
pub fn state_dir(app_handle: &AppHandle) -> Result<PathBuf, PublishError> {
    let dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| PublishError::Io(format!("resolving the app data directory: {e}")))?
        .join(STATE_DIR_NAME);
    std::fs::create_dir_all(&dir)
        .map_err(|e| PublishError::Io(format!("creating {}: {e}", dir.display())))?;
    Ok(dir)
}

/// `<dir>/<destination_id>.json`.
fn state_file(dir: &Path, destination_id: &str) -> Result<PathBuf, PublishError> {
    destination_file(dir, destination_id, "json")
}

/// `<dir>/<destination_id>.<extension>`, for every per-destination file.
pub(crate) fn destination_file(
    dir: &Path,
    destination_id: &str,
    extension: &str,
) -> Result<PathBuf, PublishError> {
    // Destination ids are `&'static str` constants today, but this path is
    // derived from one, so check rather than trust: a `../` in an id would
    // write outside the state directory.
    if destination_id.is_empty()
        || !destination_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(PublishError::Io(format!(
            "destination id {destination_id:?} is not usable as a file name"
        )));
    }
    Ok(dir.join(format!("{destination_id}.{extension}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export_processing::{ResizeMode, TiffBitDepth, WatermarkAnchor};

    /// A baseline `ExportSettings` for the fingerprint tests to vary one field
    /// of at a time.
    fn export_settings() -> ExportSettings {
        ExportSettings {
            jpeg_quality: 90,
            tiff_bit_depth: TiffBitDepth::default(),
            resize: Some(ResizeOptions {
                mode: ResizeMode::LongEdge,
                value: 2048,
                dont_enlarge: true,
            }),
            keep_metadata: true,
            preserve_timestamps: false,
            strip_gps: true,
            filename_template: Some("{original_filename}".into()),
            watermark: Some(WatermarkSettings {
                path: "/w/mark.png".into(),
                anchor: WatermarkAnchor::BottomRight,
                scale: 10.0,
                spacing: 2.0,
                opacity: 80.0,
            }),
            export_masks: false,
            preserve_folders: false,
            destination_type: Some("customFolder".into()),
            subfolder: Some("published".into()),
        }
    }

    fn relevant() -> RelevantExportSettings {
        RelevantExportSettings::from_export_settings(&export_settings(), "jpeg")
    }

    fn a_time() -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_757_000_000)
    }

    fn prints(edit: &str, settings: &str) -> Fingerprints {
        Fingerprints {
            edit_hash: edit.into(),
            settings_hash: settings.into(),
            legacy: format!("legacy:{edit}:{settings}"),
        }
    }

    fn album_uri(album_id: &str) -> RemoteContainerId {
        RemoteContainerId(format!("/api/v2/album/{album_id}"))
    }

    fn update(uri: &str) -> PublishAction {
        PublishAction::Update {
            replaces: RemoteImageId(uri.into()),
        }
    }

    fn exact_image_identity(image: &RemoteImageId) -> String {
        image.0.clone()
    }

    fn record(state: &mut PublishState, album_id: &str, path: &str, uri: &str, fp: &Fingerprints) {
        state
            .record_image(album_id, path, &RemoteImageId(uri.into()), fp, None)
            .unwrap();
    }

    /// A state with `album_id` linked and `paths` published into it.
    fn published(album_id: &str, paths: &[(&str, &str, Fingerprints)]) -> PublishState {
        let mut state = PublishState::empty("smugmug");
        state.record_link(album_id, &album_uri(album_id), None);
        for (path, uri, fp) in paths {
            record(&mut state, album_id, path, uri, fp);
        }
        state
    }

    fn membership(albums: &[(&str, &[&str])]) -> AlbumMembership {
        let items: Vec<AlbumItem> = albums
            .iter()
            .map(|(id, images)| AlbumItem::Album {
                id: (*id).into(),
                name: (*id).into(),
                icon: None,
                images: images.iter().map(|path| (*path).into()).collect(),
            })
            .collect();
        AlbumMembership::from_tree(&[AlbumItem::Group {
            id: "group".into(),
            name: "Travel".into(),
            icon: None,
            children: items,
        }])
    }

    /// A phase 1 file: two containers, and images keyed on path alone.
    fn write_v1(path: &Path, images: Value) {
        let document = serde_json::json!({
            "version": 1,
            "destination": "smugmug",
            "account": "markallison",
            "containers": {
                "iceland": {
                    "remote_uri": "/api/v2/album/Ice",
                    "web_url": "https://example.smugmug.com/Iceland",
                    "last_published": "2026-09-01T10:00:00+00:00"
                },
                "best": {
                    "remote_uri": "/api/v2/album/Best",
                    "web_url": null,
                    "last_published": "2026-09-02T10:00:00+00:00"
                }
            },
            "images": images
        });
        std::fs::write(path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    }

    fn v1_image(uri: &str, fingerprint: &str) -> Value {
        serde_json::json!({
            "remote_uri": uri,
            "web_url": null,
            "fingerprint": fingerprint,
            "last_published": "2026-09-01T10:00:00+00:00"
        })
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");

        let mut state = published(
            "album-1",
            &[(
                "/photos/a.raf",
                "/api/v2/image/XyZ123-0",
                prints("b3:e1", "b3:s1"),
            )],
        );
        state.account = Some("markallison".into());
        state.record_link(
            "album-2",
            &album_uri("album-2"),
            Some("https://example.smugmug.com/Album".into()),
        );
        state.mark_published("album-1");
        state.save_to(&path).unwrap();

        let loaded =
            PublishState::load_from(&path, "smugmug", &AlbumMembership::default()).unwrap();

        assert_eq!(loaded, state);
        let written = read_json(&path);
        assert_eq!(written["version"], 2);
        let image = &written["links"]["album-1"]["images"]["/photos/a.raf"];
        assert_eq!(image["edit_hash"], "b3:e1");
        assert_eq!(image["settings_hash"], "b3:s1");
        assert!(
            image.get("legacy_fingerprint").is_none(),
            "only migrated records carry a legacy fingerprint: {image}"
        );
        assert!(written["links"]["album-1"]["last_published"].is_string());
        assert!(written["links"]["album-2"]["last_published"].is_null());
        assert!(written["links"]["album-2"]["remote_name"].is_null());
        assert_eq!(written["links"]["album-2"]["broken"], false);
    }

    #[test]
    fn classify_separates_edits_from_settings_changes() {
        let state = published("a", &[("/p/a.raf", "/img/1", prints("e1", "s1"))]);

        assert_eq!(
            state.classify("a", "/p/new.raf", &prints("e1", "s1")),
            PublishAction::New
        );
        assert_eq!(
            state.classify("unlinked", "/p/a.raf", &prints("e1", "s1")),
            PublishAction::New
        );
        assert_eq!(
            state.classify("a", "/p/a.raf", &prints("e1", "s1")),
            PublishAction::Skip
        );
        assert_eq!(
            state.classify("a", "/p/a.raf", &prints("e2", "s1")),
            update("/img/1"),
            "an edit"
        );
        assert_eq!(
            state.classify("a", "/p/a.raf", &prints("e1", "s2")),
            PublishAction::SettingsChanged {
                replaces: RemoteImageId("/img/1".into())
            },
            "settings only"
        );
        assert_eq!(
            state.classify("a", "/p/a.raf", &prints("e2", "s2")),
            update("/img/1"),
            "an edit and a settings change is an edit"
        );
    }

    #[test]
    fn a_photo_in_two_albums_is_published_into_each_independently() {
        let mut state = published("a", &[("/p/shared.raf", "/img/in-a", prints("e1", "s1"))]);
        state.record_link("b", &album_uri("b"), None);

        assert_eq!(
            state.classify("b", "/p/shared.raf", &prints("e1", "s1")),
            PublishAction::New,
            "published into A says nothing about B"
        );
        record(
            &mut state,
            "b",
            "/p/shared.raf",
            "/img/in-b",
            &prints("e1", "s1"),
        );

        assert_eq!(
            state.image_for("a", "/p/shared.raf").unwrap().remote_uri,
            "/img/in-a",
            "A's record is untouched"
        );
        assert_eq!(
            state.image_for("b", "/p/shared.raf").unwrap().remote_uri,
            "/img/in-b"
        );
        assert_eq!(
            state.classify("b", "/p/shared.raf", &prints("e2", "s1")),
            update("/img/in-b"),
            "an edit replaces B's copy, not A's"
        );
    }

    #[test]
    fn virtual_copies_are_distinct_entries() {
        let state = published(
            "a",
            &[
                ("/p/a.raf", "/img/1", prints("e1", "s1")),
                ("/p/a.raf?vc=1", "/img/2", prints("e2", "s1")),
            ],
        );

        assert_eq!(
            state.image_for("a", "/p/a.raf").unwrap().remote_uri,
            "/img/1"
        );
        assert_eq!(
            state.image_for("a", "/p/a.raf?vc=1").unwrap().remote_uri,
            "/img/2"
        );
    }

    #[test]
    fn recording_an_image_needs_a_link() {
        let mut state = PublishState::empty("smugmug");

        let recorded = state.record_image(
            "unlinked",
            "/p/a.raf",
            &RemoteImageId("/img/1".into()),
            &prints("e1", "s1"),
            None,
        );

        assert!(recorded.is_err());
    }

    #[test]
    fn relinking_keeps_records_only_for_the_same_remote_album() {
        let mut state = published("a", &[("/p/a.raf", "/img/1", prints("e1", "s1"))]);
        let linked_at = state.link("a").unwrap().linked_at.clone();

        state.record_link("a", &album_uri("a"), None);
        assert!(state.image_for("a", "/p/a.raf").is_some());
        assert_eq!(state.link("a").unwrap().linked_at, linked_at);

        state.record_link("a", &RemoteContainerId("/api/v2/album/Other".into()), None);
        assert!(
            state.image_for("a", "/p/a.raf").is_none(),
            "records of images in the old album must not be replaced into the new one"
        );
    }

    #[test]
    fn a_missing_container_is_broken_until_it_returns_without_losing_images() {
        let mut state = published("a", &[("/p/a.raf", "/img/1", prints("e1", "s1"))]);

        let missing = state
            .apply_snapshot("a", None, exact_image_identity)
            .unwrap();
        assert_eq!(
            missing,
            RefreshReport {
                links_checked: 1,
                broken: 1,
                ..RefreshReport::default()
            }
        );
        assert!(state.link("a").unwrap().broken);
        assert!(state.image_for("a", "/p/a.raf").is_some());
        assert_eq!(
            state
                .apply_snapshot("a", None, exact_image_identity)
                .unwrap()
                .broken,
            0
        );

        let restored = state
            .apply_snapshot(
                "a",
                Some(&ContainerSnapshot {
                    name: "A".into(),
                    web_url: Some("https://example.test/a".into()),
                    images: vec![RemoteImageId("/img/1".into())],
                }),
                exact_image_identity,
            )
            .unwrap();
        assert_eq!(restored.restored, 1);
        assert!(!state.link("a").unwrap().broken);
        assert!(state.image_for("a", "/p/a.raf").is_some());
    }

    #[test]
    fn refresh_updates_a_rename_and_drops_only_recorded_images_that_are_missing() {
        let mut state = published(
            "a",
            &[
                ("/p/a.raf", "/img/1", prints("e1", "s1")),
                ("/p/b.raf", "/img/2", prints("e2", "s1")),
            ],
        );
        state
            .link_album("a", &remote_album("a", "Old name"))
            .unwrap();

        let report = state
            .apply_snapshot(
                "a",
                Some(&ContainerSnapshot {
                    name: "New name".into(),
                    web_url: Some("https://example.test/new-name".into()),
                    // `/img/3` was added outside RapidRAW and stays ignored.
                    images: vec![
                        RemoteImageId("/img/1".into()),
                        RemoteImageId("/img/3".into()),
                    ],
                }),
                exact_image_identity,
            )
            .unwrap();

        assert_eq!(report.renamed, 1);
        assert_eq!(report.images_missing, 1);
        let link = state.link("a").unwrap();
        assert_eq!(link.remote_name.as_deref(), Some("New name"));
        assert_eq!(
            link.web_url.as_deref(),
            Some("https://example.test/new-name")
        );
        assert!(state.image_for("a", "/p/a.raf").is_some());
        assert!(state.image_for("a", "/p/b.raf").is_none());
        assert_eq!(
            state.classify("a", "/p/b.raf", &prints("e2", "s1")),
            PublishAction::New,
            "a remotely deleted image uploads as new next time"
        );
    }

    #[test]
    fn refresh_keeps_a_record_when_destination_specific_image_revisions_differ() {
        let mut state = published(
            "a",
            &[("/p/a.raf", "/api/v2/image/Key-2", prints("e1", "s1"))],
        );
        let without_revision = |image: &RemoteImageId| {
            image
                .0
                .rsplit_once('-')
                .map_or_else(|| image.0.clone(), |(identity, _)| identity.to_string())
        };

        let report = state
            .apply_snapshot(
                "a",
                Some(&ContainerSnapshot {
                    name: "A".into(),
                    web_url: None,
                    images: vec![RemoteImageId("/api/v2/image/Key-0".into())],
                }),
                without_revision,
            )
            .unwrap();

        assert_eq!(report.images_missing, 0);
        assert_eq!(
            state.image_for("a", "/p/a.raf").unwrap().remote_uri,
            "/api/v2/image/Key-0",
            "the current URI is retained for the next replacement"
        );
    }

    use crate::publish::{RemoteNodeId, RemoteNodeKind};

    fn remote_album(key: &str, name: &str) -> RemoteNode {
        RemoteNode {
            id: RemoteNodeId(format!("/api/v2/node/{key}")),
            container: Some(album_uri(key)),
            kind: RemoteNodeKind::Album,
            name: name.into(),
            web_url: Some(format!("https://example.smugmug.com/{name}")),
            has_children: false,
        }
    }

    #[test]
    fn linking_records_the_remote_album_name_and_address() {
        let mut state = PublishState::empty("smugmug");

        state
            .link_album("iceland", &remote_album("Ice", "Iceland 2026"))
            .unwrap();

        let link = state.link("iceland").unwrap();
        assert_eq!(link.remote_uri, "/api/v2/album/Ice");
        assert_eq!(link.remote_name.as_deref(), Some("Iceland 2026"));
        assert_eq!(
            link.web_url.as_deref(),
            Some("https://example.smugmug.com/Iceland 2026")
        );
        assert_eq!(link.last_published, None);
        assert!(link.images.is_empty());
    }

    #[test]
    fn a_remote_album_takes_one_link_only() {
        let mut state = published("iceland", &[("/p/a.raf", "/img/1", prints("e1", "s1"))]);
        let remote = remote_album("iceland", "Iceland");

        state
            .link_album("iceland", &remote)
            .expect("relinking an album to its own remote album is not a second link");
        assert!(
            state.image_for("iceland", "/p/a.raf").is_some(),
            "and keeps its records"
        );

        let refused = state.link_album("best-of", &remote).unwrap_err();
        assert_eq!(
            refused,
            LinkRefused::AlreadyLinked {
                album_id: "iceland".into()
            }
        );
        assert!(state.link("best-of").is_none(), "a refusal records nothing");
    }

    #[test]
    fn relinking_to_another_remote_album_drops_the_image_records() {
        let mut state = published("iceland", &[("/p/a.raf", "/img/1", prints("e1", "s1"))]);

        state
            .link_album("iceland", &remote_album("Other", "Other"))
            .unwrap();

        assert_eq!(
            state.link("iceland").unwrap().remote_uri,
            "/api/v2/album/Other"
        );
        assert_eq!(
            state.classify("iceland", "/p/a.raf", &prints("e1", "s1")),
            PublishAction::New,
            "the next publish uploads everything as new"
        );
        state
            .link_album("best-of", &remote_album("iceland", "Iceland"))
            .expect("the old remote album is free again");
    }

    #[test]
    fn a_folder_cannot_be_linked() {
        let mut state = PublishState::empty("smugmug");
        let folder = RemoteNode {
            container: None,
            kind: RemoteNodeKind::Folder,
            ..remote_album("Travel", "Travel")
        };

        assert!(state.link_album("iceland", &folder).is_err());
        assert!(state.link("iceland").is_none());
    }

    #[test]
    fn unlinking_removes_the_link_and_its_image_records() {
        let mut state = published("iceland", &[("/p/a.raf", "/img/1", prints("e1", "s1"))]);
        state.record_link("best-of", &album_uri("best-of"), None);

        assert!(state.unlink_album("iceland"));

        assert!(state.link("iceland").is_none());
        assert_eq!(
            state.classify("iceland", "/p/a.raf", &prints("e1", "s1")),
            PublishAction::New
        );
        assert!(state.link("best-of").is_some(), "other links stay");
        assert!(
            !state.unlink_album("iceland"),
            "unlinking twice is harmless"
        );
        assert_eq!(
            state.links().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["best-of"]
        );
    }

    #[test]
    fn marking_settings_current_turns_settings_changes_into_skips() {
        let mut state = published(
            "a",
            &[
                ("/p/1.raf", "/img/1", prints("e1", "s1")),
                ("/p/2.raf", "/img/2", prints("e2", "s1")),
            ],
        );
        state.record_link("b", &album_uri("b"), None);
        record(&mut state, "b", "/p/1.raf", "/img/3", &prints("e1", "s1"));

        assert_eq!(state.mark_settings_current(Some("a"), "s2"), 2);
        assert_eq!(
            state.classify("a", "/p/1.raf", &prints("e1", "s2")),
            PublishAction::Skip
        );
        assert!(
            matches!(
                state.classify("b", "/p/1.raf", &prints("e1", "s2")),
                PublishAction::SettingsChanged { .. }
            ),
            "only the named album is marked"
        );

        assert_eq!(state.mark_settings_current(None, "s2"), 1);
        assert_eq!(
            state.classify("b", "/p/1.raf", &prints("e1", "s2")),
            PublishAction::Skip
        );
        assert_eq!(
            state.mark_settings_current(None, "s2"),
            0,
            "already current"
        );
    }

    #[test]
    fn settings_impact_counts_photos_and_albums() {
        let mut state = published(
            "a",
            &[
                ("/p/1.raf", "/img/1", prints("e1", "old")),
                ("/p/2.raf", "/img/2", prints("e2", "old")),
            ],
        );
        state.record_link("b", &album_uri("b"), None);
        record(&mut state, "b", "/p/1.raf", "/img/3", &prints("e1", "old"));
        state.record_link("c", &album_uri("c"), None);
        record(&mut state, "c", "/p/4.raf", "/img/4", &prints("e4", "new"));

        assert_eq!(
            state.settings_impact("new"),
            SettingsImpact {
                photos: 3,
                albums: 2
            }
        );
        assert_eq!(
            state.settings_impact("old"),
            SettingsImpact {
                photos: 1,
                albums: 1
            }
        );
    }

    #[test]
    fn v1_state_migrates_into_links_by_album_membership() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");
        write_v1(
            &path,
            serde_json::json!({
                "/p/ice.raf": v1_image("/img/ice", "b3:ice"),
                "/p/shared.raf": v1_image("/img/shared", "b3:shared"),
                "/p/orphan.raf": v1_image("/img/orphan", "b3:orphan"),
            }),
        );
        let albums = membership(&[
            ("iceland", &["/p/ice.raf", "/p/shared.raf"]),
            ("best", &["/p/shared.raf"]),
            ("never-published", &["/p/orphan.raf"]),
        ]);

        let state = PublishState::load_from(&path, "smugmug", &albums).unwrap();

        assert_eq!(state.account(), Some("markallison"));
        let iceland = state.link("iceland").unwrap();
        assert_eq!(iceland.remote_uri, "/api/v2/album/Ice");
        assert_eq!(iceland.remote_name, None);
        assert_eq!(
            iceland.web_url.as_deref(),
            Some("https://example.smugmug.com/Iceland")
        );
        assert_eq!(iceland.linked_at, "2026-09-01T10:00:00+00:00");
        assert_eq!(
            iceland.last_published.as_deref(),
            Some("2026-09-01T10:00:00+00:00")
        );
        assert!(!iceland.broken);
        assert_eq!(
            iceland.images.keys().collect::<Vec<_>>(),
            ["/p/ice.raf", "/p/shared.raf"]
        );
        assert_eq!(
            state
                .link("best")
                .unwrap()
                .images
                .keys()
                .collect::<Vec<_>>(),
            ["/p/shared.raf"],
            "a photo in two linked albums lands in both"
        );
        assert!(state.link("never-published").is_none());
        assert!(
            state
                .links
                .values()
                .all(|link| !link.images.contains_key("/p/orphan.raf")),
            "an image in no linked album is dropped"
        );

        let migrated = state.image_for("iceland", "/p/ice.raf").unwrap();
        assert_eq!(migrated.remote_uri, "/img/ice");
        assert_eq!(migrated.legacy_fingerprint.as_deref(), Some("b3:ice"));
        assert_eq!(migrated.edit_hash, None);
        assert_eq!(migrated.settings_hash, None);
        assert_eq!(migrated.last_published, "2026-09-01T10:00:00+00:00");
    }

    #[test]
    fn a_migrated_record_skips_when_its_v1_fingerprint_matches_and_then_upgrades() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");
        let adjustments = r#"{"exposure":0.5}"#;
        let unchanged = fingerprints(a_time(), 1024, adjustments, &relevant());
        let edited = fingerprints(a_time(), 1024, r#"{"exposure":0.9}"#, &relevant());
        let v1_value = fingerprint(a_time(), 1024, adjustments, &relevant());
        write_v1(
            &path,
            serde_json::json!({
                "/p/same.raf": v1_image("/img/same", &v1_value),
                "/p/edited.raf": v1_image("/img/edited", &v1_value),
            }),
        );
        let albums = membership(&[("iceland", &["/p/same.raf", "/p/edited.raf"])]);
        let mut state = PublishState::load_from(&path, "smugmug", &albums).unwrap();

        assert_eq!(
            state.classify("iceland", "/p/same.raf", &unchanged),
            PublishAction::Skip,
            "migration must not re-upload an unchanged photo"
        );
        assert_eq!(
            state.classify("iceland", "/p/edited.raf", &edited),
            update("/img/edited")
        );

        assert!(state.confirm_unchanged("iceland", "/p/same.raf", &unchanged));
        assert!(
            !state.confirm_unchanged("iceland", "/p/same.raf", &unchanged),
            "an upgraded record needs no further save"
        );
        state.save_to(&path).unwrap();
        assert_eq!(
            read_json(&path)["version"],
            2,
            "written as v2 by the next save"
        );

        let reloaded = PublishState::load_from(&path, "smugmug", &albums).unwrap();
        let upgraded = reloaded.image_for("iceland", "/p/same.raf").unwrap();
        assert_eq!(upgraded.edit_hash.as_ref(), Some(&unchanged.edit_hash));
        assert_eq!(
            upgraded.settings_hash.as_ref(),
            Some(&unchanged.settings_hash)
        );
        assert_eq!(upgraded.legacy_fingerprint, None);
        assert_eq!(
            reloaded.classify("iceland", "/p/same.raf", &unchanged),
            PublishAction::Skip
        );
    }

    #[test]
    fn migrated_records_are_left_out_of_settings_bookkeeping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");
        write_v1(
            &path,
            serde_json::json!({ "/p/a.raf": v1_image("/img/a", "b3:a") }),
        );
        let albums = membership(&[("iceland", &["/p/a.raf"])]);
        let mut state = PublishState::load_from(&path, "smugmug", &albums).unwrap();

        assert_eq!(state.settings_impact("anything"), SettingsImpact::default());
        assert_eq!(state.mark_settings_current(None, "anything"), 0);
    }

    #[test]
    fn the_account_is_read_without_migrating_anything() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");
        assert_eq!(
            PublishState::account_in(dir.path(), "smugmug").unwrap(),
            None
        );

        write_v1(
            &path,
            serde_json::json!({ "/p/a.raf": v1_image("/img/a", "b3:a") }),
        );
        let before = std::fs::read(&path).unwrap();

        assert_eq!(
            PublishState::account_in(dir.path(), "smugmug")
                .unwrap()
                .as_deref(),
            Some("markallison")
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn fingerprints_are_stable_across_runs_and_map_ordering() {
        let ordered = r#"{"exposure":0.5,"contrast":-0.2,"masks":[{"id":"m1","opacity":1.0}]}"#;
        let shuffled = r#"{"masks":[{"opacity":1.0,"id":"m1"}],"contrast":-0.2,"exposure":0.5}"#;

        let first = fingerprints(a_time(), 1024, ordered, &relevant());
        let again = fingerprints(a_time(), 1024, ordered, &relevant());
        let reordered = fingerprints(a_time(), 1024, shuffled, &relevant());

        assert_eq!(first, again, "the same inputs must hash the same twice");
        assert_eq!(reordered, first, "JSON key order must not affect the hash");
        for hash in [&first.edit_hash, &first.settings_hash, &first.legacy] {
            assert!(hash.starts_with("b3:"), "unprefixed hash: {hash}");
        }
        assert_eq!(first.settings_hash, settings_hash(&relevant()));
    }

    #[test]
    fn the_v1_fingerprint_is_unchanged() {
        let settings = RelevantExportSettings {
            output_format: "jpeg".into(),
            jpeg_quality: 90,
            resize: None,
            keep_metadata: true,
            strip_gps: false,
            watermark: None,
            export_masks: false,
        };
        // What phase 1's `fingerprint` returned for these inputs.
        let phase_1 = "b3:d351835ee8cf204b99542f968affb7e24ea87cc2af4a9b3737b4dde0254d49a6";

        assert_eq!(
            fingerprint(a_time(), 1024, r#"{"exposure":0.5}"#, &settings),
            phase_1,
            "migrated records are compared with the value phase 1 wrote"
        );
        assert_eq!(
            fingerprints(a_time(), 1024, r#"{"exposure":0.5}"#, &settings).legacy,
            phase_1
        );
    }

    #[test]
    fn fingerprints_ignore_destination_and_filename_template() {
        let here = export_settings();
        let mut elsewhere = export_settings();
        elsewhere.destination_type = Some("originalFolder".into());
        elsewhere.subfolder = Some("somewhere/else".into());
        elsewhere.filename_template = Some("{sequence}_{original_filename}".into());
        elsewhere.preserve_folders = true;

        let adjustments = r#"{"exposure":0.5}"#;
        let a = fingerprints(
            a_time(),
            1024,
            adjustments,
            &RelevantExportSettings::from_export_settings(&here, "jpeg"),
        );
        let b = fingerprints(
            a_time(),
            1024,
            adjustments,
            &RelevantExportSettings::from_export_settings(&elsewhere, "jpeg"),
        );

        assert_eq!(a, b, "where the file lands does not change its pixels");
    }

    #[test]
    fn an_edit_changes_the_edit_hash_but_not_the_settings_hash() {
        let base = fingerprints(a_time(), 1024, r#"{"exposure":0.5}"#, &relevant());
        let later = a_time() + std::time::Duration::from_secs(1);

        for (changed, what) in [
            (
                fingerprints(a_time(), 1024, r#"{"exposure":0.6}"#, &relevant()),
                "an adjustment",
            ),
            (
                fingerprints(later, 1024, r#"{"exposure":0.5}"#, &relevant()),
                "a re-edited source file",
            ),
            (
                fingerprints(a_time(), 2048, r#"{"exposure":0.5}"#, &relevant()),
                "a different source size",
            ),
        ] {
            assert_ne!(
                base.edit_hash, changed.edit_hash,
                "{what} must not be skipped"
            );
            assert_ne!(base.legacy, changed.legacy, "{what}");
            assert_eq!(
                base.settings_hash, changed.settings_hash,
                "{what} is not a settings change"
            );
        }
    }

    #[test]
    fn view_only_adjustments_do_not_change_the_edit_hash() {
        let edit = |show_clipping: bool, loading: bool| {
            let adjustments = format!(
                r#"{{"exposure":0.5,"showClipping":{show_clipping},"aiPatches":[{{"id":"p1","visible":true,"isLoading":{loading}}}]}}"#
            );
            fingerprints(a_time(), 1024, &adjustments, &relevant()).edit_hash
        };
        let base = edit(false, false);

        assert_eq!(
            edit(true, false),
            base,
            "the clipping overlay is not an edit"
        );
        assert_eq!(
            edit(false, true),
            base,
            "a patch that is generating is not an edit"
        );
    }

    #[test]
    fn rendered_toggles_still_change_the_edit_hash() {
        let edit =
            |adjustments: &str| fingerprints(a_time(), 1024, adjustments, &relevant()).edit_hash;
        let base = edit(
            r#"{"exposure":0.5,"sectionVisibility":{"basic":true},"masks":[{"id":"m1","visible":true}]}"#,
        );

        for (changed, what) in [
            (
                r#"{"exposure":0.6,"sectionVisibility":{"basic":true},"masks":[{"id":"m1","visible":true}]}"#,
                "an adjustment",
            ),
            (
                r#"{"exposure":0.5,"sectionVisibility":{"basic":false},"masks":[{"id":"m1","visible":true}]}"#,
                "a disabled section",
            ),
            (
                r#"{"exposure":0.5,"sectionVisibility":{"basic":true},"masks":[{"id":"m1","visible":false}]}"#,
                "a hidden mask",
            ),
        ] {
            assert_ne!(edit(changed), base, "{what} changes the render");
        }
    }

    #[test]
    fn a_settings_change_changes_the_settings_hash_but_not_the_edit_hash() {
        let adjustments = r#"{"exposure":0.5}"#;
        let base = fingerprints(a_time(), 1024, adjustments, &relevant());
        let mut smaller = export_settings();
        smaller.jpeg_quality = 60;

        for (settings, what) in [
            (
                RelevantExportSettings::from_export_settings(&smaller, "jpeg"),
                "a quality change",
            ),
            (
                RelevantExportSettings::from_export_settings(&export_settings(), "png"),
                "a format change",
            ),
        ] {
            let changed = fingerprints(a_time(), 1024, adjustments, &settings);
            assert_ne!(base.settings_hash, changed.settings_hash, "{what}");
            assert_ne!(base.legacy, changed.legacy, "{what}");
            assert_eq!(base.edit_hash, changed.edit_hash, "{what} is not an edit");
        }
    }

    #[test]
    fn an_interrupted_write_leaves_the_previous_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");

        let good = published("a", &[("/p/a.raf", "/img/1", prints("e1", "s1"))]);
        good.save_to(&path).unwrap();

        // A kill between the write and the rename: a half-written temp file
        // next to an untouched target.
        std::fs::write(path.with_extension("json.tmp"), br#"{"version":2,"desti"#).unwrap();

        let loaded =
            PublishState::load_from(&path, "smugmug", &AlbumMembership::default()).unwrap();

        assert_eq!(loaded, good, "the previous good state was lost");
    }

    #[test]
    fn an_unlinked_state_takes_the_connected_account() {
        let mut state = PublishState::empty("smugmug");
        state.claim_account("alice").unwrap();
        assert_eq!(state.account(), Some("alice"));

        state
            .claim_account("bob")
            .expect("nothing is linked, so nothing belongs to alice yet");
        assert_eq!(state.account(), Some("bob"));
    }

    #[test]
    fn links_belonging_to_another_account_are_refused() {
        let mut state = published("iceland", &[]);
        state.claim_account("alice").unwrap();

        state
            .claim_account("alice")
            .expect("the same account publishes on");
        let error = state.claim_account("bob").unwrap_err();

        assert!(matches!(error, PublishError::NotAuthorised(_)), "{error}");
        let message = error.to_string();
        assert!(
            message.contains("alice") && message.contains("bob"),
            "{message}"
        );
        assert_eq!(state.account(), Some("alice"), "a refusal records nothing");
    }

    #[test]
    fn links_with_no_recorded_account_are_claimed() {
        let mut state = published("iceland", &[]);
        state.claim_account("alice").unwrap();
        assert_eq!(state.account(), Some("alice"));
    }

    #[test]
    fn unknown_future_version_is_rejected_not_silently_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");
        std::fs::write(
            &path,
            br#"{"version":999,"destination":"smugmug","account":null,"links":{}}"#,
        )
        .unwrap();

        let loaded = PublishState::load_from(&path, "smugmug", &AlbumMembership::default());

        assert!(
            loaded.is_err(),
            "an unreadable version must not reset to empty: that republishes the whole library"
        );
        assert!(
            PublishState::account_in(dir.path(), "smugmug").is_err(),
            "nor may its account be trusted"
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_state_not_an_error() {
        let dir = tempfile::tempdir().unwrap();

        let loaded = PublishState::load_from(
            &dir.path().join("smugmug.json"),
            "smugmug",
            &AlbumMembership::default(),
        )
        .unwrap();

        assert_eq!(loaded, PublishState::empty("smugmug"));
    }
}
