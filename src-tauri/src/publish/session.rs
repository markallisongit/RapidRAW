//! The session driver: decides what needs publishing, renders it through the
//! export pipeline, uploads it, and records what landed.
//!
//! Per image, in this order:
//!
//! 1. Fingerprint the source (a `stat`, the sidecar, the export settings).
//! 2. Classify it against the album's link in the state file. A `Skip` stops
//!    here, before any rendering: an unchanged album costs one `stat` per
//!    photo and no GPU time.
//! 3. Render into the spool through the unmodified export pipeline.
//! 4. Upload, replacing the remote image when this is an update.
//! 5. On success, write the state entry and release the spooled file at once.
//!
//! Rendering runs in chunks, with chunk *n + 1* rendering while chunk *n*
//! uploads, so the GPU and the network overlap and the spool holds at most
//! about two chunks at a time however large the album is.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use uuid::Uuid;

use crate::export_processing::{ExportAdjustmentsMode, ExportSettings};
use crate::file_management::AlbumItem;
use crate::publish::spool::Spool;
use crate::publish::state::{
    AlbumMembership, Fingerprints, PublishAction, PublishState, RefreshReport,
    RelevantExportSettings, fingerprints,
};
use crate::publish::{
    AuthStatus, LocalContainer, PublishContext, PublishDestination, PublishError, PublishItem,
    RemoteContainerId, RemoteImageId,
};

/// Images per export call. A guess worth measuring: large enough that the
/// export pipeline's own thread pool has work to share out, small enough that
/// two chunks of 45 MP renders (~320 MB) sit comfortably in the spool.
pub const RENDER_CHUNK_SIZE: usize = 8;

/// Concurrent uploads. Also a guess worth measuring. Deliberately not the
/// export pipeline's core-and-RAM heuristic, which sizes GPU work and says
/// nothing about an uplink or a destination's rate limits.
pub const UPLOAD_CONCURRENCY: usize = 3;

/// What the session asks of the renderer. The real one is [`ExportPipeline`];
/// tests substitute one that renders nothing.
#[async_trait]
pub trait RenderPipeline: Send + Sync {
    /// The MIME type of what [`render`](Self::render) produces.
    fn mime(&self) -> &'static str;

    /// What decides whether a photo needs publishing again. Runs for every
    /// photo, the skipped ones included, so it must stay cheap: a `stat` and
    /// the sidecar, never a decode.
    fn fingerprints(&self, virtual_path: &str) -> Result<Fingerprints, PublishError>;

    /// The name the image goes by at the destination. Only asked of photos
    /// that will be uploaded. `index` and `total` feed `{sequence}`.
    fn file_name(
        &self,
        virtual_path: &str,
        index: usize,
        total: usize,
    ) -> Result<String, PublishError>;

    /// Renders every path into `out_dir`. One entry per input, in input
    /// order; `None` where that image failed to render.
    async fn render(
        &self,
        virtual_paths: &[String],
        out_dir: &Path,
    ) -> Result<Vec<Option<PathBuf>>, PublishError>;
}

/// One local album to publish.
pub struct PublishRequest {
    pub album: LocalContainer,
    /// Virtual paths, so virtual copies publish as distinct photos.
    pub paths: Vec<String>,
    /// Every album's photos, which loading a v1 state file needs to migrate it.
    pub albums: AlbumMembership,
}

impl PublishRequest {
    /// The album `album_id` from the album tree, with the names of the groups
    /// above it. `None` when no album has that id — a group's id included,
    /// since phase 1 publishes one album at a time.
    pub fn from_album_tree(tree: &[AlbumItem], album_id: &str) -> Option<Self> {
        let (album, images) = find_album(tree, album_id)?;
        Some(PublishRequest {
            album,
            paths: images.to_vec(),
            albums: AlbumMembership::from_tree(tree),
        })
    }
}

/// The album `album_id` anywhere in the tree, with the names of the groups
/// above it and its photos. `None` for a group's id, which holds no photos.
pub fn find_album<'t>(
    tree: &'t [AlbumItem],
    album_id: &str,
) -> Option<(LocalContainer, &'t [String])> {
    fn find<'t>(
        items: &'t [AlbumItem],
        album_id: &str,
        parents: &mut Vec<String>,
    ) -> Option<(LocalContainer, &'t [String])> {
        for item in items {
            match item {
                AlbumItem::Album {
                    id, name, images, ..
                } if id == album_id => {
                    let album = LocalContainer {
                        album_id: id.clone(),
                        name: name.clone(),
                        parent_path: parents.clone(),
                    };
                    return Some((album, images));
                }
                AlbumItem::Album { .. } => {}
                AlbumItem::Group { name, children, .. } => {
                    parents.push(name.clone());
                    if let Some(found) = find(children, album_id, parents) {
                        return Some(found);
                    }
                    parents.pop();
                }
            }
        }
        None
    }
    find(tree, album_id, &mut Vec::new())
}

/// What publishing would do, counted without rendering or uploading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PublishPreview {
    pub new: usize,
    /// Edited since they were published.
    pub update: usize,
    /// Unedited, but published with different output settings. Uploaded
    /// again only under [`SettingsChangePolicy::Republish`].
    pub settings_changed: usize,
    pub skip: usize,
    /// Photos that could not be fingerprinted — a missing source file, most
    /// likely. Publishing would report each one as failed.
    pub unreadable: usize,
}

/// Classifies every photo exactly as [`PublishSession::run`] would, at the
/// same cost as a republish of an unchanged album: a fingerprint per photo.
pub fn preview(
    pipeline: &dyn RenderPipeline,
    state: &PublishState,
    album_id: &str,
    paths: &[String],
) -> PublishPreview {
    let mut counts = PublishPreview::default();
    for path in paths {
        match pipeline.fingerprints(path) {
            Ok(fingerprints) => match state.classify(album_id, path, &fingerprints) {
                PublishAction::New => counts.new += 1,
                PublishAction::Update { .. } => counts.update += 1,
                PublishAction::SettingsChanged { .. } => counts.settings_changed += 1,
                PublishAction::Skip => counts.skip += 1,
            },
            Err(_) => counts.unreadable += 1,
        }
    }
    counts
}

/// What a publish does with a photo that would upload again only because the
/// output settings changed — Lightroom's "Republish all / Leave as-is".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SettingsChangePolicy {
    /// Render and upload it, replacing the existing upload, as for an edit.
    Republish,
    /// Leave the existing upload alone and record it as current with the new
    /// settings.
    KeepExisting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemState {
    Skipped,
    Uploaded,
    Updated,
    Failed,
    Ambiguous,
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub completed: usize,
    pub total: usize,
    pub current_file: String,
    pub state: ItemState,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailedItem {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionSummary {
    pub uploaded: usize,
    pub updated: usize,
    pub skipped: usize,
    pub failed: Vec<FailedItem>,
    /// Neither confirmed nor refuted, even after reconciliation. Not recorded
    /// in the state file, so the next publish tries them again.
    pub ambiguous: Vec<String>,
    pub cancelled: bool,
}

/// Mirrors the export path's `export-progress` / `export-complete` naming so
/// the frontend patterns carry over.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    Progress(Progress),
    Complete(SessionSummary),
    Error(String),
    Cancelled(SessionSummary),
}

impl SessionEvent {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Progress(_) => "publish-progress",
            Self::Complete(_) => "publish-complete",
            Self::Error(_) => "publish-error",
            Self::Cancelled(_) => "publish-cancelled",
        }
    }
}

pub type EventSink = Arc<dyn Fn(SessionEvent) + Send + Sync>;

/// Emits session events to the frontend.
pub fn tauri_event_sink(app_handle: tauri::AppHandle) -> EventSink {
    Arc::new(move |event| {
        let name = event.name();
        let _ = match event {
            SessionEvent::Progress(progress) => app_handle.emit(name, progress),
            SessionEvent::Complete(summary) | SessionEvent::Cancelled(summary) => {
                app_handle.emit(name, summary)
            }
            SessionEvent::Error(message) => app_handle.emit(name, message),
        };
    })
}

/// Refuses when the destination is connected to a different account from the
/// one the state's links belong to, and records the connected account when
/// nothing is linked yet. Not being connected passes: a preview needs no
/// connection, and a publish fails on its first call without one.
pub async fn check_account(
    destination: &dyn PublishDestination,
    ctx: &PublishContext,
    state: &mut PublishState,
) -> Result<(), PublishError> {
    match destination.auth_status(ctx).await? {
        AuthStatus::Connected { account } => state.claim_account(&account),
        AuthStatus::NotConfigured | AuthStatus::NotAuthorised => Ok(()),
    }
}

