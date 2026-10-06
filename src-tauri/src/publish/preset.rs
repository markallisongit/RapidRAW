//! The output a destination publishes with: its linked export preset, turned
//! into the `ExportSettings` and output format the export pipeline takes.
//!
//! Resolved here rather than taken from the frontend, so publishing renders
//! what the destination is configured for and not whatever the Export panel
//! happens to be set to.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::app_settings::ExportPreset;
use crate::export_processing::{
    BorderOptions, ExportSettings, PadOptions, ResizeOptions, TiffBitDepth, WatermarkSettings,
};
use crate::publish::PublishError;
use crate::publish::state::{PublishState, RelevantExportSettings, SettingsImpact, settings_hash};

/// What a destination publishes with.
#[derive(Debug, Clone)]
pub struct PublishOutput {
    pub export_settings: ExportSettings,
    /// A file extension, as the export pipeline takes it: `jpg`, not `jpeg`.
    pub output_format: String,
}

impl PublishOutput {
    /// What every photo published with this output records as its
    /// [`Fingerprints::settings_hash`](crate::publish::state::Fingerprints::settings_hash).
    pub fn settings_hash(&self) -> String {
        settings_hash(&RelevantExportSettings::from_export_settings(
            &self.export_settings,
            &self.output_format,
        ))
    }
}

/// Why a command that needs the destination's output could not run, tagged
/// so the panel can offer the way out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum PresetError {
    /// No preset is chosen, or the chosen one no longer exists. The panel
    /// asks the user to choose one.
    PresetMissing {
        preset_id: Option<String>,
    },
    Failed {
        message: String,
    },
}

impl From<String> for PresetError {
    fn from(message: String) -> Self {
        Self::Failed { message }
    }
}

impl From<PublishError> for PresetError {
    fn from(error: PublishError) -> Self {
        Self::Failed {
            message: error.to_string(),
        }
    }
}

/// The preset `preset_id` from `presets`, as a [`PublishOutput`].
pub fn resolve(preset_id: &str, presets: &[ExportPreset]) -> Result<PublishOutput, PresetError> {
    let preset = presets
        .iter()
        .find(|preset| preset.id == preset_id)
        .ok_or_else(|| PresetError::PresetMissing {
            preset_id: Some(preset_id.to_string()),
        })?;
    from_preset(preset)
}

/// The same `ExportSettings` and output format the Export panel's
/// `handleExport` builds from a preset's values, so photos published with the
/// Export panel set to a preset, before destinations had their own, hash
/// alike with that preset and do not republish.
///
/// A resize mode or watermark anchor the export pipeline would not accept is
/// an error, as the Export panel's own export of that preset would be.
pub fn from_preset(preset: &ExportPreset) -> Result<PublishOutput, PresetError> {
    let resize = if preset.enable_resize {
        Some(ResizeOptions {
            mode: parse(&preset.name, "resize mode", Some(&preset.resize_mode))?,
            value: preset.resize_value,
            dont_enlarge: preset.dont_enlarge,
        })
    } else {
        None
    };
    // Fallbacks are the Export panel's own, for a preset saved with the option
    // switched on but a value missing.
    let border = if preset.enable_border.unwrap_or(false) {
        Some(BorderOptions {
            basis: match &preset.border_basis {
                Some(basis) => parse(&preset.name, "border basis", Some(basis))?,
                None => Default::default(),
            },
            horizontal_percent: preset.border_horizontal_percent.unwrap_or(2.0),
            vertical_percent: preset.border_vertical_percent.unwrap_or(2.0),
            color: preset
                .border_color
                .clone()
                .unwrap_or_else(|| "#ffffff".into()),
        })
    } else {
        None
    };
    let pad = if preset.enable_pad.unwrap_or(false) {
        Some(PadOptions {
            ratio_width: preset.pad_ratio_width.unwrap_or(1.0),
            ratio_height: preset.pad_ratio_height.unwrap_or(1.0),
            color: preset.pad_color.clone().unwrap_or_else(|| "#ffffff".into()),
        })
    } else {
        None
    };
    let watermark = match &preset.watermark_path {
        Some(path) if preset.enable_watermark && !path.is_empty() => Some(WatermarkSettings {
            path: path.clone(),
            anchor: parse(
                &preset.name,
                "watermark anchor",
                preset.watermark_anchor.as_deref(),
            )?,
            scale: preset.watermark_scale as f32,
            spacing: preset.watermark_spacing as f32,
            opacity: preset.watermark_opacity as f32,
        }),
        _ => None,
    };

    Ok(PublishOutput {
        export_settings: ExportSettings {
            jpeg_quality: preset.jpeg_quality,
            // The frontend stores it on a preset, but `ExportPreset` here has no
            // such field, so this is the 16 the frontend falls back to.
            tiff_bit_depth: TiffBitDepth::default(),
            resize,
            border,
            pad,
            keep_metadata: preset.keep_metadata,
            // Not stored in a preset. Sets the local file's mtime, which
            // nothing about an upload reads.
            preserve_timestamps: false,
            strip_gps: preset.strip_gps,
            filename_template: Some(preset.filename_template.clone()),
            watermark,
            export_masks: preset.export_masks.unwrap_or(false),
            preserve_folders: preset.preserve_folders.unwrap_or(false),
            destination_type: Some(
                preset
                    .destination_type
                    .clone()
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| DEFAULT_DESTINATION_TYPE.to_string()),
            ),
            subfolder: Some(preset.subfolder.clone().unwrap_or_default()),
        },
        output_format: output_format(&preset.file_format).to_string(),
    })
}

