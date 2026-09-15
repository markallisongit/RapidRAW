//! The Tauri commands, and the startup hook, that connect the publish module
//! to the app.
//!
//! Marshalling only: each command looks up its destination, assembles a
//! [`PublishContext`] and delegates. Anything worth testing lives in the
//! module that owns it, because this layer cannot be exercised without a
//! running app.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::stream::{self, StreamExt};
use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::AppState;
use crate::file_management::AlbumItem;
use crate::publish::links::{
    Candidates, ChosenPair, ExistingMatch, LinkError, LinkInfo, LinkTarget, Looks, PhotoFacts,
    ToLookAt, VisualHash, candidates, link_album, linked_images, list_links,
};
use crate::publish::preset::{
    PresetError, PublishOutput, destination_output, keep_existing_uploads, settings_impact,
};
use crate::publish::session::{
    ExportPipeline, PublishPreview, PublishRequest, PublishSession, SettingsChangePolicy,
    check_account, preview, refresh_destination, tauri_event_sink,
};
use crate::publish::settings::DestinationSettings;
use crate::publish::state::{
    AlbumMembership, PublishState, RefreshReport, SettingsImpact, state_dir,
};
use crate::publish::{
    AuthChallenge, AuthStatus, CaptureTime, DestinationCapabilities, PublishContext,
    PublishDestination, PublishError, RemoteImage, RemoteImageId, RemoteNode, RemoteNodeId,
    credential_store, spool,
};

#[derive(Serialize)]
pub struct DestinationInfo {
    id: &'static str,
    display_name: &'static str,
    capabilities: DestinationCapabilities,
}

#[tauri::command]
pub fn publish_get_destinations(state: State<'_, AppState>) -> Vec<DestinationInfo> {
    state
        .publish_registry
        .all()
        .iter()
        .map(|destination| DestinationInfo {
            id: destination.id(),
            display_name: destination.display_name(),
            capabilities: destination.capabilities(),
        })
        .collect()
}