/// Reconciles one link, or every link, with the destination's current
/// read-only view. Callers decide when to persist so a failed multi-link
/// refresh never writes a partial result.
pub async fn refresh_destination(
    destination: &dyn PublishDestination,
    ctx: &PublishContext,
    state: &mut PublishState,
    album_id: Option<&str>,
) -> Result<RefreshReport, PublishError> {
    if !destination.capabilities().supports_reconcile {
        return Err(PublishError::Rejected(format!(
            "{} does not support refreshing linked albums",
            destination.display_name()
        )));
    }

    let targets: Vec<(String, RemoteContainerId)> = state
        .links()
        .filter(|(id, _)| album_id.is_none_or(|wanted| wanted == id.as_str()))
        .map(|(id, link)| (id.clone(), RemoteContainerId(link.remote_uri.clone())))
        .collect();
    if let Some(album_id) = album_id
        && targets.is_empty()
    {
        return Err(PublishError::Rejected(format!(
            "album {album_id} is not linked"
        )));
    }

    let mut report = RefreshReport::default();
    for (album_id, container) in targets {
        let snapshot = destination.inspect_container(&container, ctx).await?;
        report.include(state.apply_snapshot(&album_id, snapshot.as_ref(), |image| {
            destination.image_identity(image)
        })?);
    }
    Ok(report)
}

pub struct PublishSession<'a> {
    destination: &'a dyn PublishDestination,
    pipeline: &'a dyn RenderPipeline,
    ctx: &'a PublishContext,
    /// Parent of the session's spool directory: the app cache in the app, a
    /// temp directory under test.
    spool_base: PathBuf,
    events: EventSink,
}

impl<'a> PublishSession<'a> {
    pub fn new(
        destination: &'a dyn PublishDestination,
        pipeline: &'a dyn RenderPipeline,
        ctx: &'a PublishContext,
        spool_base: PathBuf,
        events: EventSink,
    ) -> Self {
        Self {
            destination,
            pipeline,
            ctx,
            spool_base,
            events,
        }
    }

    /// Runs the whole session and emits exactly one terminal event.
    ///
    /// A failure of one image is recorded in the summary and never ends the
    /// session; an `Err` here means the session itself could not continue —
    /// no album, an unwritable state file, a renderer that would not start.
    pub async fn run(
        &self,
        request: PublishRequest,
        on_settings_change: SettingsChangePolicy,
    ) -> Result<SessionSummary, PublishError> {
        let result = self.drive(request, on_settings_change).await;
        (self.events)(match &result {
            Ok(summary) if summary.cancelled => SessionEvent::Cancelled(summary.clone()),
            Ok(summary) => SessionEvent::Complete(summary.clone()),
            Err(error) => SessionEvent::Error(error.to_string()),
        });
        result
    }

    async fn drive(
        &self,
        request: PublishRequest,
        on_settings_change: SettingsChangePolicy,
    ) -> Result<SessionSummary, PublishError> {
        let capabilities = self.destination.capabilities();
        let mime = self.pipeline.mime();
        if !capabilities.accepted_mime_types.contains(&mime) {
            return Err(PublishError::Rejected(format!(
                "{} does not accept {mime}",
                self.destination.display_name()
            )));
        }

        let mut run = Run {
            state: PublishState::load_in(
                &self.ctx.state_dir,
                self.destination.id(),
                &request.albums,
            )?,
            state_dir: &self.ctx.state_dir,
            album_id: request.album.album_id.clone(),
            events: &self.events,
            summary: SessionSummary::default(),
            completed: 0,
            total: request.paths.len(),
        };
        // Before anything remote: another account's ids would name albums and
        // images it does not own.
        check_account(self.destination, self.ctx, &mut run.state).await?;
        if self.cancelled() {
            run.summary.cancelled = true;
            return Ok(run.summary);
        }

        // One remote listing immediately before classification prevents a
        // publish from replacing a deleted image or rendering for an album
        // that has disappeared. Save before the broken-link refusal below so
        // the panel shows what the session discovered.
        if capabilities.supports_reconcile {
            refresh_destination(
                self.destination,
                self.ctx,
                &mut run.state,
                Some(&request.album.album_id),
            )
            .await?;
            run.save()?;
        }

        let container = match run.state.link(&request.album.album_id) {
            Some(link) if link.broken => {
                return Err(PublishError::Rejected(format!(
                    "the linked album no longer exists on {}. Link \"{}\" to another album first",
                    self.destination.display_name(),
                    request.album.name
                )));
            }
            Some(link) => RemoteContainerId(link.remote_uri.clone()),
            // Never found or created by name: the user chooses where an album
            // goes, and sees the privacy of one RapidRAW creates, when linking.
            None => {
                return Err(PublishError::Rejected(format!(
                    "\"{}\" is not linked to an album on {}. Link it first",
                    request.album.name,
                    self.destination.display_name()
                )));
            }
        };

        // Every skip is decided here, before the spool exists and before
        // anything is rendered.
        let work = self.classify(&request.paths, on_settings_change, &mut run);
        if work.is_empty() {
            return run.finish();
        }

        let spool = Spool::create_at(&self.spool_base)?;
        let mut queued = work.into_iter();
        let mut rendered = Vec::new();
        let mut ambiguous = Vec::new();
        let mut chunk_number = 0;

        loop {
            if self.cancelled() {
                run.summary.cancelled = true;
                break;
            }
            let chunk: Vec<Work> = queued.by_ref().take(RENDER_CHUNK_SIZE).collect();
            if chunk.is_empty() && rendered.is_empty() {
                break;
            }

            // Chunk n + 1 renders while chunk n uploads.
            let uploading = std::mem::take(&mut rendered);
            let (render_result, upload_result) = futures::join!(
                self.render_chunk(&spool, chunk_number, chunk),
                self.upload_all(&spool, &container, uploading, &mut run, &mut ambiguous),
            );
            upload_result?;
            let (ready, render_failures) = render_result?;
            chunk_number += 1;

            // A render cut short by cancellation is not a render failure.
            if self.cancelled() {
                run.summary.cancelled = true;
                break;
            }
            for (work, error) in render_failures {
                run.fail(&work.path, &error);
            }
            rendered = ready;
        }

        if run.summary.cancelled {
            // Possibly landed, possibly not; nothing is recorded, so the next
            // publish finds out.
            for item in &ambiguous {
                run.unresolved(&item.work);
            }
        } else if !ambiguous.is_empty() {
            self.resolve(&spool, &container, ambiguous, &mut run, &capabilities)
                .await?;
            run.summary.cancelled = self.cancelled();
        }

        run.finish()
    }

    fn cancelled(&self) -> bool {
        self.ctx.cancel.load(Ordering::SeqCst)
    }

    /// Fingerprints and classifies every photo, reporting skips and
    /// preparation failures as they are found, and returns the rest.
    fn classify(
        &self,
        paths: &[String],
        on_settings_change: SettingsChangePolicy,
        run: &mut Run<'_>,
    ) -> Vec<Work> {
        let mut uploads = Vec::new();

        for (index, path) in paths.iter().enumerate() {
            let fingerprints = match self.pipeline.fingerprints(path) {
                Ok(fingerprints) => fingerprints,
                Err(error) => {
                    run.fail(path, &error);
                    continue;
                }
            };
            let replaces = match run.state.classify(&run.album_id, path, &fingerprints) {
                PublishAction::Skip => {
                    run.skip(path, &fingerprints);
                    continue;
                }
                PublishAction::SettingsChanged { .. }
                    if on_settings_change == SettingsChangePolicy::KeepExisting =>
                {
                    run.keep(path, &fingerprints);
                    continue;
                }
                PublishAction::New => None,
                PublishAction::Update { replaces }
                | PublishAction::SettingsChanged { replaces } => Some(replaces),
            };
            uploads.push((index, fingerprints, replaces));
        }

        let indices: Vec<usize> = uploads.iter().map(|(index, ..)| *index).collect();
        let names = upload_file_names(self.pipeline, paths, &indices);
        let mut work = Vec::new();
        for ((index, fingerprints, replaces), name) in uploads.into_iter().zip(names) {
            let path = &paths[index];
            match name {
                Ok(file_name) => work.push(Work {
                    path: path.clone(),
                    fingerprints,
                    file_name,
                    replaces,
                    request_id: Uuid::new_v4(),
                }),
                Err(error) => run.fail(path, &error),
            }
        }
        work
    }

    /// Renders one chunk into its own directory inside the spool, then moves
    /// each output up into a spool slot. The directory keeps the export
    /// pipeline's `{sequence}` names from colliding with the previous chunk's
    /// files, which are still uploading.
    async fn render_chunk(
        &self,
        spool: &Spool,
        number: usize,
        chunk: Vec<Work>,
    ) -> Result<(Vec<Rendered>, Vec<(Work, PublishError)>), PublishError> {
        if chunk.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let dir = spool.path().join(format!("chunk-{number}"));
        std::fs::create_dir_all(&dir)
            .map_err(|e| PublishError::Io(format!("creating {}: {e}", dir.display())))?;
        let paths: Vec<String> = chunk.iter().map(|work| work.path.clone()).collect();

        let result = self.pipeline.render(&paths, &dir).await.map(|outputs| {
            let mut outputs = outputs.into_iter();
            let mut ready = Vec::new();
            let mut failed = Vec::new();
            for work in chunk {
                let Some(output) = outputs.next().flatten() else {
                    failed.push((
                        work,
                        PublishError::Rejected("the image could not be rendered".into()),
                    ));
                    continue;
                };
                // Prefixed with the request id: two photos may share a
                // destination file name, but never a spool slot.
                let file = spool.slot(&format!("{}-{}", work.request_id.simple(), work.file_name));
                match std::fs::rename(&output, &file) {
                    Ok(()) => ready.push(Rendered { work, file }),
                    Err(e) => failed.push((
                        work,
                        PublishError::Io(format!(
                            "moving {} into the spool: {e}",
                            output.display()
                        )),
                    )),
                }
            }
            (ready, failed)
        });

        if let Err(e) = std::fs::remove_dir_all(&dir) {
            log::warn!(
                "Failed to remove publish chunk directory {}: {e}",
                dir.display()
            );
        }
        result
    }