/// The output to publish with: the destination's preset. With none chosen
/// there is nothing to publish with, and nothing falls back to the Export
/// panel's settings.
pub fn destination_output(
    preset_id: Option<&str>,
    presets: &[ExportPreset],
) -> Result<PublishOutput, PresetError> {
    resolve(
        preset_id.ok_or(PresetError::PresetMissing { preset_id: None })?,
        presets,
    )
}

/// How many published photos switching the destination to `preset_id`
/// would upload again, compared by settings hash alone.
pub fn settings_impact(
    state: &PublishState,
    preset_id: &str,
    presets: &[ExportPreset],
) -> Result<SettingsImpact, PresetError> {
    Ok(state.settings_impact(&resolve(preset_id, presets)?.settings_hash()))
}

/// Records every link's photos as current with the destination's preset,
/// for "Keep existing uploads" after switching to it. Returns how many
/// records changed.
pub fn keep_existing_uploads(
    state: &mut PublishState,
    preset_id: Option<&str>,
    presets: &[ExportPreset],
) -> Result<usize, PresetError> {
    let preset_id = preset_id.ok_or(PresetError::PresetMissing { preset_id: None })?;
    let settings_hash = resolve(preset_id, presets)?.settings_hash();
    Ok(state.mark_settings_current(None, &settings_hash))
}

/// What the Export panel falls back to for a preset saved without one.
const DEFAULT_DESTINATION_TYPE: &str = "customFolder";

/// The first extension of the format with this id in the frontend's
/// `FILE_FORMATS`, which falls back to its first entry, JPEG.
fn output_format(file_format: &str) -> &'static str {
    match file_format {
        "png" => "png",
        "tiff" => "tiff",
        "webp" => "webp",
        "jxl" => "jxl",
        "avif" => "avif",
        "cube" => "cube",
        _ => "jpg",
    }
}