#[tauri::command]
pub async fn publish_get_auth_status(
    destination_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<AuthStatus, String> {
    let destination = destination(&state, &destination_id)?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    Ok(destination.auth_status(&ctx).await?)
}

/// Disconnects when the credentials belong to a different application, whose
/// access token the new key could never sign with.
#[tauri::command]
pub async fn publish_set_credentials(
    destination_id: String,
    key: String,
    secret: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let destination = destination(&state, &destination_id)?;
    if credential_store::replace_consumer(&destination_id, &key, &secret)? {
        let ctx = context(&app_handle, &destination_id, idle_cancel())?;
        destination.disconnect(&ctx).await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn publish_begin_auth(
    destination_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<AuthChallenge, String> {
    let destination = destination(&state, &destination_id)?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    Ok(destination.begin_auth(&ctx).await?)
}

#[tauri::command]
pub async fn publish_complete_auth(
    destination_id: String,
    verifier: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let destination = destination(&state, &destination_id)?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    Ok(destination.complete_auth(&verifier, &ctx).await?)
}

#[tauri::command]
pub fn publish_get_settings(
    destination_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<DestinationSettings, String> {
    destination(&state, &destination_id)?;
    Ok(DestinationSettings::load_in(
        &state_dir(&app_handle)?,
        &destination_id,
    )?)
}

/// Refuses a privacy the destination cannot create an album with, rather
/// than storing a choice that would fail at the first new album.
#[tauri::command]
pub fn publish_set_settings(
    destination_id: String,
    settings: DestinationSettings,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let destination = destination(&state, &destination_id)?;
    if !destination
        .capabilities()
        .supported_privacy
        .contains(&settings.new_album_privacy)
    {
        return Err(format!(
            "{} cannot create albums with {:?} privacy",
            destination.display_name(),
            settings.new_album_privacy
        ));
    }
    Ok(settings.save_in(&state_dir(&app_handle)?, &destination_id)?)
}

/// Refused while publishing: the session would lose its token part way
/// through and fail every remaining photo.
#[tauri::command]
pub async fn publish_disconnect(
    destination_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let destination = destination(&state, &destination_id)?;
    let _session = state.publish_registry.begin_session()?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    Ok(destination.disconnect(&ctx).await?)
}

/// One level of the remote album tree: `parent` is a node id from an earlier
/// listing, and `None` the account root.
#[tauri::command]
pub async fn publish_list_remote(
    destination_id: String,
    parent: Option<String>,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<Vec<RemoteNode>, String> {
    let destination = destination(&state, &destination_id)?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    let parent = parent.map(RemoteNodeId);
    Ok(destination.list_containers(parent.as_ref(), &ctx).await?)
}

/// Needs no connection: names come from the local album tree.
#[tauri::command]
pub fn publish_list_links(
    destination_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<Vec<LinkInfo>, String> {
    destination(&state, &destination_id)?;
    let tree = crate::file_management::get_albums(app_handle.clone())?;
    let publish_state = load_state(&app_handle, &destination_id, &tree)?;
    Ok(list_links(&publish_state, &tree))
}

/// Refreshes one linked album, or every link when `album_id` is absent.
/// This reads the destination and updates only RapidRAW's local state.
#[tauri::command]
pub async fn publish_refresh(
    destination_id: String,
    album_id: Option<String>,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<RefreshReport, String> {
    let destination = destination(&state, &destination_id)?;
    let _session = state.publish_registry.begin_session()?;
    let tree = crate::file_management::get_albums(app_handle.clone())?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    let mut publish_state = load_state(&app_handle, &destination_id, &tree)?;
    check_account(destination.as_ref(), &ctx, &mut publish_state).await?;
    let report = refresh_destination(
        destination.as_ref(),
        &ctx,
        &mut publish_state,
        album_id.as_deref(),
    )
    .await?;
    publish_state.save_in(&ctx.state_dir)?;
    Ok(report)
}

/// Refused while publishing, as is unlinking: the session saves the state it
/// loaded at the start, which would silently undo the change.
#[tauri::command]
pub async fn publish_link_album(
    destination_id: String,
    album_id: String,
    target: LinkTarget,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<LinkInfo, LinkError> {
    let destination = destination(&state, &destination_id)?;
    let _session = state.publish_registry.begin_session()?;
    let tree = crate::file_management::get_albums(app_handle.clone())?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    let mut publish_state = load_state(&app_handle, &destination_id, &tree)?;

    let info = link_album(
        destination.as_ref(),
        &ctx,
        &mut publish_state,
        &tree,
        &album_id,
        target,
    )
    .await?;
    publish_state.save_in(&ctx.state_dir)?;
    Ok(info)
}

/// What the album's linked remote album already holds of its photos: paired
/// by the name publishing would give them, and failing that by the source
/// file's name, the capture time and how they look. Reads the destination
/// and the photos; records nothing.
///
/// Holds the session slot while it runs, so `publish_cancel` stops it; while
/// comparing looks it reports `publish-match-progress`.
#[tauri::command]
pub async fn publish_match_existing(
    destination_id: String,
    album_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<ExistingMatch, PresetError> {
    let destination = destination(&state, &destination_id)?;
    let request = album(&app_handle, &album_id)?;
    let output = output(&app_handle, &destination_id)?;
    let session = state.publish_registry.begin_session()?;
    let ctx = context(&app_handle, &destination_id, session.cancel_flag())?;
    let mut publish_state =
        PublishState::load_in(&ctx.state_dir, &destination_id, &request.albums)?;
    check_account(destination.as_ref(), &ctx, &mut publish_state).await?;
    let remote = linked_images(destination.as_ref(), &ctx, &publish_state, &album_id).await?;
    let pipeline = ExportPipeline::new(
        app_handle.clone(),
        output.export_settings,
        output.output_format,
        idle_cancel(),
    );

    // A fingerprint per photo, as for a preview, and a sidecar read for its
    // capture time.
    let identity = Arc::clone(&destination);
    let found = tauri::async_runtime::spawn_blocking(move || {
        candidates(
            &pipeline,
            &SourceFacts,
            &publish_state,
            &album_id,
            &request.paths,
            remote,
            &|id| identity.image_identity(id),
        )
    })
    .await
    .map_err(|e| PresetError::from(e.to_string()))?;

    let gpu = crate::gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();
    let looks = read_looks(destination.as_ref(), &ctx, &found, &app_handle, gpu).await?;
    Ok(found.pair(&looks))
}

/// Records the pairs the user chose from [`publish_match_existing`], each
/// checked against a fresh listing of the remote album: a pair whose remote
/// image has gone, or whose photo has been recorded since, is skipped.
/// Returns how many were recorded. Nothing on the destination changes.
/// Refused while publishing, like linking.
#[tauri::command]
pub async fn publish_adopt_existing(
    destination_id: String,
    album_id: String,
    pairs: Vec<ChosenPair>,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<usize, PresetError> {
    let destination = destination(&state, &destination_id)?;
    let request = album(&app_handle, &album_id)?;
    let output = output(&app_handle, &destination_id)?;
    let _session = state.publish_registry.begin_session()?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    let mut publish_state =
        PublishState::load_in(&ctx.state_dir, &destination_id, &request.albums)?;
    check_account(destination.as_ref(), &ctx, &mut publish_state).await?;
    let remote = linked_images(destination.as_ref(), &ctx, &publish_state, &album_id).await?;
    let pipeline = ExportPipeline::new(
        app_handle,
        output.export_settings,
        output.output_format,
        idle_cancel(),
    );

    tauri::async_runtime::spawn_blocking(move || {
        let identity = |id: &RemoteImageId| destination.image_identity(id);
        let found = candidates(
            &pipeline,
            &NoCaptureTimes,
            &publish_state,
            &album_id,
            &request.paths,
            remote,
            &identity,
        );
        let recorded = found.adopt(&mut publish_state, &pairs, &identity)?;
        if recorded > 0 {
            publish_state.save_in(&ctx.state_dir)?;
        }
        Ok(recorded)
    })
    .await
    .map_err(|e| PresetError::from(e.to_string()))?
}

/// A local photo's thumbnail for reviewing pairs, as a `data:` URL: the
/// edited picture, from RapidRAW's thumbnail cache, rendered when missing.
#[tauri::command]
pub async fn publish_local_thumbnail(
    path: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<String, String> {
    let gpu = crate::gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();
    tauri::async_runtime::spawn_blocking(move || {
        let image = crate::file_management::get_cached_or_generate_thumbnail_image(
            &path,
            &app_handle,
            gpu.as_ref(),
        )
        .map_err(|e| e.to_string())?;
        let small = image.thumbnail(REVIEW_THUMBNAIL_PX, REVIEW_THUMBNAIL_PX);
        let mut jpeg = std::io::Cursor::new(Vec::new());
        small
            .to_rgb8()
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .map_err(|e| e.to_string())?;
        Ok(jpeg_data_url(jpeg.get_ref()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A remote image's thumbnail for reviewing pairs, as a `data:` URL, fetched
/// through the destination. `None` when it has none to give.
#[tauri::command]
pub async fn publish_remote_thumbnail(
    destination_id: String,
    thumbnail_url: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<Option<String>, String> {
    let destination = destination(&state, &destination_id)?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    let image = RemoteImage {
        thumbnail_url: Some(thumbnail_url),
        ..RemoteImage::named(RemoteImageId(String::new()), "")
    };
    Ok(destination
        .fetch_thumbnail(&image, &ctx)
        .await?
        .map(|bytes| jpeg_data_url(&bytes)))
}

/// Forgets the link and its image records. Nothing on the destination is
/// touched, and unlinking an album that is not linked is not an error.
#[tauri::command]
pub fn publish_unlink(
    destination_id: String,
    album_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    destination(&state, &destination_id)?;
    let _session = state.publish_registry.begin_session()?;
    let tree = crate::file_management::get_albums(app_handle.clone())?;
    let mut publish_state = load_state(&app_handle, &destination_id, &tree)?;
    if publish_state.unlink_album(&album_id) {
        publish_state.save_in(&state_dir(&app_handle)?)?;
    }
    Ok(())
}

/// Counts new, edited, settings-changed and unchanged photos, for the
/// destination's preset. Never renders or uploads, and needs no connection:
/// it reads only the state file, the photos and, when connected, which
/// account that is.
#[tauri::command]
pub async fn publish_preview(
    destination_id: String,
    album_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<PublishPreview, PresetError> {
    let destination = destination(&state, &destination_id)?;
    let request = album(&app_handle, &album_id)?;
    let output = output(&app_handle, &destination_id)?;
    let ctx = context(&app_handle, &destination_id, idle_cancel())?;
    let mut publish_state =
        PublishState::load_in(&ctx.state_dir, &destination_id, &request.albums)?;
    // Refused like the publish it previews. The claim this may record is
    // never saved: a preview writes nothing.
    check_account(destination.as_ref(), &ctx, &mut publish_state).await?;
    let pipeline = ExportPipeline::new(
        app_handle,
        output.export_settings,
        output.output_format,
        idle_cancel(),
    );

    // A stat and a sidecar read per photo is too much blocking for an async
    // worker once an album runs to thousands.
    tauri::async_runtime::spawn_blocking(move || {
        preview(
            &pipeline,
            &publish_state,
            &request.album.album_id,
            &request.paths,
        )
    })
    .await
    .map_err(|e| e.to_string().into())
}

/// Starts publishing `album_id` and returns at once. The session reports
/// through `publish-progress` and ends with exactly one of
/// `publish-complete`, `publish-cancelled` or `publish-error`.
///
/// Renders with the destination's preset. `on_settings_change` decides
/// whether photos whose only change is that preset upload again, and is
/// required so that never happens without the user having been asked. The
/// album must be linked: publishing never finds or creates one by name.
#[tauri::command]
pub async fn publish_album(
    destination_id: String,
    album_id: String,
    on_settings_change: SettingsChangePolicy,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), PresetError> {
    let destination = destination(&state, &destination_id)?;
    let request = album(&app_handle, &album_id)?;
    let output = output(&app_handle, &destination_id)?;
    let spool_base = spool::spool_root(&app_handle)?;
    let session = state.publish_registry.begin_session()?;
    let ctx = context(&app_handle, &destination_id, session.cancel_flag())?;

    tauri::async_runtime::spawn(async move {
        // Held until the session ends, however it ends.
        let _session = session;
        let pipeline = ExportPipeline::new(
            app_handle.clone(),
            output.export_settings,
            output.output_format,
            Arc::clone(&ctx.cancel),
        );
        let events = tauri_event_sink(app_handle);
        let run = PublishSession::new(destination.as_ref(), &pipeline, &ctx, spool_base, events)
            .run(request, on_settings_change)
            .await;
        if let Err(error) = run {
            log::error!("Publishing to {destination_id} stopped: {error}");
        }
    });
    Ok(())
}

/// How many published photos switching the destination to
/// `export_preset_id` would upload again, and in how many albums. Compares
/// settings hashes only, so it reads no photos.
#[tauri::command]
pub fn publish_settings_impact(
    destination_id: String,
    export_preset_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<SettingsImpact, PresetError> {
    destination(&state, &destination_id)?;
    let tree = crate::file_management::get_albums(app_handle.clone())?;
    let publish_state = load_state(&app_handle, &destination_id, &tree)?;
    settings_impact(
        &publish_state,
        &export_preset_id,
        &export_presets(&app_handle)?,
    )
}

/// Records every published photo as current with the destination's preset,
/// so switching to it uploads nothing again. Returns how many records
/// changed. Refused while publishing, like linking.
#[tauri::command]
pub fn publish_keep_existing_uploads(
    destination_id: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<usize, PresetError> {
    destination(&state, &destination_id)?;
    let _session = state.publish_registry.begin_session()?;
    let dir = state_dir(&app_handle)?;
    let tree = crate::file_management::get_albums(app_handle.clone())?;
    let mut publish_state = load_state(&app_handle, &destination_id, &tree)?;
    let preset_id = DestinationSettings::load_in(&dir, &destination_id)?.export_preset_id;

    let changed = keep_existing_uploads(
        &mut publish_state,
        preset_id.as_deref(),
        &export_presets(&app_handle)?,
    )?;
    if changed > 0 {
        publish_state.save_in(&dir)?;
    }
    Ok(changed)
}

/// `false` when there was no session to cancel, which is not an error: the
/// session may simply have finished as the user clicked.
#[tauri::command]
pub fn publish_cancel(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.publish_registry.cancel_session()?)
}

/// Clears spool directories left by a session that was killed. On its own
/// thread, logging rather than failing: nothing about a stale cache
/// directory is worth delaying or breaking startup for.
pub fn sweep_spool_in_background(app_handle: AppHandle) {
    std::thread::spawn(move || match spool::sweep_orphaned_sessions(&app_handle) {
        Ok(0) => {}
        Ok(removed) => log::info!("Removed {removed} orphaned publish spool(s)"),
        Err(error) => log::warn!("Sweeping orphaned publish spools failed: {error}"),
    });
}

impl From<PublishError> for String {
    fn from(error: PublishError) -> Self {
        error.to_string()
    }
}

fn destination(
    state: &AppState,
    destination_id: &str,
) -> Result<Arc<dyn PublishDestination>, String> {
    state
        .publish_registry
        .get(destination_id)
        .cloned()
        .ok_or_else(|| format!("unknown publish destination: {destination_id}"))
}

fn album(app_handle: &AppHandle, album_id: &str) -> Result<PublishRequest, String> {
    let tree = crate::file_management::get_albums(app_handle.clone())?;
    PublishRequest::from_album_tree(&tree, album_id)
        .ok_or_else(|| format!("no album with id {album_id}"))
}

/// What `destination_id` publishes with: its preset, which must be chosen.
fn output(app_handle: &AppHandle, destination_id: &str) -> Result<PublishOutput, PresetError> {
    let settings = DestinationSettings::load_in(&state_dir(app_handle)?, destination_id)?;
    destination_output(
        settings.export_preset_id.as_deref(),
        &export_presets(app_handle)?,
    )
}

fn export_presets(
    app_handle: &AppHandle,
) -> Result<Vec<crate::app_settings::ExportPreset>, String> {
    Ok(crate::app_settings::load_settings(app_handle.clone())?.export_presets)
}

/// The album tree migrates a v1 state file; the caller has it loaded anyway.
fn load_state(
    app_handle: &AppHandle,
    destination_id: &str,
    tree: &[AlbumItem],
) -> Result<PublishState, PublishError> {
    PublishState::load_in(
        &state_dir(app_handle)?,
        destination_id,
        &AlbumMembership::from_tree(tree),
    )
}

fn context(
    app_handle: &AppHandle,
    destination_id: &str,
    cancel: Arc<AtomicBool>,
) -> Result<PublishContext, PublishError> {
    let state_dir = state_dir(app_handle)?;
    Ok(PublishContext {
        new_container_privacy: DestinationSettings::load_in(&state_dir, destination_id)?
            .new_album_privacy,
        state_dir,
        consumer: credential_store::load_consumer(destination_id)?,
        cancel,
    })
}

/// For calls outside a session, which nothing can cancel.
fn idle_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Big enough to judge a pair at a glance, small enough to send dozens.
const REVIEW_THUMBNAIL_PX: u32 = 320;

/// Remote thumbnails fetched at once while comparing looks.
const THUMBNAIL_FETCHES: usize = 4;

fn jpeg_data_url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Capture times from each photo's source file: the cached EXIF RapidRAW
/// keeps beside it, read from the file itself the first time.
struct SourceFacts;

impl PhotoFacts for SourceFacts {
    fn capture(&self, virtual_path: &str) -> (Option<CaptureTime>, Option<String>) {
        let (source, _) = crate::file_management::parse_virtual_path(virtual_path);
        let exif = crate::exif_processing::read_rrexif_sidecar(&source)
            .or_else(|| {
                let bytes = std::fs::read(&source).ok()?;
                Some(crate::exif_processing::read_exif_data(
                    &source.to_string_lossy(),
                    &bytes,
                ))
            })
            .unwrap_or_default();
        // Read as file naming reads it, which takes the camera's clock as
        // local time; turned back, that is the clock again.
        let at = crate::exif_processing::try_get_exif_creation_date(&source)
            .map(|utc| utc.with_timezone(&chrono::Local).naive_local());
        let millis = exif
            .get("SubSecTimeOriginal")
            .and_then(|digits| CaptureTime::millis_from_fraction(digits));
        let model = exif
            .get("Model")
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        (at.map(|at| CaptureTime::new(at, millis)), model)
    }
}

/// For adopting, which pairs nothing and so compares no capture times.
struct NoCaptureTimes;

impl PhotoFacts for NoCaptureTimes {
    fn capture(&self, _virtual_path: &str) -> (Option<CaptureTime>, Option<String>) {
        (None, None)
    }
}

#[derive(Clone, Serialize)]
struct MatchProgress {
    checked: usize,
    total: usize,
}

/// Hashes what [`Candidates::to_look_at`] names: the local photos' edited
/// thumbnails on the blocking pool, the remote thumbnails meanwhile. A photo
/// whose picture cannot be read just has no looks to compare. Stops between
/// photos when `ctx.cancel` is set.
async fn read_looks(
    destination: &dyn PublishDestination,
    ctx: &PublishContext,
    found: &Candidates,
    app_handle: &AppHandle,
    gpu: Option<crate::image_processing::GpuContext>,
) -> Result<Looks, PublishError> {
    let ToLookAt { local, remote } = found.to_look_at();
    let total = local.len() + remote.len();
    let mut looks = Looks::default();
    if total == 0 {
        return Ok(looks);
    }
    let checked = Arc::new(AtomicUsize::new(0));
    let report = {
        let checked = Arc::clone(&checked);
        let app_handle = app_handle.clone();
        move || {
            let checked = checked.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = app_handle.emit("publish-match-progress", MatchProgress { checked, total });
        }
    };
    let _ = app_handle.emit(
        "publish-match-progress",
        MatchProgress { checked: 0, total },
    );

    let local: Vec<(usize, String)> = local
        .into_iter()
        .map(|(index, path)| (index, path.to_string()))
        .collect();
    let hashing = {
        let cancel = Arc::clone(&ctx.cancel);
        let app_handle = app_handle.clone();
        let report = report.clone();
        tauri::async_runtime::spawn_blocking(move || {
            local
                .par_iter()
                .filter_map(|(index, path)| {
                    if cancel.load(Ordering::SeqCst) {
                        return None;
                    }
                    let image = crate::file_management::get_cached_or_generate_thumbnail_image(
                        path,
                        &app_handle,
                        gpu.as_ref(),
                    );
                    report();
                    match image {
                        Ok(image) => Some((*index, VisualHash::of(&image))),
                        Err(error) => {
                            log::warn!("No thumbnail of {path} to compare: {error}");
                            None
                        }
                    }
                })
                .collect::<HashMap<usize, VisualHash>>()
        })
    };

    // Built here rather than in a closure passed to the stream, whose
    // borrows the command's future could not prove `Send`.
    let fetching: Vec<_> = remote
        .into_iter()
        .map(|(index, image)| {
            let fetch = destination.fetch_thumbnail(image, ctx);
            async move { (index, fetch.await) }
        })
        .collect();
    let mut fetches = stream::iter(fetching).buffer_unordered(THUMBNAIL_FETCHES);
    while let Some((index, fetched)) = fetches.next().await {
        if ctx.cancel.load(Ordering::SeqCst) {
            break;
        }
        report();
        let image = match fetched {
            Ok(Some(bytes)) => image::load_from_memory(&bytes).map_err(|e| e.to_string()),
            Ok(None) => continue,
            Err(error) => Err(error.to_string()),
        };
        match image {
            Ok(image) => {
                looks.remote.insert(index, VisualHash::of(&image));
            }
            Err(error) => log::warn!("No remote thumbnail to compare: {error}"),
        }
    }
    drop(fetches);

    looks.local = hashing.await.map_err(|e| PublishError::Io(e.to_string()))?;
    if ctx.cancel.load(Ordering::SeqCst) {
        return Err(PublishError::Cancelled);
    }
    Ok(looks)
}