    /// Uploads with bounded concurrency, applying each outcome as it arrives.
    /// Ambiguous items keep their spooled file: reconciliation needs its size,
    /// and a re-queue needs its bytes.
    async fn upload_all(
        &self,
        spool: &Spool,
        container: &RemoteContainerId,
        items: Vec<Rendered>,
        run: &mut Run<'_>,
        ambiguous: &mut Vec<Rendered>,
    ) -> Result<(), PublishError> {
        let mime = self.pipeline.mime();
        let mut outcomes = stream::iter(items)
            .map(|rendered| async move {
                // Checked per image, so cancelling stops a chunk part-way.
                let outcome = if self.cancelled() {
                    Err(PublishError::Cancelled)
                } else {
                    self.destination
                        .publish_image(&rendered.item(container, mime), self.ctx)
                        .await
                };
                (rendered, outcome)
            })
            .buffer_unordered(UPLOAD_CONCURRENCY);

        while let Some((rendered, outcome)) = outcomes.next().await {
            match outcome {
                Ok(id) => {
                    // State first: a released file with no record would be
                    // uploaded again, as a duplicate, by the next publish.
                    run.succeed(&rendered.work, &id)?;
                    release(spool, &rendered.file);
                }
                Err(PublishError::Ambiguous { .. }) => ambiguous.push(rendered),
                Err(PublishError::Cancelled) => release(spool, &rendered.file),
                Err(error) => {
                    run.fail(&rendered.work.path, &error);
                    release(spool, &rendered.file);
                }
            }
        }
        Ok(())
    }

    /// Asks the destination what the ambiguous uploads left behind. What it
    /// found is recorded; what it did not is uploaded once more; whatever is
    /// still unresolved after that is reported rather than guessed at.
    async fn resolve(
        &self,
        spool: &Spool,
        container: &RemoteContainerId,
        ambiguous: Vec<Rendered>,
        run: &mut Run<'_>,
        capabilities: &crate::publish::DestinationCapabilities,
    ) -> Result<(), PublishError> {
        let found = if capabilities.supports_reconcile {
            let mime = self.pipeline.mime();
            let items: Vec<PublishItem<'_>> = ambiguous
                .iter()
                .map(|rendered| rendered.item(container, mime))
                .collect();
            match self
                .destination
                .reconcile(container, &items, self.ctx)
                .await
            {
                Ok(found) => Some(found),
                Err(error) => {
                    log::warn!("Reconciling the publish session failed: {error}");
                    None
                }
            }
        } else {
            None
        };

        // Without an answer, a re-upload is exactly the blind retry that
        // risks a duplicate.
        let Some(found) = found else {
            for rendered in &ambiguous {
                run.unresolved(&rendered.work);
            }
            return Ok(());
        };

        let mut requeue = Vec::new();
        for rendered in ambiguous {
            match found
                .iter()
                .find(|(name, _)| *name == rendered.work.file_name)
            {
                Some((_, id)) => {
                    run.succeed(&rendered.work, id)?;
                    release(spool, &rendered.file);
                }
                None => requeue.push(rendered),
            }
        }

        let mut still_ambiguous = Vec::new();
        self.upload_all(spool, container, requeue, run, &mut still_ambiguous)
            .await?;
        for rendered in &still_ambiguous {
            run.unresolved(&rendered.work);
        }
        Ok(())
    }
}

/// A photo that needs uploading.
struct Work {
    path: String,
    fingerprints: Fingerprints,
    file_name: String,
    replaces: Option<RemoteImageId>,
    /// One per photo for the whole session, re-queue included, so the
    /// destination can deduplicate.
    request_id: Uuid,
}

/// A photo rendered into the spool, waiting to upload.
struct Rendered {
    work: Work,
    file: PathBuf,
}

impl Rendered {
    fn item<'r>(&'r self, container: &'r RemoteContainerId, mime: &'static str) -> PublishItem<'r> {
        PublishItem {
            file: &self.file,
            file_name: self.work.file_name.clone(),
            mime,
            title: None,
            caption: None,
            keywords: Vec::new(),
            container,
            replaces: self.work.replaces.clone(),
            request_id: self.work.request_id,
        }
    }
}

/// The session's running tally, and the one place state is written and
/// progress is reported.
struct Run<'s> {
    state: PublishState,
    state_dir: &'s Path,
    /// The album being published, whose link every record goes under.
    album_id: String,
    events: &'s EventSink,
    summary: SessionSummary,
    completed: usize,
    total: usize,
}

impl Run<'_> {
    fn save(&self) -> Result<(), PublishError> {
        self.state.save_in(self.state_dir)
    }

    /// Stamps the link as published unless the session was cancelled, and
    /// saves: skipped records upgraded from v1 hashes are otherwise unwritten.
    fn finish(mut self) -> Result<SessionSummary, PublishError> {
        if !self.summary.cancelled {
            self.state.mark_published(&self.album_id);
        }
        self.save()?;
        Ok(self.summary)
    }

    fn report(&mut self, path: &str, state: ItemState) {
        self.completed += 1;
        (self.events)(SessionEvent::Progress(Progress {
            completed: self.completed,
            total: self.total,
            current_file: path.to_string(),
            state,
        }));
    }

    fn skip(&mut self, path: &str, fingerprints: &Fingerprints) {
        self.state
            .confirm_unchanged(&self.album_id, path, fingerprints);
        self.summary.skipped += 1;
        self.report(path, ItemState::Skipped);
    }

    /// A settings-only change the user chose not to upload: recorded as
    /// current with the new settings, and reported as a skip.
    fn keep(&mut self, path: &str, fingerprints: &Fingerprints) {
        self.state
            .mark_image_settings_current(&self.album_id, path, &fingerprints.settings_hash);
        self.summary.skipped += 1;
        self.report(path, ItemState::Skipped);
    }

    /// Saved on every success rather than at the end: a crash mid-session
    /// must not forget what already landed.
    fn succeed(&mut self, work: &Work, id: &RemoteImageId) -> Result<(), PublishError> {
        self.state
            .record_image(&self.album_id, &work.path, id, &work.fingerprints, None)?;
        self.save()?;
        let state = if work.replaces.is_some() {
            self.summary.updated += 1;
            ItemState::Updated
        } else {
            self.summary.uploaded += 1;
            ItemState::Uploaded
        };
        self.report(&work.path, state);
        Ok(())
    }

    fn fail(&mut self, path: &str, error: &PublishError) {
        log::warn!("Publishing {path} failed: {error}");
        self.summary.failed.push(FailedItem {
            path: path.to_string(),
            error: error.to_string(),
        });
        self.report(path, ItemState::Failed);
    }

    fn unresolved(&mut self, work: &Work) {
        self.summary.ambiguous.push(work.path.clone());
        self.report(&work.path, ItemState::Ambiguous);
    }
}

/// Logged rather than propagated: a file that will not delete is the spool's
/// `Drop` problem at the end of the session, not a reason to stop publishing.
fn release(spool: &Spool, file: &Path) {
    if let Err(error) = spool.release(file) {
        log::warn!("{error}");
    }
}

/// How often a render in progress checks for cancellation.
const CANCEL_POLL: Duration = Duration::from_millis(200);

/// The filename template the export pipeline falls back to, so a published
/// photo is named as the same photo exported would be.
const DEFAULT_FILENAME_TEMPLATE: &str = "{original_filename}_edited";

/// Renders through `export_images_impl`, unmodified, so watermarking,
/// metadata, GPS stripping, virtual copies and masks behave exactly as they
/// do for an export.
pub struct ExportPipeline {
    app_handle: tauri::AppHandle,
    export_settings: ExportSettings,
    output_format: String,
    cancel: Arc<AtomicBool>,
}

impl ExportPipeline {
    pub fn new(
        app_handle: tauri::AppHandle,
        export_settings: ExportSettings,
        output_format: String,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self {
            app_handle,
            export_settings,
            output_format,
            cancel,
        }
    }
}

