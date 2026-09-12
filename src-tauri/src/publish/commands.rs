//! The Tauri commands, and the startup hook, that connect the publish module
//! to the app.
//!
//! Marshalling only: each command looks up its destination, assembles a
//! [`PublishContext`] and delegates. Anything worth testing lives in the
//! module that owns it, because this layer cannot be exercised without a
//! running app.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::AppState;
use crate::export_processing::ExportSettings;
use crate::publish::session::{
    ExportPipeline, PublishPreview, PublishRequest, PublishSession, preview, tauri_event_sink,
};
use crate::publish::state::{PublishState, state_dir};
use crate::publish::{
    AuthChallenge, AuthStatus, DestinationCapabilities, PublishContext, PublishDestination,
    PublishError, credential_store, spool,
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

#[tauri::command]
pub fn publish_set_credentials(
    destination_id: String,
    key: String,
    secret: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    destination(&state, &destination_id)?;
    Ok(credential_store::replace_consumer(
        &state_dir(&app_handle)?,
        &destination_id,
        &key,
        &secret,
    )?)
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

/// Counts new, changed and unchanged photos. Never renders or uploads, and
/// needs no connection: it reads only the state file and the photos.
#[tauri::command]
pub async fn publish_preview(
    destination_id: String,
    album_id: String,
    export_settings: ExportSettings,
    output_format: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<PublishPreview, String> {
    destination(&state, &destination_id)?;
    let request = album(&app_handle, &album_id)?;
    let publish_state = PublishState::load_in(&state_dir(&app_handle)?, &destination_id)?;
    let pipeline = ExportPipeline::new(app_handle, export_settings, output_format, idle_cancel());

    // A stat and a sidecar read per photo is too much blocking for an async
    // worker once an album runs to thousands.
    tauri::async_runtime::spawn_blocking(move || preview(&pipeline, &publish_state, &request.paths))
        .await
        .map_err(|e| e.to_string())
}

/// Starts publishing `album_id` and returns at once. The session reports
/// through `publish-progress` and ends with exactly one of
/// `publish-complete`, `publish-cancelled` or `publish-error`.
#[tauri::command]
pub async fn publish_album(
    destination_id: String,
    album_id: String,
    export_settings: ExportSettings,
    output_format: String,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let destination = destination(&state, &destination_id)?;
    let request = album(&app_handle, &album_id)?;
    let spool_base = spool::spool_root(&app_handle)?;
    let session = state.publish_registry.begin_session()?;
    let ctx = context(&app_handle, &destination_id, session.cancel_flag())?;

    tauri::async_runtime::spawn(async move {
        // Held until the session ends, however it ends.
        let _session = session;
        let pipeline = ExportPipeline::new(
            app_handle.clone(),
            export_settings,
            output_format,
            Arc::clone(&ctx.cancel),
        );
        let events = tauri_event_sink(app_handle);
        let run = PublishSession::new(destination.as_ref(), &pipeline, &ctx, spool_base, events)
            .run(request)
            .await;
        if let Err(error) = run {
            log::error!("Publishing to {destination_id} stopped: {error}");
        }
    });
    Ok(())
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

fn context(
    app_handle: &AppHandle,
    destination_id: &str,
    cancel: Arc<AtomicBool>,
) -> Result<PublishContext, PublishError> {
    Ok(PublishContext {
        state_dir: state_dir(app_handle)?,
        consumer: credential_store::load_consumer(destination_id)?,
        cancel,
    })
}

/// For calls outside a session, which nothing can cancel.
fn idle_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
