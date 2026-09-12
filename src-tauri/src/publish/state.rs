//! Publish state: what has already been published where, and whether it has
//! changed since.
//!
//! Two jobs. The remote-ID map is what makes a republish *replace* rather than
//! duplicate — without it every run would add a second copy of every photo.
//! The fingerprint is what makes an unchanged album cost nothing: it is
//! compared before rendering, so a skip costs one `stat` and no GPU time.
//!
//! Lives in `app_data_dir/publish/<destination_id>.json`, mirroring
//! `albums/albums.json` (`file_management.rs:838-847`) — unlike the spool this
//! is not regenerable, and losing it means re-uploading the user's library.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::export_processing::{ExportSettings, ResizeOptions, WatermarkSettings};
use crate::publish::{PublishError, RemoteContainerId, RemoteImageId};

/// Subdirectory of `app_data_dir` holding one file per destination.
const STATE_DIR_NAME: &str = "publish";

/// Bumped only for a format change that an older build could misread. An
/// unrecognised value is an error, never a reset — see [`PublishState::load_from`].
const STATE_VERSION: u32 = 1;

/// Where a container (album, folder, set) ended up at the destination.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContainerRecord {
    pub remote_uri: String,
    pub web_url: Option<String>,
    /// RFC 3339. A string rather than a `DateTime<Utc>` because chrono's
    /// `serde` feature is not enabled in this tree, as in `spool.rs`.
    pub last_published: String,
}

/// Where one publishable photo ended up, and what it looked like at the time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageRecord {
    pub remote_uri: String,
    pub web_url: Option<String>,
    /// The [`fingerprint`] as of the last successful upload.
    pub fingerprint: String,
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
    containers: BTreeMap<String, ContainerRecord>,
    images: BTreeMap<String, ImageRecord>,
}

/// What a republish would do to one photo, and what the panel previews.
#[derive(Debug, PartialEq, Eq)]
pub enum PublishAction {
    New,
    Update { replaces: RemoteImageId },
    Skip,
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

impl PublishState {
    pub fn empty(destination_id: &str) -> Self {
        Self {
            version: STATE_VERSION,
            destination: destination_id.to_string(),
            account: None,
            containers: BTreeMap::new(),
            images: BTreeMap::new(),
        }
    }

    /// Loads `<dir>/<destination_id>.json`, where `dir` is what [`state_dir`]
    /// resolves to — or a temp directory under test.
    pub fn load_in(dir: &Path, destination_id: &str) -> Result<Self, PublishError> {
        Self::load_from(&state_file(dir, destination_id)?, destination_id)
    }

    /// [`Self::load_in`] with the file named directly, so persistence is testable
    /// against a plain temp directory.
    ///
    /// A missing file is an empty state — nothing has been published yet. A
    /// file that is present but unreadable is an error: silently starting from
    /// empty would re-upload the entire library as duplicates.
    pub fn load_from(path: &Path, destination_id: &str) -> Result<Self, PublishError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty(destination_id));
            }
            Err(e) => {
                return Err(PublishError::Io(format!(
                    "reading publish state {}: {e}",
                    path.display()
                )));
            }
        };

        // Read the version before the body: a future format may not fit this
        // struct at all, and the point is to say so rather than to guess.
        let version = serde_json::from_slice::<VersionProbe>(&bytes)
            .map(|probe| probe.version)
            .map_err(|e| {
                PublishError::Io(format!(
                    "publish state {} is not readable: {e}",
                    path.display()
                ))
            })?;
        if version != STATE_VERSION {
            return Err(PublishError::Io(format!(
                "publish state {} has version {version}, expected {STATE_VERSION}: \
                 it was written by a newer build of RapidRAW",
                path.display()
            )));
        }

        serde_json::from_slice(&bytes).map_err(|e| {
            PublishError::Io(format!(
                "publish state {} is not readable: {e}",
                path.display()
            ))
        })
    }

    pub fn save_in(&self, dir: &Path) -> Result<(), PublishError> {
        self.save_to(&state_file(dir, &self.destination)?)
    }

    /// Atomic: writes `<path>.tmp`, flushes it to disk, then renames over the
    /// target. A kill at any point leaves either the previous state or the new
    /// one, never a truncated file — and a stranded `.tmp` is ignored by
    /// [`Self::load_from`] and overwritten by the next save.
    pub fn save_to(&self, path: &Path) -> Result<(), PublishError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| PublishError::Io(format!("creating {}: {e}", parent.display())))?;
        }

        let encoded = serde_json::to_vec_pretty(self)
            .map_err(|e| PublishError::Io(format!("encoding publish state: {e}")))?;

        let tmp = path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)
                .map_err(|e| PublishError::Io(format!("creating {}: {e}", tmp.display())))?;
            file.write_all(&encoded)
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

    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    pub fn set_account(&mut self, account: Option<String>) {
        self.account = account;
    }

    pub fn record_container(
        &mut self,
        album_id: &str,
        id: &RemoteContainerId,
        web_url: Option<String>,
    ) {
        self.containers.insert(
            album_id.to_string(),
            ContainerRecord {
                remote_uri: id.0.clone(),
                web_url,
                last_published: now_rfc3339(),
            },
        );
    }

    #[cfg(test)]
    pub fn image_for(&self, virtual_path: &str) -> Option<&ImageRecord> {
        self.images.get(virtual_path)
    }

    /// Keyed on the **full virtual path**, never the source path: virtual
    /// copies are distinct publishable photos, distinguished by a `vc=`
    /// suffix. Keying on the source would collapse every copy of an image onto
    /// one remote image, each republish overwriting the last.
    pub fn record_image(
        &mut self,
        virtual_path: &str,
        id: &RemoteImageId,
        fingerprint: &str,
        web_url: Option<String>,
    ) {
        self.images.insert(
            virtual_path.to_string(),
            ImageRecord {
                remote_uri: id.0.clone(),
                web_url,
                fingerprint: fingerprint.to_string(),
                last_published: now_rfc3339(),
            },
        );
    }

    /// New / Update / Skip — drives the panel's preview counts, and is checked
    /// before rendering so a `Skip` costs no GPU time.
    pub fn classify(&self, virtual_path: &str, fingerprint: &str) -> PublishAction {
        match self.images.get(virtual_path) {
            None => PublishAction::New,
            Some(record) if record.fingerprint == fingerprint => PublishAction::Skip,
            Some(record) => PublishAction::Update {
                replaces: RemoteImageId(record.remote_uri.clone()),
            },
        }
    }
}