#[async_trait]
impl RenderPipeline for ExportPipeline {
    fn mime(&self) -> &'static str {
        match self.output_format.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "tif" | "tiff" => "image/tiff",
            "webp" => "image/webp",
            "jxl" => "image/jxl",
            _ => "application/octet-stream",
        }
    }

    fn fingerprints(&self, virtual_path: &str) -> Result<Fingerprints, PublishError> {
        let (source, sidecar) = crate::file_management::parse_virtual_path(virtual_path);
        let unreadable = |e: std::io::Error| PublishError::Io(format!("{}: {e}", source.display()));
        let metadata = std::fs::metadata(&source).map_err(unreadable)?;
        let modified = metadata.modified().map_err(unreadable)?;
        let adjustments = crate::exif_processing::load_sidecar(&sidecar)
            .adjustments
            .to_string();
        let settings = RelevantExportSettings::from_export_settings(
            &self.export_settings,
            &self.output_format,
        );
        Ok(fingerprints(
            modified,
            metadata.len(),
            &adjustments,
            &settings,
        ))
    }

    /// Follows the user's export filename template, with the export
    /// pipeline's `_VCnn` suffix for a virtual copy.
    fn file_name(
        &self,
        virtual_path: &str,
        index: usize,
        total: usize,
    ) -> Result<String, PublishError> {
        let (source, _) = crate::file_management::parse_virtual_path(virtual_path);
        let template = self
            .export_settings
            .filename_template
            .as_deref()
            .unwrap_or(DEFAULT_FILENAME_TEMPLATE);
        let mut stem = crate::file_management::generate_filename_from_template(
            template,
            &source,
            index + 1,
            total,
            &crate::exif_processing::get_creation_date_from_path(&source),
        );
        if let Some((_, copy)) = virtual_path.rsplit_once("?vc=") {
            stem = format!("{stem}_VC{copy:0>2}");
        }
        Ok(format!(
            "{stem}.{}",
            self.output_format.to_ascii_lowercase()
        ))
    }

    async fn render(
        &self,
        virtual_paths: &[String],
        out_dir: &Path,
    ) -> Result<Vec<Option<PathBuf>>, PublishError> {
        // `{sequence}` into a flat, empty directory makes every output name
        // predictable from its input's position; the destination name comes
        // from `file_name` instead.
        let settings = ExportSettings {
            filename_template: Some("{sequence}".into()),
            preserve_folders: false,
            destination_type: None,
            subfolder: None,
            ..self.export_settings.clone()
        };

        let (completion_tx, mut completion_rx) = tokio::sync::oneshot::channel();
        crate::export_processing::export_images_impl(
            virtual_paths.to_vec(),
            out_dir.to_string_lossy().into_owned(),
            false,
            Vec::new(),
            settings,
            self.output_format.clone(),
            ExportAdjustmentsMode::UseSidecars {
                active_path: None,
                active_adjustments: None,
            },
            self.app_handle.state::<crate::AppState>(),
            self.app_handle.clone(),
            Some(completion_tx),
        )
        .await
        .map_err(PublishError::Rejected)?;

        // The export's own error count is not needed: which outputs exist
        // says, per image, what rendered.
        let mut cancel_requested = false;
        loop {
            tokio::select! {
                _ = &mut completion_rx => break,
                _ = tokio::time::sleep(CANCEL_POLL) => {
                    if !cancel_requested && self.cancel.load(Ordering::SeqCst) {
                        cancel_requested = true;
                        let _ = crate::export_processing::cancel_export(
                            self.app_handle.state::<crate::AppState>(),
                            self.app_handle.clone(),
                        );
                    }
                }
            }
        }

        let files: Vec<PathBuf> = std::fs::read_dir(out_dir)
            .map_err(|e| PublishError::Io(format!("reading {}: {e}", out_dir.display())))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        Ok((0..virtual_paths.len())
            .map(|index| sequenced_output(&files, index, virtual_paths.len()))
            .collect())
    }
}

/// Where a spooled render's output landed, for an export run with the
/// `{sequence}` filename template: `<seq>.<ext>`, or `<seq>_VC<nn>.<ext>` for a
/// virtual copy, with `<seq>` zero-padded to the width of `count`.
///
/// Matching the sequence prefix, rather than predicting the whole name, keeps
/// this independent of how the export pipeline suffixes virtual copies.
fn sequenced_output(files: &[PathBuf], index: usize, count: usize) -> Option<PathBuf> {
    let sequence = format!("{:0width$}", index + 1, width = count.to_string().len());
    files
        .iter()
        .find(|file| {
            file.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix(&sequence))
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('_'))
        })
        .cloned()
}

/// The names the photos at `uploads`, indices into `paths`, go by at the
/// destination when a session uploads them: `{sequence}` counts every photo in
/// the album, uploaded or not, and a name repeated among the uploads is made
/// unique. One entry per upload, in order.
///
/// Shared with adopting photos already in a remote album, which must predict
/// exactly the names a publish would send.
pub fn upload_file_names(
    pipeline: &dyn RenderPipeline,
    paths: &[String],
    uploads: &[usize],
) -> Vec<Result<String, PublishError>> {
    let mut names: Vec<Result<String, PublishError>> = uploads
        .iter()
        .map(|&index| pipeline.file_name(&paths[index], index, paths.len()))
        .collect();
    let mut named: Vec<String> = names.iter().flatten().cloned().collect();
    make_unique(&mut named);
    let mut unique = named.into_iter();
    for name in names.iter_mut().flatten() {
        *name = unique.next().expect("one unique name per name");
    }
    names
}

