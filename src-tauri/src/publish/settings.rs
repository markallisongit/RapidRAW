//! Per-destination settings: the output preset and the privacy of new albums.
//!
//! Lives in `app_data_dir/publish/<destination_id>.settings.json`, beside the
//! state file but separate from it on purpose: settings are a few bytes the
//! user edits, state is large and machine-written, and neither should have to
//! rewrite the other. Not in `AppSettings` either — only publishing reads
//! these, and that struct and its frontend mirror are busy upstream files.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::publish::state::{destination_file, write_atomically};
use crate::publish::{ContainerPrivacy, PublishError};

/// Bumped only for a format change an older build could misread.
const SETTINGS_VERSION: u32 = 1;

/// What the Publish Manager edits for one destination.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DestinationSettings {
    /// An id from `AppSettings.export_presets`. `None` until one is chosen.
    pub export_preset_id: Option<String>,
    /// Applied only when an album is created, never to one found or linked.
    pub new_album_privacy: ContainerPrivacy,
}

impl DestinationSettings {
    /// A missing file is the defaults. A file that is present but unreadable
    /// is an error, like the state file: quietly falling back to Public would
    /// publish more widely than the user chose.
    pub fn load_in(dir: &Path, destination_id: &str) -> Result<Self, PublishError> {
        let path = settings_file(dir, destination_id)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(PublishError::Io(format!(
                    "reading publish settings {}: {e}",
                    path.display()
                )));
            }
        };
        let file: SettingsFile = serde_json::from_slice(&bytes).map_err(|e| {
            PublishError::Io(format!(
                "publish settings {} are not readable: {e}",
                path.display()
            ))
        })?;
        if file.version != SETTINGS_VERSION {
            return Err(PublishError::Io(format!(
                "publish settings {} have version {}, expected {SETTINGS_VERSION}: \
                 they were written by a newer build of RapidRAW",
                path.display(),
                file.version
            )));
        }
        Ok(file.settings)
    }

    /// Atomic, like the state file.
    pub fn save_in(&self, dir: &Path, destination_id: &str) -> Result<(), PublishError> {
        let path = settings_file(dir, destination_id)?;
        let encoded = serde_json::to_vec_pretty(&SettingsFile {
            version: SETTINGS_VERSION,
            settings: self.clone(),
        })
        .map_err(|e| PublishError::Io(format!("encoding publish settings: {e}")))?;
        write_atomically(&path, &encoded)
    }
}

/// The file adds a version the commands never expose.
#[derive(Serialize, Deserialize)]
struct SettingsFile {
    version: u32,
    #[serde(flatten)]
    settings: DestinationSettings,
}

fn settings_file(dir: &Path, destination_id: &str) -> Result<PathBuf, PublishError> {
    destination_file(dir, destination_id, "settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let settings = DestinationSettings::load_in(dir.path(), "smugmug").unwrap();
        assert_eq!(settings.export_preset_id, None);
        assert_eq!(settings.new_album_privacy, ContainerPrivacy::Public);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let settings = DestinationSettings {
            export_preset_id: Some("preset-1".into()),
            new_album_privacy: ContainerPrivacy::Unlisted,
        };
        settings.save_in(dir.path(), "smugmug").unwrap();

        assert_eq!(
            DestinationSettings::load_in(dir.path(), "smugmug").unwrap(),
            settings
        );
        let written = read_json(&dir.path().join("smugmug.settings.json"));
        assert_eq!(
            written,
            serde_json::json!({
                "version": 1,
                "export_preset_id": "preset-1",
                "new_album_privacy": "Unlisted"
            })
        );
    }

    #[test]
    fn saving_leaves_no_temporary_file_and_never_touches_the_state_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("smugmug.json"), b"state").unwrap();

        DestinationSettings::default()
            .save_in(dir.path(), "smugmug")
            .unwrap();
        DestinationSettings::default()
            .save_in(dir.path(), "smugmug")
            .unwrap();

        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["smugmug.json", "smugmug.settings.json"]);
        assert_eq!(
            std::fs::read(dir.path().join("smugmug.json")).unwrap(),
            b"state"
        );
    }

    #[test]
    fn an_unreadable_file_is_an_error_not_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("smugmug.settings.json"), b"{ not json").unwrap();
        assert!(DestinationSettings::load_in(dir.path(), "smugmug").is_err());
    }

    #[test]
    fn a_newer_version_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("smugmug.settings.json"),
            br#"{"version":2,"export_preset_id":null,"new_album_privacy":"Public"}"#,
        )
        .unwrap();
        assert!(DestinationSettings::load_in(dir.path(), "smugmug").is_err());
    }

    #[test]
    fn an_unsafe_destination_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(DestinationSettings::load_in(dir.path(), "../x").is_err());
        assert!(
            DestinationSettings::default()
                .save_in(dir.path(), "../x")
                .is_err()
        );
    }
}