/// Just enough of the file to decide whether the rest can be trusted.
#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

/// What has to change about a photo before it is worth uploading again —
/// Lightroom's `metadataThatTriggersRepublish`.
///
/// Fields are length-prefixed rather than concatenated, so no rearrangement of
/// one field's contents can imitate another's ("ab" + "c" and "a" + "bc" hash
/// differently).
pub fn fingerprint(
    source_mtime: SystemTime,
    source_size: u64,
    adjustments_json: &str,
    export_settings: &RelevantExportSettings,
) -> String {
    let mut hasher = blake3::Hasher::new();

    // Signed nanoseconds from the epoch, so a pre-1970 mtime stays distinct
    // rather than saturating to zero with every other pre-epoch timestamp.
    let mtime_nanos = match source_mtime.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => since.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    hasher.update(&mtime_nanos.to_le_bytes());
    hasher.update(&source_size.to_le_bytes());

    let adjustments = canonical_json_str(adjustments_json);
    update_field(&mut hasher, adjustments.as_bytes());

    // Through `to_value` and back so the settings hash the same way the
    // adjustments do: sorted keys, no dependence on field declaration order.
    let settings = serde_json::to_value(export_settings)
        .map(|value| canonical_json(&value))
        .unwrap_or_default();
    update_field(&mut hasher, settings.as_bytes());

    format!("b3:{}", hasher.finalize().to_hex())
}