/// Suffixes repeated names `_1`, `_2`, … before the extension, as the export
/// pipeline does on disk: a filename template without `{sequence}` gives every
/// photo the same name, and reconciliation matches on names.
fn make_unique(names: &mut [String]) {
    let mut taken = HashSet::new();
    for name in names.iter_mut() {
        if taken.insert(name.clone()) {
            continue;
        }
        let (stem, extension) = match name.rsplit_once('.') {
            Some((stem, extension)) => (stem.to_string(), format!(".{extension}")),
            None => (name.clone(), String::new()),
        };
        let mut counter = 1;
        while !taken.insert(format!("{stem}_{counter}{extension}")) {
            counter += 1;
        }
        *name = format!("{stem}_{counter}{extension}");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    use tempfile::TempDir;

    use super::*;
    use crate::publish::{
        AuthChallenge, ContainerPrivacy, ContainerSnapshot, DestinationCapabilities, RemoteImage,
        RemoteNode, RemoteNodeId, state::PublishState,
    };

    const DESTINATION: &str = "stub";
    /// The album every test publishes unless it names another.
    const ALBUM: &str = "album-1";
    const FILE_BYTES: usize = 1000;

    /// Walks the spool base, so the measurement covers every session
    /// directory and any chunk directory inside one.
    fn footprint(base: &Path) -> u64 {
        walkdir::WalkDir::new(base)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| entry.file_name() != "session.json")
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum()
    }

    fn path(n: usize) -> String {
        format!("/photos/DSC_{n:04}.ARW")
    }

    fn file_name(n: usize) -> String {
        format!("DSC_{n:04}.jpg")
    }

    /// The remote album the stub finds or creates for a local one.
    fn album_uri(album_id: &str) -> String {
        format!("/api/v2/album/{album_id}")
    }

    /// Distinct per album, so a photo uploaded into two albums has two ids.
    fn image_id(album_id: &str, name: &str) -> RemoteImageId {
        RemoteImageId(format!("{}/image/{name}", album_uri(album_id)))
    }

    /// Renders a fixed-size file per image and counts what it was asked to do.
    struct StubPipeline {
        /// Photos whose edit hash differs from the default, to simulate an edit.
        edited: HashSet<String>,
        /// The settings hash every photo gets, to simulate a settings change.
        settings: String,
        renders: AtomicUsize,
        spool_base: PathBuf,
        peak: AtomicU64,
    }

    impl StubPipeline {
        fn new(spool_base: &Path) -> Self {
            Self {
                edited: HashSet::new(),
                settings: "settings:a".into(),
                renders: AtomicUsize::new(0),
                spool_base: spool_base.to_path_buf(),
                peak: AtomicU64::new(0),
            }
        }

        fn fingerprints_of(&self, virtual_path: &str) -> Fingerprints {
            let edit_hash = if self.edited.contains(virtual_path) {
                format!("edited:{virtual_path}")
            } else {
                format!("edit:{virtual_path}")
            };
            Fingerprints {
                legacy: format!("legacy:{edit_hash}:{}", self.settings),
                edit_hash,
                settings_hash: self.settings.clone(),
            }
        }
    }

    #[async_trait]
    impl RenderPipeline for StubPipeline {
        fn mime(&self) -> &'static str {
            "image/jpeg"
        }

        fn fingerprints(&self, virtual_path: &str) -> Result<Fingerprints, PublishError> {
            Ok(self.fingerprints_of(virtual_path))
        }

        fn file_name(
            &self,
            virtual_path: &str,
            _index: usize,
            _total: usize,
        ) -> Result<String, PublishError> {
            let stem = Path::new(virtual_path)
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap();
            Ok(format!("{stem}.jpg"))
        }

        async fn render(
            &self,
            virtual_paths: &[String],
            out_dir: &Path,
        ) -> Result<Vec<Option<PathBuf>>, PublishError> {
            self.renders
                .fetch_add(virtual_paths.len(), Ordering::SeqCst);
            let outputs = (0..virtual_paths.len())
                .map(|index| {
                    let file = out_dir.join(format!("{}.jpg", index + 1));
                    std::fs::write(&file, vec![0u8; FILE_BYTES]).unwrap();
                    Some(file)
                })
                .collect();
            self.peak
                .fetch_max(footprint(&self.spool_base), Ordering::SeqCst);
            Ok(outputs)
        }
    }

    enum Scripted {
        Fail,
        Ambiguous,
    }

    /// Records every call in one ordered log, and answers uploads as scripted
    /// per file name — success unless told otherwise.
    struct StubDestination {
        log: Mutex<Vec<String>>,
        uploads: Mutex<Vec<(String, Option<RemoteImageId>)>>,
        /// The remote album each upload went into, by file name.
        containers: Mutex<Vec<(String, String)>>,
        script: Mutex<HashMap<String, VecDeque<Scripted>>>,
        /// What reconcile reports as having landed.
        landed: HashSet<String>,
        /// Sets the cancel flag during the upload with this (1-based) number.
        cancel_on_upload: Option<(usize, Arc<AtomicBool>)>,
        spool_base: PathBuf,
        peak: AtomicU64,
        /// What `auth_status` reports: `None` is not connected.
        account: Option<String>,
        supports_reconcile: bool,
        container_exists: bool,
    }

    impl StubDestination {
        fn new(spool_base: &Path) -> Self {
            Self {
                account: Some("stub".into()),
                log: Mutex::new(Vec::new()),
                uploads: Mutex::new(Vec::new()),
                containers: Mutex::new(Vec::new()),
                script: Mutex::new(HashMap::new()),
                landed: HashSet::new(),
                cancel_on_upload: None,
                spool_base: spool_base.to_path_buf(),
                peak: AtomicU64::new(0),
                supports_reconcile: false,
                container_exists: true,
            }
        }

        fn connected_as(mut self, account: Option<&str>) -> Self {
            self.account = account.map(str::to_string);
            self
        }

        fn script(self, name: &str, outcomes: Vec<Scripted>) -> Self {
            self.script
                .lock()
                .unwrap()
                .insert(name.to_string(), outcomes.into());
            self
        }

        fn with_reconcile(mut self) -> Self {
            self.supports_reconcile = true;
            self
        }

        fn with_missing_container(mut self) -> Self {
            self.supports_reconcile = true;
            self.container_exists = false;
            self
        }

        fn uploaded_names(&self) -> Vec<String> {
            self.uploads
                .lock()
                .unwrap()
                .iter()
                .map(|(name, _)| name.clone())
                .collect()
        }
    }

    #[async_trait]
    impl PublishDestination for StubDestination {
        fn id(&self) -> &'static str {
            DESTINATION
        }

        fn display_name(&self) -> &'static str {
            "Stub"
        }

        fn capabilities(&self) -> DestinationCapabilities {
            DestinationCapabilities {
                supports_replace: true,
                supports_reconcile: self.supports_reconcile,
                supports_nested_containers: false,
                max_bytes: None,
                accepted_mime_types: &["image/jpeg"],
                supported_privacy: &[ContainerPrivacy::Public],
            }
        }

        async fn auth_status(&self, _ctx: &PublishContext) -> Result<AuthStatus, PublishError> {
            Ok(match &self.account {
                Some(account) => AuthStatus::Connected {
                    account: account.clone(),
                },
                None => AuthStatus::NotAuthorised,
            })
        }

        async fn begin_auth(&self, _ctx: &PublishContext) -> Result<AuthChallenge, PublishError> {
            Err(PublishError::Rejected("the stub needs no auth".into()))
        }

        async fn complete_auth(
            &self,
            _verifier: &str,
            _ctx: &PublishContext,
        ) -> Result<(), PublishError> {
            Err(PublishError::Rejected("the stub needs no auth".into()))
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
            _name: &str,
            _ctx: &PublishContext,
        ) -> Result<Option<RemoteNode>, PublishError> {
            unimplemented!()
        }

        async fn create_container(
            &self,
            _name: &str,
            _ctx: &PublishContext,
        ) -> Result<RemoteNode, PublishError> {
            unimplemented!()
        }

        async fn container(
            &self,
            _id: &RemoteContainerId,
            _ctx: &PublishContext,
        ) -> Result<RemoteNode, PublishError> {
            unimplemented!()
        }

        async fn inspect_container(
            &self,
            _container: &RemoteContainerId,
            _ctx: &PublishContext,
        ) -> Result<Option<ContainerSnapshot>, PublishError> {
            if !self.container_exists {
                return Ok(None);
            }
            Ok(Some(ContainerSnapshot {
                name: "Album".into(),
                web_url: None,
                images: Vec::new(),
            }))
        }

        async fn list_container_images(
            &self,
            _container: &RemoteContainerId,
            _ctx: &PublishContext,
        ) -> Result<Vec<RemoteImage>, PublishError> {
            unimplemented!()
        }

        async fn publish_image(
            &self,
            item: &PublishItem<'_>,
            ctx: &PublishContext,
        ) -> Result<RemoteImageId, PublishError> {
            assert!(
                item.file.is_file(),
                "uploads read a spooled file that exists"
            );
            self.peak
                .fetch_max(footprint(&self.spool_base), Ordering::SeqCst);
            self.log
                .lock()
                .unwrap()
                .push(format!("upload:{}", item.file_name));
            let count = {
                let mut uploads = self.uploads.lock().unwrap();
                uploads.push((item.file_name.clone(), item.replaces.clone()));
                uploads.len()
            };
            self.containers
                .lock()
                .unwrap()
                .push((item.file_name.clone(), item.container.0.clone()));
            if let Some((at, flag)) = &self.cancel_on_upload {
                assert!(Arc::ptr_eq(flag, &ctx.cancel));
                if count == *at {
                    flag.store(true, Ordering::SeqCst);
                }
            }

            let scripted = self
                .script
                .lock()
                .unwrap()
                .get_mut(&item.file_name)
                .and_then(VecDeque::pop_front);
            match scripted {
                Some(Scripted::Fail) => Err(PublishError::Rejected("scripted failure".into())),
                Some(Scripted::Ambiguous) => Err(PublishError::Ambiguous {
                    file_name: item.file_name.clone(),
                }),
                None => Ok(RemoteImageId(format!(
                    "{}/image/{}",
                    item.container.0, item.file_name
                ))),
            }
        }

        async fn reconcile(
            &self,
            container: &RemoteContainerId,
            expected: &[PublishItem<'_>],
            _ctx: &PublishContext,
        ) -> Result<Vec<(String, RemoteImageId)>, PublishError> {
            assert_eq!(container.0, album_uri(ALBUM));
            let names: Vec<&str> = expected.iter().map(|i| i.file_name.as_str()).collect();
            self.log
                .lock()
                .unwrap()
                .push(format!("reconcile:{}", names.join(",")));
            Ok(expected
                .iter()
                .filter(|item| self.landed.contains(&item.file_name))
                .map(|item| (item.file_name.clone(), image_id(ALBUM, &item.file_name)))
                .collect())
        }
    }

    struct Harness {
        dir: TempDir,
        ctx: PublishContext,
        events: Arc<Mutex<Vec<SessionEvent>>>,
    }

    impl Harness {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let ctx = PublishContext {
                state_dir: dir.path().join("state"),
                consumer: None,
                cancel: Arc::new(AtomicBool::new(false)),
                new_container_privacy: ContainerPrivacy::Public,
            };
            Self {
                dir,
                ctx,
                events: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn spool_base(&self) -> PathBuf {
            self.dir.path().join("spool")
        }

        fn state(&self) -> PublishState {
            PublishState::load_in(
                &self.ctx.state_dir,
                DESTINATION,
                &AlbumMembership::default(),
            )
            .unwrap()
        }

        fn state_file(&self) -> PathBuf {
            self.ctx.state_dir.join(format!("{DESTINATION}.json"))
        }

        /// Records `paths` as already published into [`ALBUM`] with the
        /// stub's fingerprints.
        fn published(&self, pipeline: &StubPipeline, paths: &[String]) {
            let mut state = self.state();
            state.record_link(ALBUM, &RemoteContainerId(album_uri(ALBUM)), None);
            for path in paths {
                let name = format!(
                    "{}.jpg",
                    Path::new(path).file_stem().unwrap().to_str().unwrap()
                );
                state
                    .record_image(
                        ALBUM,
                        path,
                        &image_id(ALBUM, &name),
                        &pipeline.fingerprints_of(path),
                        None,
                    )
                    .unwrap();
            }
            state.save_in(&self.ctx.state_dir).unwrap();
        }

        async fn run(
            &self,
            destination: &StubDestination,
            pipeline: &StubPipeline,
            paths: Vec<String>,
        ) -> SessionSummary {
            self.run_request(destination, pipeline, request(ALBUM, paths))
                .await
        }

        async fn run_request(
            &self,
            destination: &StubDestination,
            pipeline: &StubPipeline,
            request: PublishRequest,
        ) -> SessionSummary {
            self.run_with(
                destination,
                pipeline,
                request,
                SettingsChangePolicy::Republish,
            )
            .await
        }

        /// Links `album_id` to its stub remote album, as the panel does
        /// before anything can be published, unless it is linked already.
        fn link(&self, album_id: &str) {
            let mut state = self.state();
            if state.link(album_id).is_none() {
                state.record_link(album_id, &RemoteContainerId(album_uri(album_id)), None);
                state.save_in(&self.ctx.state_dir).unwrap();
            }
        }

        async fn run_with(
            &self,
            destination: &StubDestination,
            pipeline: &StubPipeline,
            request: PublishRequest,
            on_settings_change: SettingsChangePolicy,
        ) -> SessionSummary {
            self.link(&request.album.album_id);
            let recorder = Arc::clone(&self.events);
            let events: EventSink = Arc::new(move |event| recorder.lock().unwrap().push(event));
            PublishSession::new(destination, pipeline, &self.ctx, self.spool_base(), events)
                .run(request, on_settings_change)
                .await
                .unwrap()
        }

        fn terminal_event(&self) -> &'static str {
            self.events.lock().unwrap().last().unwrap().name()
        }
    }

    fn paths(range: std::ops::RangeInclusive<usize>) -> Vec<String> {
        range.map(path).collect()
    }

    fn request(album_id: &str, paths: Vec<String>) -> PublishRequest {
        PublishRequest {
            album: LocalContainer {
                album_id: album_id.into(),
                name: format!("Album {album_id}"),
                parent_path: vec![],
            },
            paths,
            albums: AlbumMembership::default(),
        }
    }

    #[tokio::test]
    async fn publishing_into_another_account_is_refused_before_anything_is_touched() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base()).connected_as(Some("bob"));
        let mut state = harness.state();
        state.record_link(ALBUM, &RemoteContainerId(album_uri(ALBUM)), None);
        state.claim_account("alice").unwrap();
        state.save_in(&harness.ctx.state_dir).unwrap();
        let before = std::fs::read(harness.state_file()).unwrap();

        let recorder = Arc::clone(&harness.events);
        let events: EventSink = Arc::new(move |event| recorder.lock().unwrap().push(event));
        let result = PublishSession::new(
            &destination,
            &pipeline,
            &harness.ctx,
            harness.spool_base(),
            events,
        )
        .run(
            request(ALBUM, paths(1..=2)),
            SettingsChangePolicy::Republish,
        )
        .await;

        let error = result.expect_err("bob must not publish into alice's albums");
        assert!(matches!(error, PublishError::NotAuthorised(_)), "{error}");
        assert_eq!(harness.terminal_event(), "publish-error");
        assert!(
            destination.log.lock().unwrap().is_empty(),
            "no album was looked up and nothing was uploaded"
        );
        assert_eq!(std::fs::read(harness.state_file()).unwrap(), before);
    }

    #[tokio::test]
    async fn the_first_publish_records_the_connected_account() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base()).connected_as(Some("alice"));

        harness.run(&destination, &pipeline, paths(1..=1)).await;

        assert_eq!(harness.state().account(), Some("alice"));
    }

    #[tokio::test]
    async fn the_account_check_passes_when_not_connected_and_refuses_another_account() {
        let harness = Harness::new();
        let mut state = harness.state();
        state.record_link(ALBUM, &RemoteContainerId(album_uri(ALBUM)), None);
        state.claim_account("alice").unwrap();

        let disconnected = StubDestination::new(&harness.spool_base()).connected_as(None);
        check_account(&disconnected, &harness.ctx, &mut state)
            .await
            .expect("a preview needs no connection");

        let bob = StubDestination::new(&harness.spool_base()).connected_as(Some("bob"));
        assert!(check_account(&bob, &harness.ctx, &mut state).await.is_err());
    }

    #[test]
    fn preview_counts_what_a_publish_would_do_without_rendering() {
        let harness = Harness::new();
        let mut pipeline = StubPipeline::new(&harness.spool_base());
        harness.published(&pipeline, &paths(1..=3));
        pipeline.edited = [path(2)].into();

        let counts = preview(&pipeline, &harness.state(), ALBUM, &paths(1..=5));

        assert_eq!(
            counts,
            PublishPreview {
                new: 2,
                update: 1,
                settings_changed: 0,
                skip: 2,
                unreadable: 0,
            }
        );
        assert_eq!(pipeline.renders.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_album_is_found_anywhere_in_the_tree_with_its_groups() {
        let album = |id: &str, name: &str| AlbumItem::Album {
            id: id.into(),
            name: name.into(),
            icon: None,
            images: vec![format!("/photos/{name}.ARW")],
        };
        let tree = vec![
            album("top", "Top"),
            AlbumItem::Group {
                id: "g1".into(),
                name: "Travel".into(),
                icon: None,
                children: vec![
                    AlbumItem::Group {
                        id: "g2".into(),
                        name: "Empty".into(),
                        icon: None,
                        children: vec![],
                    },
                    AlbumItem::Group {
                        id: "g3".into(),
                        name: "2026".into(),
                        icon: None,
                        children: vec![album("ice", "Iceland")],
                    },
                ],
            },
        ];

        let found = PublishRequest::from_album_tree(&tree, "ice").unwrap();
        assert_eq!(found.album.name, "Iceland");
        assert_eq!(found.album.parent_path, vec!["Travel", "2026"]);
        assert_eq!(found.paths, vec!["/photos/Iceland.ARW"]);

        let top = PublishRequest::from_album_tree(&tree, "top").unwrap();
        assert!(top.album.parent_path.is_empty());

        assert!(
            PublishRequest::from_album_tree(&tree, "g1").is_none(),
            "a group is not an album"
        );
        assert!(PublishRequest::from_album_tree(&tree, "nope").is_none());
    }

    #[tokio::test]
    async fn unchanged_images_are_skipped_without_rendering() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());
        harness.published(&pipeline, &paths(1..=5));

        let summary = harness.run(&destination, &pipeline, paths(1..=5)).await;

        assert_eq!(
            pipeline.renders.load(Ordering::SeqCst),
            0,
            "a skip must cost no render"
        );
        assert!(destination.uploaded_names().is_empty());
        assert_eq!(summary.skipped, 5);
        assert_eq!(harness.terminal_event(), "publish-complete");
        assert!(
            harness
                .state()
                .link(ALBUM)
                .unwrap()
                .last_published
                .is_some(),
            "a completed session stamps the link"
        );
    }

    #[tokio::test]
    async fn changed_images_are_uploaded_with_replaces_set() {
        let harness = Harness::new();
        let mut pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());
        harness.published(&pipeline, &paths(1..=3));
        pipeline.edited = [path(1), path(2)].into();

        let summary = harness.run(&destination, &pipeline, paths(1..=4)).await;

        let mut uploads = destination.uploads.lock().unwrap().clone();
        uploads.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            uploads,
            vec![
                (file_name(1), Some(image_id(ALBUM, &file_name(1)))),
                (file_name(2), Some(image_id(ALBUM, &file_name(2)))),
                (file_name(4), None),
            ],
            "edits replace in place, a new photo is added, an unchanged one is left alone"
        );
        assert_eq!(pipeline.renders.load(Ordering::SeqCst), 3);
        assert_eq!(
            (summary.updated, summary.uploaded, summary.skipped),
            (2, 1, 1)
        );
        let state = harness.state();
        assert_eq!(
            state.classify(ALBUM, &path(1), &pipeline.fingerprints_of(&path(1))),
            PublishAction::Skip,
            "the new fingerprint is recorded"
        );
    }

    #[test]
    fn preview_counts_settings_changes_apart_from_edits() {
        let harness = Harness::new();
        let mut pipeline = StubPipeline::new(&harness.spool_base());
        harness.published(&pipeline, &paths(1..=3));
        pipeline.settings = "settings:b".into();
        pipeline.edited = [path(1)].into();

        let counts = preview(&pipeline, &harness.state(), ALBUM, &paths(1..=4));

        assert_eq!(
            counts,
            PublishPreview {
                new: 1,
                update: 1,
                settings_changed: 2,
                skip: 0,
                unreadable: 0,
            },
            "an edit with new settings is an edit"
        );
    }

    #[tokio::test]
    async fn republish_uploads_photos_whose_settings_changed() {
        let harness = Harness::new();
        let mut pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());
        harness.published(&pipeline, &paths(1..=2));
        pipeline.settings = "settings:b".into();

        let summary = harness
            .run_with(
                &destination,
                &pipeline,
                request(ALBUM, paths(1..=2)),
                SettingsChangePolicy::Republish,
            )
            .await;

        let mut uploads = destination.uploads.lock().unwrap().clone();
        uploads.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            uploads,
            vec![
                (file_name(1), Some(image_id(ALBUM, &file_name(1)))),
                (file_name(2), Some(image_id(ALBUM, &file_name(2)))),
            ],
            "replaced in place, like an edit"
        );
        assert_eq!(summary.updated, 2);
        assert_eq!(
            harness
                .state()
                .classify(ALBUM, &path(1), &pipeline.fingerprints_of(&path(1))),
            PublishAction::Skip,
            "the new settings hash is recorded"
        );
    }

    #[tokio::test]
    async fn keeping_existing_uploads_publishes_only_edits_and_new_photos() {
        let harness = Harness::new();
        let mut pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());
        harness.published(&pipeline, &paths(1..=3));
        pipeline.settings = "settings:b".into();
        pipeline.edited = [path(1)].into();

        let summary = harness
            .run_with(
                &destination,
                &pipeline,
                request(ALBUM, paths(1..=4)),
                SettingsChangePolicy::KeepExisting,
            )
            .await;

        let mut uploads = destination.uploads.lock().unwrap().clone();
        uploads.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            uploads,
            vec![
                (file_name(1), Some(image_id(ALBUM, &file_name(1)))),
                (file_name(4), None),
            ],
            "the edit and the new photo upload; the settings-only changes do not"
        );
        assert_eq!(
            pipeline.renders.load(Ordering::SeqCst),
            2,
            "a kept upload costs no render"
        );
        assert_eq!(
            (summary.updated, summary.uploaded, summary.skipped),
            (1, 1, 2)
        );
        let reported: Vec<ItemState> = harness
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Progress(progress) if progress.current_file == path(2) => {
                    Some(progress.state)
                }
                _ => None,
            })
            .collect();
        assert_eq!(reported, [ItemState::Skipped]);

        let state = harness.state();
        for n in 1..=4 {
            assert_eq!(
                state.classify(ALBUM, &path(n), &pipeline.fingerprints_of(&path(n))),
                PublishAction::Skip,
                "photo {n} is current with the new settings"
            );
        }
        assert_eq!(
            state.image_for(ALBUM, &path(2)).unwrap().remote_uri,
            image_id(ALBUM, &file_name(2)).0,
            "a kept upload keeps its remote image"
        );
    }

    #[tokio::test]
    async fn a_photo_shared_by_two_albums_is_uploaded_into_each() {
        let harness = Harness::new();
        let mut pipeline = StubPipeline::new(&harness.spool_base());
        let first = StubDestination::new(&harness.spool_base());
        harness.run(&first, &pipeline, paths(1..=2)).await;

        let second = StubDestination::new(&harness.spool_base());
        let summary = harness
            .run_request(&second, &pipeline, request("album-2", paths(2..=3)))
            .await;

        let mut uploads = second.uploads.lock().unwrap().clone();
        uploads.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            uploads,
            vec![(file_name(2), None), (file_name(3), None)],
            "the shared photo is new to the second album, not skipped or replaced"
        );
        assert!(
            second
                .containers
                .lock()
                .unwrap()
                .iter()
                .all(|(_, container)| *container == album_uri("album-2"))
        );
        assert_eq!(summary.uploaded, 2);
        let state = harness.state();
        assert_eq!(
            state.image_for(ALBUM, &path(2)).unwrap().remote_uri,
            image_id(ALBUM, &file_name(2)).0,
            "the first album's record is untouched"
        );
        assert_eq!(
            state.image_for("album-2", &path(2)).unwrap().remote_uri,
            image_id("album-2", &file_name(2)).0
        );

        pipeline.edited = [path(2)].into();
        let third = StubDestination::new(&harness.spool_base());
        harness
            .run_request(&third, &pipeline, request("album-2", paths(2..=3)))
            .await;
        assert_eq!(
            third.uploads.lock().unwrap().clone(),
            vec![(file_name(2), Some(image_id("album-2", &file_name(2))))],
            "an edit replaces the copy in the album being published"
        );
    }

    /// A remote album the user chose, whose URI is not what finding one by
    /// name would produce.
    const CHOSEN: &str = "/api/v2/album/ChosenOnSmugMug";

    #[tokio::test]
    async fn a_linked_album_publishes_into_its_link_without_a_lookup() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());
        let mut state = harness.state();
        state.record_link(ALBUM, &RemoteContainerId(CHOSEN.into()), None);
        state.save_in(&harness.ctx.state_dir).unwrap();

        let summary = harness.run(&destination, &pipeline, paths(1..=2)).await;

        assert_eq!(summary.uploaded, 2);
        assert!(
            destination
                .containers
                .lock()
                .unwrap()
                .iter()
                .all(|(_, container)| container == CHOSEN)
        );
        assert_eq!(harness.state().link(ALBUM).unwrap().remote_uri, CHOSEN);
        assert!(harness.state().image_for(ALBUM, &path(1)).is_some());
    }

    #[tokio::test]
    async fn an_unlinked_album_is_refused_before_anything_is_touched() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());

        let recorder = Arc::clone(&harness.events);
        let events: EventSink = Arc::new(move |event| recorder.lock().unwrap().push(event));
        let result = PublishSession::new(
            &destination,
            &pipeline,
            &harness.ctx,
            harness.spool_base(),
            events,
        )
        .run(
            request(ALBUM, paths(1..=2)),
            SettingsChangePolicy::Republish,
        )
        .await;

        let error = result.expect_err("publishing requires a link");
        assert!(matches!(error, PublishError::Rejected(_)), "{error}");
        assert_eq!(harness.terminal_event(), "publish-error");
        assert!(
            destination.log.lock().unwrap().is_empty(),
            "no album is found or created by name, and nothing uploads"
        );
        assert!(harness.state().link(ALBUM).is_none());
    }

    #[tokio::test]
    async fn pre_publish_refresh_stops_on_a_deleted_container_before_rendering() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base()).with_missing_container();
        harness.link(ALBUM);

        let recorder = Arc::clone(&harness.events);
        let events: EventSink = Arc::new(move |event| recorder.lock().unwrap().push(event));
        let result = PublishSession::new(
            &destination,
            &pipeline,
            &harness.ctx,
            harness.spool_base(),
            events,
        )
        .run(
            request(ALBUM, paths(1..=2)),
            SettingsChangePolicy::Republish,
        )
        .await;

        let error = result.expect_err("a deleted remote album stops the session");
        assert!(matches!(error, PublishError::Rejected(_)), "{error}");
        assert!(harness.state().link(ALBUM).unwrap().broken);
        assert_eq!(pipeline.renders.load(Ordering::SeqCst), 0);
        assert!(destination.uploaded_names().is_empty());
        assert_eq!(harness.terminal_event(), "publish-error");
    }

    #[tokio::test]
    async fn a_broken_link_is_not_published_into() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());
        std::fs::create_dir_all(&harness.ctx.state_dir).unwrap();
        let mut state = harness.state();
        state.record_link(ALBUM, &RemoteContainerId(CHOSEN.into()), None);
        state.save_in(&harness.ctx.state_dir).unwrap();
        let mut written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(harness.state_file()).unwrap()).unwrap();
        written["links"][ALBUM]["broken"] = true.into();
        std::fs::write(harness.state_file(), written.to_string()).unwrap();

        let recorder = Arc::clone(&harness.events);
        let events: EventSink = Arc::new(move |event| recorder.lock().unwrap().push(event));
        let result = PublishSession::new(
            &destination,
            &pipeline,
            &harness.ctx,
            harness.spool_base(),
            events,
        )
        .run(
            request(ALBUM, paths(1..=2)),
            SettingsChangePolicy::Republish,
        )
        .await;

        assert!(result.is_err());
        assert!(
            destination.log.lock().unwrap().is_empty(),
            "nothing is created in its place and nothing uploads"
        );
    }

    #[tokio::test]
    async fn a_v1_state_file_republishes_nothing_that_is_unchanged() {
        let harness = Harness::new();
        let mut pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());
        let unchanged = pipeline.fingerprints_of(&path(1)).legacy;
        pipeline.edited = [path(2)].into();
        std::fs::create_dir_all(&harness.ctx.state_dir).unwrap();
        let v1_image = |n: usize| {
            serde_json::json!({
                "remote_uri": image_id(ALBUM, &file_name(n)).0,
                "web_url": null,
                "fingerprint": unchanged.replace(&path(1), &path(n)),
                "last_published": "2026-09-01T10:00:00+00:00"
            })
        };
        let v1 = serde_json::json!({
            "version": 1,
            "destination": DESTINATION,
            "account": "stub",
            "containers": {
                ALBUM: {
                    "remote_uri": album_uri(ALBUM),
                    "web_url": null,
                    "last_published": "2026-09-01T10:00:00+00:00"
                }
            },
            "images": { path(1): v1_image(1), path(2): v1_image(2) }
        });
        std::fs::write(harness.state_file(), v1.to_string()).unwrap();
        let tree = vec![AlbumItem::Album {
            id: ALBUM.into(),
            name: "Iceland".into(),
            icon: None,
            images: paths(1..=2),
        }];
        let request = PublishRequest::from_album_tree(&tree, ALBUM).unwrap();

        let summary = harness.run_request(&destination, &pipeline, request).await;

        assert_eq!(
            destination.uploads.lock().unwrap().clone(),
            vec![(file_name(2), Some(image_id(ALBUM, &file_name(2))))],
            "only the edited photo uploads, replacing its migrated record"
        );
        assert_eq!((summary.skipped, summary.updated), (1, 1));
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(harness.state_file()).unwrap()).unwrap();
        assert_eq!(written["version"], 2);
        let state = harness.state();
        let upgraded = state.image_for(ALBUM, &path(1)).unwrap();
        assert_eq!(
            upgraded.legacy_fingerprint, None,
            "the skipped record upgrades"
        );
        assert_eq!(
            upgraded.edit_hash.as_deref(),
            Some(pipeline.fingerprints_of(&path(1)).edit_hash.as_str())
        );
    }

    #[tokio::test]
    async fn a_single_image_failure_does_not_abort_the_batch() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination =
            StubDestination::new(&harness.spool_base()).script(&file_name(3), vec![Scripted::Fail]);

        let summary = harness.run(&destination, &pipeline, paths(1..=5)).await;

        let uploaded = destination.uploaded_names();
        assert!(uploaded.contains(&file_name(4)) && uploaded.contains(&file_name(5)));
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.failed[0].path, path(3));
        let state = harness.state();
        let recorded: Vec<bool> = (1..=5)
            .map(|n| state.image_for(ALBUM, &path(n)).is_some())
            .collect();
        assert_eq!(recorded, [true, true, false, true, true]);
        assert_eq!(harness.terminal_event(), "publish-complete");
    }

    #[tokio::test]
    async fn ambiguous_items_trigger_reconcile_at_session_end() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let mut destination = StubDestination::new(&harness.spool_base())
            .with_reconcile()
            .script(&file_name(2), vec![Scripted::Ambiguous])
            .script(&file_name(4), vec![Scripted::Ambiguous]);
        destination.landed = [file_name(2)].into();

        let summary = harness.run(&destination, &pipeline, paths(1..=5)).await;

        let log = destination.log.lock().unwrap().clone();
        let reconciles: Vec<usize> = log
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.starts_with("reconcile:"))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            reconciles.len(),
            1,
            "one reconcile for the session: {log:?}"
        );
        let reconcile_at = reconciles[0];
        let mut reconciled: Vec<&str> =
            log[reconcile_at]["reconcile:".len()..].split(',').collect();
        reconciled.sort();
        assert_eq!(reconciled, [file_name(2), file_name(4)]);
        for n in 1..=5 {
            let first_upload = log
                .iter()
                .position(|entry| *entry == format!("upload:{}", file_name(n)))
                .unwrap();
            assert!(
                first_upload < reconcile_at,
                "reconcile waits for every upload: {log:?}"
            );
        }
        assert_eq!(
            log[reconcile_at + 1..],
            [format!("upload:{}", file_name(4))],
            "what did not land is uploaded again; what did is not"
        );
        let state = harness.state();
        assert!((1..=5).all(|n| state.image_for(ALBUM, &path(n)).is_some()));
        assert!(summary.ambiguous.is_empty());
    }

    #[tokio::test]
    async fn only_confirmed_successes_are_written_to_state() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base())
            .with_reconcile()
            .script(&file_name(1), vec![Scripted::Fail])
            .script(
                &file_name(2),
                vec![Scripted::Ambiguous, Scripted::Ambiguous],
            );

        let summary = harness.run(&destination, &pipeline, paths(1..=3)).await;

        let state = harness.state();
        assert!(
            state.image_for(ALBUM, &path(1)).is_none(),
            "a failure is not recorded"
        );
        assert!(
            state.image_for(ALBUM, &path(2)).is_none(),
            "an ambiguity reconcile could not confirm is not recorded"
        );
        assert!(state.image_for(ALBUM, &path(3)).is_some());
        assert_eq!(summary.ambiguous, [path(2)]);
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.uploaded, 1);
    }

    #[tokio::test]
    async fn cancellation_stops_work_and_cleans_the_spool() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let mut destination = StubDestination::new(&harness.spool_base());
        destination.cancel_on_upload = Some((3, Arc::clone(&harness.ctx.cancel)));

        let summary = harness.run(&destination, &pipeline, paths(1..=40)).await;

        assert!(summary.cancelled);
        assert_eq!(
            destination.uploaded_names().len(),
            3,
            "cancellation is honoured within a chunk, not only between chunks"
        );
        assert!(
            pipeline.renders.load(Ordering::SeqCst) <= 2 * RENDER_CHUNK_SIZE,
            "no chunk starts rendering after cancellation"
        );
        let leftovers: Vec<_> = std::fs::read_dir(harness.spool_base())
            .map(|entries| entries.filter_map(Result::ok).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "the spool is removed: {leftovers:?}");
        let state = harness.state();
        assert_eq!(
            (1..=40)
                .filter(|n| state.image_for(ALBUM, &path(*n)).is_some())
                .count(),
            3
        );
        assert_eq!(harness.terminal_event(), "publish-cancelled");
    }

    #[tokio::test]
    async fn the_spool_footprint_stays_bounded_across_a_long_run() {
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());

        let summary = harness.run(&destination, &pipeline, paths(1..=100)).await;

        assert_eq!(summary.uploaded, 100);
        let bound = (2 * RENDER_CHUNK_SIZE * FILE_BYTES) as u64;
        let peak = pipeline
            .peak
            .load(Ordering::SeqCst)
            .max(destination.peak.load(Ordering::SeqCst));
        assert!(peak > 0, "the footprint was measured");
        assert!(
            peak <= bound,
            "peak {peak} bytes exceeded two chunks ({bound})"
        );
    }

    /// A Tauri async command must return a `Send` future, and the command
    /// that starts a session will await this one.
    #[test]
    fn a_session_can_run_inside_a_tauri_command() {
        fn assert_send<T: Send>(_: &T) {}
        let harness = Harness::new();
        let pipeline = StubPipeline::new(&harness.spool_base());
        let destination = StubDestination::new(&harness.spool_base());
        let events: EventSink = Arc::new(|_| {});
        let session = PublishSession::new(
            &destination,
            &pipeline,
            &harness.ctx,
            harness.spool_base(),
            events,
        );
        let run = session.run(request(ALBUM, vec![]), SettingsChangePolicy::Republish);
        assert_send(&run);
    }

    #[test]
    fn a_sequenced_output_is_found_by_its_padded_prefix() {
        let files: Vec<PathBuf> = ["01.jpg", "02_VC01.jpg", "10.jpg"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            sequenced_output(&files, 0, 10),
            Some(PathBuf::from("01.jpg"))
        );
        assert_eq!(
            sequenced_output(&files, 1, 10),
            Some(PathBuf::from("02_VC01.jpg"))
        );
        assert_eq!(
            sequenced_output(&files, 9, 10),
            Some(PathBuf::from("10.jpg"))
        );
        assert_eq!(
            sequenced_output(&files, 2, 10),
            None,
            "a failed render has no output"
        );
    }

    #[test]
    fn repeated_file_names_are_made_unique() {
        let mut names = vec![
            "photo.jpg".to_string(),
            "photo.jpg".into(),
            "photo.jpg".into(),
        ];
        make_unique(&mut names);
        assert_eq!(names, ["photo.jpg", "photo_1.jpg", "photo_2.jpg"]);
    }

    /// Names every photo by its position, as `{sequence}` does, or all alike,
    /// as a template without one does.
    struct TemplatePipeline {
        sequence: bool,
    }

    #[async_trait]
    impl RenderPipeline for TemplatePipeline {
        fn mime(&self) -> &'static str {
            "image/jpeg"
        }

        fn fingerprints(&self, _virtual_path: &str) -> Result<Fingerprints, PublishError> {
            unimplemented!()
        }

        fn file_name(
            &self,
            _virtual_path: &str,
            index: usize,
            total: usize,
        ) -> Result<String, PublishError> {
            Ok(if self.sequence {
                format!("Trip-{}of{total}.jpg", index + 1)
            } else {
                "Trip.jpg".into()
            })
        }

        async fn render(
            &self,
            _virtual_paths: &[String],
            _out_dir: &Path,
        ) -> Result<Vec<Option<PathBuf>>, PublishError> {
            unimplemented!()
        }
    }

    #[test]
    fn upload_names_count_every_photo_and_repeats_among_the_uploads_are_made_unique() {
        let names = |pipeline: &TemplatePipeline| -> Vec<String> {
            upload_file_names(pipeline, &paths(1..=4), &[1, 3])
                .into_iter()
                .map(Result::unwrap)
                .collect()
        };

        assert_eq!(
            names(&TemplatePipeline { sequence: true }),
            ["Trip-2of4.jpg", "Trip-4of4.jpg"],
            "a skipped photo still takes its place in the sequence"
        );
        assert_eq!(
            names(&TemplatePipeline { sequence: false }),
            ["Trip.jpg", "Trip_1.jpg"]
        );
    }
}