/// A preset's string for one of the export pipeline's camelCase enums.
fn parse<T: DeserializeOwned>(
    preset_name: &str,
    what: &str,
    value: Option<&str>,
) -> Result<T, PresetError> {
    value
        .and_then(|value| serde_json::from_value(Value::String(value.to_string())).ok())
        .ok_or_else(|| PresetError::Failed {
            message: format!(
                "the export preset \"{preset_name}\" has an unknown {what}: {}",
                value.unwrap_or("none")
            ),
        })
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use serde_json::{Value, json};

    use super::*;
    use crate::publish::state::{Fingerprints, PublishAction, fingerprints};
    use crate::publish::{RemoteContainerId, RemoteImageId};

    /// A preset as the frontend saves it, every optional field set.
    fn preset_json() -> Value {
        json!({
            "id": "smugmug-web",
            "name": "SmugMug",
            "fileFormat": "jpeg",
            "jpegQuality": 88,
            "enableResize": true,
            "resizeMode": "longEdge",
            "resizeValue": 3000,
            "dontEnlarge": false,
            "enablePad": true,
            "padRatioWidth": 4.0,
            "padRatioHeight": 5.0,
            "padColor": "#000000",
            "enableBorder": true,
            "borderBasis": "shortEdge",
            "borderHorizontalPercent": 3.0,
            "borderVerticalPercent": 5.0,
            "borderColor": "#fafafa",
            "keepMetadata": true,
            "stripGps": true,
            "filenameTemplate": "{original_filename}_web",
            "enableWatermark": true,
            "watermarkPath": "/marks/sig.png",
            "watermarkAnchor": "bottomRight",
            "watermarkScale": 12,
            "watermarkSpacing": 4,
            "watermarkOpacity": 70,
            "exportMasks": true,
            "preserveFolders": true,
            "lastExportPath": "/exports",
            "destinationType": "originalFolder",
            "subfolder": "web"
        })
    }

    fn preset(json: Value) -> ExportPreset {
        serde_json::from_value(json).unwrap()
    }

    fn with(changes: Value) -> ExportPreset {
        let mut json = preset_json();
        for (key, value) in changes.as_object().unwrap() {
            json[key] = value.clone();
        }
        preset(json)
    }

    /// What the Export panel's `handleExport` sends for a preset, as the JSON
    /// the command receives.
    fn settings_json(output: &PublishOutput) -> Value {
        serde_json::to_value(&output.export_settings).unwrap()
    }

    #[test]
    fn a_preset_converts_as_the_frontend_converts_it() {
        let output = from_preset(&preset(preset_json())).unwrap();

        assert_eq!(output.output_format, "jpg", "the format's first extension");
        assert_eq!(
            settings_json(&output),
            json!({
                "jpegQuality": 88,
                "tiffBitDepth": 16,
                "resize": { "mode": "longEdge", "value": 3000, "dontEnlarge": false },
                "border": {
                    "basis": "shortEdge",
                    "horizontalPercent": 3.0,
                    "verticalPercent": 5.0,
                    "color": "#fafafa"
                },
                "pad": { "ratioWidth": 4.0, "ratioHeight": 5.0, "color": "#000000" },
                "keepMetadata": true,
                "preserveTimestamps": false,
                "stripGps": true,
                "filenameTemplate": "{original_filename}_web",
                "watermark": {
                    "path": "/marks/sig.png",
                    "anchor": "bottomRight",
                    "scale": 12.0,
                    "spacing": 4.0,
                    "opacity": 70.0
                },
                "exportMasks": true,
                "preserveFolders": true,
                "destinationType": "originalFolder",
                "subfolder": "web"
            })
        );
    }

    #[test]
    fn resize_border_pad_and_watermark_are_left_out_unless_enabled() {
        let output = from_preset(&with(json!({
            "enableResize": false,
            "enablePad": false,
            "enableBorder": false,
            "enableWatermark": false,
        })))
        .unwrap();
        let settings = settings_json(&output);
        assert_eq!(settings["resize"], Value::Null);
        assert_eq!(settings["border"], Value::Null);
        assert_eq!(settings["pad"], Value::Null);
        assert_eq!(settings["watermark"], Value::Null);

        let no_path = from_preset(&with(json!({ "watermarkPath": "" }))).unwrap();
        assert_eq!(
            settings_json(&no_path)["watermark"],
            Value::Null,
            "a watermark with no image is no watermark"
        );
        let null_path = from_preset(&with(json!({ "watermarkPath": null }))).unwrap();
        assert_eq!(settings_json(&null_path)["watermark"], Value::Null);
    }

    #[test]
    fn missing_optional_fields_take_the_export_panels_defaults() {
        let mut json = preset_json();
        for key in [
            "enablePad",
            "enableBorder",
            "exportMasks",
            "preserveFolders",
            "destinationType",
            "subfolder",
        ] {
            json.as_object_mut().unwrap().remove(key);
        }

        let settings = settings_json(&from_preset(&preset(json)).unwrap());

        assert_eq!(
            settings["border"],
            Value::Null,
            "presets saved before borders"
        );
        assert_eq!(settings["pad"], Value::Null);
        assert_eq!(settings["exportMasks"], false);
        assert_eq!(settings["preserveFolders"], false);
        assert_eq!(settings["destinationType"], "customFolder");
        assert_eq!(settings["subfolder"], "");
    }

    #[test]
    fn an_enabled_border_or_pad_fills_in_the_export_panels_defaults() {
        let mut json = preset_json();
        for key in [
            "padRatioWidth",
            "padRatioHeight",
            "padColor",
            "borderBasis",
            "borderHorizontalPercent",
            "borderVerticalPercent",
            "borderColor",
        ] {
            json.as_object_mut().unwrap().remove(key);
        }

        let settings = settings_json(&from_preset(&preset(json)).unwrap());

        assert_eq!(
            settings["border"],
            json!({
                "basis": "longEdge",
                "horizontalPercent": 2.0,
                "verticalPercent": 2.0,
                "color": "#ffffff"
            })
        );
        assert_eq!(
            settings["pad"],
            json!({ "ratioWidth": 1.0, "ratioHeight": 1.0, "color": "#ffffff" })
        );
    }

    #[test]
    fn the_output_format_is_the_formats_first_extension() {
        for (format, extension) in [
            ("jpeg", "jpg"),
            ("png", "png"),
            ("tiff", "tiff"),
            ("webp", "webp"),
            ("jxl", "jxl"),
            ("avif", "avif"),
            ("unheard-of", "jpg"),
        ] {
            let output = from_preset(&with(json!({ "fileFormat": format }))).unwrap();
            assert_eq!(output.output_format, extension, "{format}");
        }
    }

    #[test]
    fn a_preset_the_export_pipeline_would_refuse_is_an_error() {
        assert!(matches!(
            from_preset(&with(json!({ "resizeMode": "diagonal" }))),
            Err(PresetError::Failed { .. })
        ));
        assert!(matches!(
            from_preset(&with(json!({ "watermarkAnchor": null }))),
            Err(PresetError::Failed { .. })
        ));
        assert!(matches!(
            from_preset(&with(json!({ "borderBasis": "diagonal" }))),
            Err(PresetError::Failed { .. })
        ));
        assert!(
            from_preset(&with(json!({
                "resizeMode": "diagonal",
                "enableResize": false,
                "borderBasis": "diagonal",
                "enableBorder": false,
                "watermarkAnchor": null,
                "enableWatermark": false,
            })))
            .is_ok(),
            "only what is enabled has to make sense"
        );
    }

    #[test]
    fn a_preset_is_resolved_by_id() {
        let presets = vec![
            with(json!({ "id": "a", "jpegQuality": 60 })),
            with(json!({ "id": "b", "jpegQuality": 95 })),
        ];

        let output = resolve("b", &presets).unwrap();

        assert_eq!(output.export_settings.jpeg_quality, 95);
    }

    #[test]
    fn a_deleted_preset_is_preset_missing() {
        let presets = vec![with(json!({ "id": "a" }))];

        assert_eq!(
            resolve("deleted", &presets).unwrap_err(),
            PresetError::PresetMissing {
                preset_id: Some("deleted".into())
            }
        );
    }

    #[test]
    fn a_destination_publishes_only_with_its_own_preset() {
        let presets = vec![with(json!({ "id": "a", "jpegQuality": 60 }))];

        let configured = destination_output(Some("a"), &presets).unwrap();
        assert_eq!(configured.export_settings.jpeg_quality, 60);
        assert_eq!(configured.output_format, "jpg");

        assert_eq!(
            destination_output(None, &presets).unwrap_err(),
            PresetError::PresetMissing { preset_id: None },
            "with no preset chosen, nothing falls back to the Export panel"
        );
        assert_eq!(
            destination_output(Some("deleted"), &presets).unwrap_err(),
            PresetError::PresetMissing {
                preset_id: Some("deleted".into())
            }
        );
    }

    fn prints_for(path: &str, output: &PublishOutput) -> Fingerprints {
        let settings = RelevantExportSettings::from_export_settings(
            &output.export_settings,
            &output.output_format,
        );
        fingerprints(SystemTime::UNIX_EPOCH, path.len() as u64, "{}", &settings)
    }

    /// Two links holding three uploads published with preset `old`, and one
    /// link whose upload already uses `new`.
    fn published_with(presets: &[ExportPreset]) -> PublishState {
        let old = resolve("old", presets).unwrap();
        let new = resolve("new", presets).unwrap();
        let mut state = PublishState::empty("smugmug");
        for (album, paths, output) in [
            ("iceland", &["/p/1.raf", "/p/2.raf"][..], &old),
            ("best-of", &["/p/1.raf"][..], &old),
            ("web", &["/p/9.raf"][..], &new),
        ] {
            state.record_link(
                album,
                &RemoteContainerId(format!("/api/v2/album/{album}")),
                None,
            );
            for path in paths {
                state
                    .record_image(
                        album,
                        path,
                        &RemoteImageId(format!("/img/{album}{path}")),
                        &prints_for(path, output),
                        None,
                    )
                    .unwrap();
            }
        }
        state
    }

    fn two_presets() -> Vec<ExportPreset> {
        vec![
            with(json!({ "id": "old", "jpegQuality": 80 })),
            with(json!({ "id": "new", "jpegQuality": 95 })),
        ]
    }

    #[test]
    fn settings_impact_counts_uploads_across_links() {
        let presets = two_presets();
        let state = published_with(&presets);

        assert_eq!(
            settings_impact(&state, "new", &presets).unwrap(),
            SettingsImpact {
                photos: 3,
                albums: 2
            }
        );
        assert_eq!(
            settings_impact(&state, "old", &presets).unwrap(),
            SettingsImpact {
                photos: 1,
                albums: 1
            }
        );
        assert_eq!(
            settings_impact(&state, "deleted", &presets).unwrap_err(),
            PresetError::PresetMissing {
                preset_id: Some("deleted".into())
            }
        );
    }

    #[test]
    fn a_renamed_output_template_affects_nothing() {
        let presets = vec![
            with(json!({ "id": "old", "jpegQuality": 80 })),
            with(json!({ "id": "new", "jpegQuality": 95 })),
            with(json!({ "id": "renamed", "jpegQuality": 80, "filenameTemplate": "{sequence}" })),
        ];
        let state = published_with(&presets);

        assert_eq!(
            settings_impact(&state, "renamed", &presets).unwrap(),
            SettingsImpact {
                photos: 1,
                albums: 1
            },
            "only the upload made with the other quality"
        );
    }

    #[test]
    fn keeping_existing_uploads_marks_every_link_current() {
        let presets = two_presets();
        let mut state = published_with(&presets);

        assert_eq!(
            keep_existing_uploads(&mut state, Some("new"), &presets).unwrap(),
            3
        );

        assert_eq!(
            settings_impact(&state, "new", &presets).unwrap(),
            SettingsImpact::default()
        );
        let new = resolve("new", &presets).unwrap();
        assert_eq!(
            state.classify("best-of", "/p/1.raf", &prints_for("/p/1.raf", &new)),
            PublishAction::Skip
        );
        assert_eq!(
            keep_existing_uploads(&mut state, None, &presets).unwrap_err(),
            PresetError::PresetMissing { preset_id: None }
        );
    }

    #[test]
    fn preset_missing_serialises_for_the_panel() {
        assert_eq!(
            serde_json::to_value(PresetError::PresetMissing { preset_id: None }).unwrap(),
            json!({ "kind": "PresetMissing", "preset_id": null })
        );
    }
}