fn update_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Canonicalises a JSON document, falling back to the raw text when it is not
/// JSON at all — an unparseable adjustments blob should still produce a stable
/// fingerprint rather than collapsing every such image onto one hash.
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
    Ok(dir.join(format!("{destination_id}.json")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export_processing::{ResizeMode, WatermarkAnchor};

    /// A baseline `ExportSettings` for the fingerprint tests to vary one field
    /// of at a time.
    fn export_settings() -> ExportSettings {
        ExportSettings {
            jpeg_quality: 90,
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

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");

        let mut state = PublishState::empty("smugmug");
        state.set_account(Some("markallison".into()));
        state.record_container(
            "album-1",
            &RemoteContainerId("/api/v2/album/AbCdEf".into()),
            Some("https://example.smugmug.com/Album".into()),
        );
        state.record_image(
            "/photos/a.raf",
            &RemoteImageId("/api/v2/image/XyZ123-0".into()),
            "b3:aaa",
            None,
        );
        state.save_to(&path).unwrap();

        let loaded = PublishState::load_from(&path, "smugmug").unwrap();

        assert_eq!(loaded, state);
    }

    #[test]
    fn classify_returns_new_update_skip() {
        let mut state = PublishState::empty("smugmug");
        assert_eq!(state.classify("/p/a.raf", "b3:aaa"), PublishAction::New);

        state.record_image(
            "/p/a.raf",
            &RemoteImageId("/api/v2/image/X".into()),
            "b3:aaa",
            None,
        );
        assert_eq!(state.classify("/p/a.raf", "b3:aaa"), PublishAction::Skip);
        assert_eq!(
            state.classify("/p/a.raf", "b3:bbb"),
            PublishAction::Update {
                replaces: RemoteImageId("/api/v2/image/X".into())
            }
        );
    }

    #[test]
    fn virtual_copies_are_distinct_entries() {
        let mut state = PublishState::empty("smugmug");
        state.record_image("/p/a.raf", &RemoteImageId("/img/1".into()), "b3:aaa", None);
        state.record_image(
            "/p/a.raf?vc=1",
            &RemoteImageId("/img/2".into()),
            "b3:bbb",
            None,
        );

        assert_eq!(state.image_for("/p/a.raf").unwrap().remote_uri, "/img/1");
        assert_eq!(
            state.image_for("/p/a.raf?vc=1").unwrap().remote_uri,
            "/img/2"
        );
    }

    #[test]
    fn fingerprint_is_stable_across_runs_and_map_ordering() {
        let ordered = r#"{"exposure":0.5,"contrast":-0.2,"masks":[{"id":"m1","opacity":1.0}]}"#;
        let shuffled = r#"{"masks":[{"opacity":1.0,"id":"m1"}],"contrast":-0.2,"exposure":0.5}"#;

        let first = fingerprint(a_time(), 1024, ordered, &relevant());
        let again = fingerprint(a_time(), 1024, ordered, &relevant());
        let reordered = fingerprint(a_time(), 1024, shuffled, &relevant());

        assert_eq!(first, again, "the same inputs must hash the same twice");
        assert_eq!(reordered, first, "JSON key order must not affect the hash");
        assert!(first.starts_with("b3:"), "unprefixed fingerprint: {first}");
    }

    #[test]
    fn fingerprint_ignores_destination_and_filename_template() {
        let here = export_settings();
        let mut elsewhere = export_settings();
        elsewhere.destination_type = Some("originalFolder".into());
        elsewhere.subfolder = Some("somewhere/else".into());
        elsewhere.filename_template = Some("{sequence}_{original_filename}".into());
        elsewhere.preserve_folders = true;

        let adjustments = r#"{"exposure":0.5}"#;
        let a = fingerprint(
            a_time(),
            1024,
            adjustments,
            &RelevantExportSettings::from_export_settings(&here, "jpeg"),
        );
        let b = fingerprint(
            a_time(),
            1024,
            adjustments,
            &RelevantExportSettings::from_export_settings(&elsewhere, "jpeg"),
        );

        assert_eq!(a, b, "where the file lands does not change its pixels");
    }

    #[test]
    fn fingerprint_changes_when_adjustments_change() {
        let before = fingerprint(a_time(), 1024, r#"{"exposure":0.5}"#, &relevant());
        let after = fingerprint(a_time(), 1024, r#"{"exposure":0.6}"#, &relevant());

        assert_ne!(before, after);
    }

    #[test]
    fn fingerprint_changes_when_the_source_or_export_settings_change() {
        let base = fingerprint(a_time(), 1024, r#"{"exposure":0.5}"#, &relevant());

        let later = a_time() + std::time::Duration::from_secs(1);
        assert_ne!(
            base,
            fingerprint(later, 1024, r#"{"exposure":0.5}"#, &relevant()),
            "a re-edited source file must not be skipped"
        );
        assert_ne!(
            base,
            fingerprint(a_time(), 2048, r#"{"exposure":0.5}"#, &relevant()),
            "a different source size must not be skipped"
        );

        let mut smaller = export_settings();
        smaller.jpeg_quality = 60;
        assert_ne!(
            base,
            fingerprint(
                a_time(),
                1024,
                r#"{"exposure":0.5}"#,
                &RelevantExportSettings::from_export_settings(&smaller, "jpeg")
            ),
            "a quality change must not be skipped"
        );
        assert_ne!(
            base,
            fingerprint(
                a_time(),
                1024,
                r#"{"exposure":0.5}"#,
                &RelevantExportSettings::from_export_settings(&export_settings(), "png")
            ),
            "a format change must not be skipped"
        );
    }

    #[test]
    fn an_interrupted_write_leaves_the_previous_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");

        let mut good = PublishState::empty("smugmug");
        good.record_image("/p/a.raf", &RemoteImageId("/img/1".into()), "b3:aaa", None);
        good.save_to(&path).unwrap();

        // A kill between the write and the rename: a half-written temp file
        // next to an untouched target.
        std::fs::write(path.with_extension("json.tmp"), br#"{"version":1,"desti"#).unwrap();

        let loaded = PublishState::load_from(&path, "smugmug").unwrap();

        assert_eq!(loaded, good, "the previous good state was lost");
    }

    #[test]
    fn unknown_future_version_is_rejected_not_silently_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smugmug.json");
        std::fs::write(
            &path,
            br#"{"version":999,"destination":"smugmug","account":null,"containers":{},"images":{}}"#,
        )
        .unwrap();

        let loaded = PublishState::load_from(&path, "smugmug");

        assert!(
            loaded.is_err(),
            "an unreadable version must not reset to empty: that republishes the whole library"
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_state_not_an_error() {
        let dir = tempfile::tempdir().unwrap();

        let loaded = PublishState::load_from(&dir.path().join("smugmug.json"), "smugmug").unwrap();

        assert_eq!(loaded, PublishState::empty("smugmug"));
    }
}
