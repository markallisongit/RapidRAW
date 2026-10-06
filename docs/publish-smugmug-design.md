# RapidRAW → SmugMug publishing

**Design document** · 2026-09-12, updated 2026-10-06 · Mark Allison (with Claude)
Target: [CyberTimon/RapidRAW](https://github.com/CyberTimon/RapidRAW) · Fork: [markallisongit/RapidRAW](https://github.com/markallisongit/RapidRAW) · Branch: `feat/publish-destinations-smugmug`

**Implementation:** phase 1 tracked in [#13](https://github.com/markallisongit/RapidRAW/issues/13) (issues [#1](https://github.com/markallisongit/RapidRAW/issues/1)–[#12](https://github.com/markallisongit/RapidRAW/issues/12)); the Publish Manager batch in [#14](https://github.com/markallisongit/RapidRAW/issues/14) (issues [#15](https://github.com/markallisongit/RapidRAW/issues/15)–[#29](https://github.com/markallisongit/RapidRAW/issues/29) and [#34](https://github.com/markallisongit/RapidRAW/issues/34)). Each issue carries its own task detail; this document is the rationale only.

**Status:** phase 1 complete, tested end-to-end against a live SmugMug account on 2026-09-13. The Publish Manager batch is built; its full manual walk passed on Linux on 2026-10-06 ([#28](https://github.com/markallisongit/RapidRAW/issues/28)), and the Windows walk ([#29](https://github.com/markallisongit/RapidRAW/issues/29)) is what remains. This document describes the code as it is.

## Summary

Publish RapidRAW albums to SmugMug from inside the app — no manual export step, no user-visible
intermediate files. Built on a general `PublishDestination` trait so further destinations (Flickr,
S3, Immich) are cheap. Images spool to a managed temp dir, upload, and are deleted.

**In scope:** one-action publish; republish replaces rather than duplicates; unchanged photos
skipped; full reuse of the export pipeline; resumable; guaranteed cleanup; no secrets in the binary.

**Out:** delete sync, comment/rating pull, pushing renames to the destination, `Group` tree
mirroring, video, downloading destination-only photos, several accounts per destination, other
destinations.

## Decisions

| #   | Question      | Decision                                                                                           |
| --- | ------------- | -------------------------------------------------------------------------------------------------- |
| 1   | Sync model    | Publish, phased. Remote-ID map + skip-unchanged in phase 1                                         |
| 2   | Mapping       | One RapidRAW `Album` → one SmugMug album. Phase 1 found or created it by name; superseded by 13    |
| 3   | Credentials   | User supplies their own API key/secret. Nothing embedded                                           |
| 4   | Token storage | OS keyring (`keyring` 4.2). Explicit error where unavailable, no plaintext fallback                |
| 5   | OAuth         | Hand-rolled `oauth1` module over `hmac`/`sha1` (~150 lines)                                        |
| 6   | Callback      | Out-of-band verifier code. No loopback listener                                                    |
| 7   | Byte path     | Spool to managed temp dir, upload from disk, delete on success                                     |
| 8   | Pipeline      | Call existing `export_images_impl` unmodified with a temp output folder                            |
| 9   | Idempotency   | Stable `X-Smug-UploadRequestId` per image + post-timeout reconciliation                            |

The Publish Manager batch (#14, decided 2026-09-13) added:

| #   | Question                 | Decision                                                                                                                                                  |
| --- | ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 10  | Where publishing lives   | A native dockable panel tab, `Panel.Publish`, beside Metadata and Export — not a button by the bottom bar. A few more upstream lines, built as upstream would |
| 11  | Where set-up lives       | A **Publish Manager** modal opened from the panel. Not a Settings category (`SettingsPanel.tsx` is large and busy upstream), not in-panel pages (350 px is too narrow for two panes) |
| 12  | Output settings          | Each destination uses one existing **export preset**, never the Export panel's current values. Changing it asks: republish affected photos, or keep the uploads |
| 13  | Album mapping            | **Explicit links**, like Lightroom's published collections: link to a remote album that exists (including one made outside RapidRAW), or create one. Nothing is found or created by name at publish time |
| 14  | Sync                     | Linking plus a read-only **refresh** from the destination. No download of destination-only photos, no comment/rating pull, no delete sync               |
| 15  | New-album privacy        | A destination setting, default **Public**, matching SmugMug's own Lightroom plugin (`SmNode.create` sets `Privacy = PUBLIC` when none is chosen; plugin 3.5.19.1 bytecode). Stated before an album is created, never applied to one that is linked |
| 16  | Right-click publish      | "Publish to ▸" on albums in Sources                                                                                                                       |
| 17  | Deleting a linked album  | Nothing is deleted remotely. A persistent notice names the remote album left behind, in the destination's own terms, with its URL                        |
| 18  | Several accounts         | Not built. One configuration per destination type                                                                                                         |

Rationale for the contentious ones. **User keys (3):** an embedded consumer secret in an
open-source desktop binary is trivially extractable and puts CyberTimon on the hook for every
user's rate limits. **Spool (7):** retry becomes a re-read rather than a GPU re-run or a RAM
hostage; it decouples encode rate from uplink rate; it makes resume possible; memory stays bounded
regardless of album size. One write and one read of a 5–25 MB file is noise against the RAW decode
that produced it. **Pipeline reuse (8):** zero changes to a 1,759-line actively-edited file — the
fork's largest conflict risk — and watermarking, metadata, GPS stripping, filename templates,
virtual copies and mask handling all work for free, with no drift, because there is one path.

Rejected: the [`smugmug` crate](https://docs.rs/smugmug) has no upload support, which is the part
we need.

## SmugMug API v2 — the traps

- **OAuth 1.0a only.** HMAC-SHA1 over `METHOD&encoded_url&encoded_sorted_params`.
- Percent-encoding is RFC 3986 _unreserved only_ (`-._~` survive, uppercase hex). Rust's URL
  helpers do not match this — hand-rolled and table-tested.
- **Uploads accept OAuth parameters only in the `Authorization` header.** The canonical failure:
  clients defaulting to query-string params work for ordinary API calls and fail opaquely on every
  upload.
- Uploads go to a different host, `POST https://upload.smugmug.com/`, and **the file is the raw
  request body** — not multipart, not form-encoded. So the body contributes no signature
  parameters; `Content-MD5` protects it.
- **`X-Smug-ImageUri` replaces** an existing image instead of creating one. This is what makes
  non-duplicating republish possible.

Undocumented headers, observed in SmugMug's own Lightroom plugin: `X-Smug-UploadRequestId` (stable
per-upload id — with `X-Smug-RetryCount`, lets the server deduplicate retries) and `X-Smug-Version`
are adopted; `X-Smug-AssetUri` is not, as nothing in phase 1 needs it. The same plugin logs _"Album Upload
timeout previously, search for files that might have uploaded successfully"_ — even SmugMug's
client reconciles rather than blindly retrying. It also sets per-upload timeouts from measured
bandwidth, and runs uploads on a pool separate from rendering.

## Architecture

**Pipeline.** `export_images_impl` (`export_processing.rs`) is already `pub(crate)` and takes a
completion channel; `run_headless_export` is a working template for calling it outside
the command layer. A publish session does the same with a temp output folder.

**Chunking.** Process in chunks of 8 — render chunk _n+1_ while uploading chunk _n_, so the GPU and
the network overlap and the spool stays bounded at ~16 images (~320 MB at 45 MP). Tuning constant.

**Spool.** `app_cache_dir/publish-spool/<session_uuid>/` — cache, not data: regenerable, and the OS
already treats it as disposable. Write a `session.json` marker with pid and start time; delete each
file _immediately_ on upload success so the footprint tracks outstanding work; remove the directory
via a `Drop` guard on every exit path, mirroring `ExportTaskGuard`. `Drop` can't run
after SIGKILL, so also sweep at startup for sessions >24 h old or with a dead pid. Never surfaced
to the user.

**Concurrency.** The export loop sizes its pool from cores and free RAM (clamped 1–4) —
a GPU heuristic, wrong for network I/O. Publishing uses a separate semaphore, default 3.

**Retry.** Exponential backoff with jitter on 5xx and 429, honouring `Retry-After`, max 4 attempts,
same request id throughout. Timeouts are derived from measured throughput and file size — fixed
values are wrong when sizes vary 20× and uplinks 100×. Cancellation reuses the `AtomicBool` pattern
from `cancel_export`, and an upload in flight is raced against it, so cancelling a stalled upload
does not wait out its timeout; one abandoned that way reports as unconfirmed.

**A dead connection stops the session.** Without that, every remaining photo would spend its full
retry schedule (about 35 s each) failing. Once `OFFLINE_STREAK` uploads in a row (one per upload
slot) fail on the transport or go unconfirmed, the session stops and reports the rest as not
tried. Publishing again picks up where it left off.

**Ambiguous failures.** A timeout may or may not have committed; blind retry risks a duplicate,
giving up risks a missing photo. Retry with the same request id; if exhausted, mark _ambiguous_,
not failed; at session end (after a cancel too) call `reconcile()` to list the album and match by
filename and size, and record what's found. What isn't found is reported as unconfirmed and never
re-sent in the same session: a listing can lag behind a fresh upload. Per-image failures never
abort the batch, and the state file records only confirmed successes.

**Upload journal.** Every upload is journaled as `pending` in the state file, and saved, before it
is sent (once per chunk; a failed save stops the session first). A confirmed upload's record
replaces its entry; one known not to have landed (cancelled before sending, refused) drops it; an
ambiguous one keeps it. Whatever is left (a lost response, a kill, a failed save) is settled the
next time the album is listed, by the pre-publish refresh or ↻: an unclaimed image with the same
file name and size is recorded with the fingerprints the upload was rendered with (the oldest, if
several match), and the rest are forgotten. Rendering is deterministic, so name and size identify
an upload RapidRAW made. A settled photo is reported as uploaded, and an edit made since still
replaces it. A destination that cannot list discards its journal.

**Refresh is read-only towards the destination.** `refresh_destination` (`session.rs`) asks
`inspect_container` for each link's current name and images and applies the snapshot to the state
(`PublishState::apply_snapshot`): a renamed remote album updates the link's `remote_name`; a
missing one marks the link `broken` and keeps its image records in case it comes back; a recorded
image no longer in a live album is forgotten, so the next publish uploads that photo afresh instead
of replacing an id that is gone. It never renames, recreates, deletes or uploads anything. The same
refresh, for one link, runs at the start of every publish, so a publish never replaces into a
deleted image and never renders for a deleted album: a broken link stops it before the spool
exists. Pressing ↻ runs it for every link.

**Photos a remote album already holds** (`links.rs`, #26 and #27). Linking to an album that was
filled by hand, or by an earlier install, would otherwise upload everything again. The matcher
pairs local photos with remote images, strongest evidence first: the exact name publishing would
give the photo; the source file's stem inside the remote name; the same capture time on the same
camera model; and a perceptual hash of the thumbnails (`image_hasher` double-gradient, as
`culling.rs` uses; edited-photo pairs measured 0–4 bits apart, different photos 36 or more). Each
photo and each remote image is in at most one pair, and a tie pairs nothing. Only exact names are
adopted unseen. Every other pair is shown side by side for the user to tick, because a wrong pair
would make the next edit overwrite a different photo. An adopted image is recorded with the
current fingerprints, as if just published.

### The trait

```rust
#[async_trait]
pub trait PublishDestination: Send + Sync {
    fn id(&self) -> &'static str;               // "smugmug"
    fn display_name(&self) -> &'static str;
    fn capabilities(&self) -> DestinationCapabilities;

    async fn auth_status(&self, ctx: &PublishContext) -> Result<AuthStatus, PublishError>;
    async fn begin_auth(&self, ctx: &PublishContext) -> Result<AuthChallenge, PublishError>;
    async fn complete_auth(&self, verifier: &str, ctx: &PublishContext) -> Result<(), PublishError>;
    /// Forgets the access token only: consumer key, state and links survive.
    async fn disconnect(&self, ctx: &PublishContext) -> Result<(), PublishError>;

    /// The remote album tree, a level at a time, for linking to an album that exists.
    async fn list_containers(&self, parent: Option<&RemoteNodeId>, ctx: &PublishContext)
        -> Result<Vec<RemoteNode>, PublishError>;
    /// Linking to a new album: find reports a same-named one, so create never reuses it.
    async fn find_container(&self, name: &str, ctx: &PublishContext)
        -> Result<Option<RemoteNode>, PublishError>;
    /// With `ctx.new_container_privacy`.
    async fn create_container(&self, name: &str, ctx: &PublishContext)
        -> Result<RemoteNode, PublishError>;
    async fn container(&self, id: &RemoteContainerId, ctx: &PublishContext)
        -> Result<RemoteNode, PublishError>;

    /// Read-only, for refresh: name and images now, or `None` when it is gone.
    async fn inspect_container(&self, id: &RemoteContainerId, ctx: &PublishContext)
        -> Result<Option<ContainerSnapshot>, PublishError>;
    /// Read-only, for adopting photos a linked album already holds.
    async fn list_container_images(&self, id: &RemoteContainerId, ctx: &PublishContext)
        -> Result<Vec<RemoteImage>, PublishError>;
    /// Thumbnail bytes for the review of possible pairs. Default: none.
    async fn fetch_thumbnail(&self, image: &RemoteImage, ctx: &PublishContext)
        -> Result<Option<Vec<u8>>, PublishError>;
    /// Compares ids across listings; SmugMug strips the revision suffix (`-0`, `-1`).
    fn image_identity(&self, image: &RemoteImageId) -> String;

    async fn publish_image(&self, item: &PublishItem<'_>, ctx: &PublishContext)
        -> Result<RemoteImageId, PublishError>;

    /// After an ambiguous failure, ask the destination what actually landed.
    async fn reconcile(&self, container: &RemoteContainerId, expected: &[PublishItem<'_>],
        ctx: &PublishContext) -> Result<Vec<(String, RemoteImageId)>, PublishError>;
}

pub struct PublishItem<'a> {
    pub file: &'a Path,              // in the spool; deleted after success
    pub file_name: String,
    pub mime: &'static str,
    pub title: Option<String>,
    pub caption: Option<String>,
    pub keywords: Vec<String>,
    pub container: &'a RemoteContainerId,
    pub replaces: Option<RemoteImageId>,  // Some → X-Smug-ImageUri, replace in place
    pub request_id: Uuid,                 // X-Smug-UploadRequestId, stable across retries
}
```

`publish_image` takes a **path**, not bytes — SmugMug reads the file once, hashes it for
`Content-MD5` and sends that buffer, which is its business. It buffers rather than streams: the
project's `reqwest` has no `stream` feature, and three concurrent 5–25 MB buffers are cheap.
`DestinationCapabilities` (`supports_replace`, `supports_reconcile`, `supports_nested_containers`,
`max_bytes`, `accepted_mime_types`, `supported_privacy`) means the session never special-cases on
`id()`. `supported_privacy` lists the `ContainerPrivacy` levels (`Public`, `Unlisted`, `Private`;
destination-neutral, each destination maps them onto its own terms) the manager offers for new
albums. `async-trait` is required: `dyn` async traits aren't
object-safe without boxing on Rust 1.98.

The registry is a plain `Vec<Arc<dyn PublishDestination>>` with an explicit constructor, not
`inventory`/`linkme` link-time magic — one obvious registration site, and a stable one-line seam
for the fork.

### Publish state

`app_data_dir/publish/<destination_id>.json`, mirroring `albums/albums.json`
(`file_management.rs`). Written atomically (temp + rename) after each success, so an
interrupted publish resumes.

```jsonc
{
  "version": 2,
  "destination": "smugmug",
  "account": "markallison",
  "links": {
    "<album_id>": {
      "remote_uri": "/api/v2/album/AbCdEf",
      "remote_name": null,                 // null when unknown
      "web_url": "…",
      "linked_at": "2026-09-12T10:14:02Z",
      "last_published": "…",               // null until the first publish
      "broken": false,                     // the remote album is gone
      "images": {
        "<virtual_path>": {
          "remote_uri": "/api/v2/image/XyZ123-0",
          "web_url": "…",
          "edit_hash": "b3:…",
          "settings_hash": "b3:…",
          "last_published": "…",
        },
      },
      "pending": [                         // omitted when empty
        {
          "path": "<virtual_path>",
          "file_name": "DSC_0001.jpg",     // exactly as sent
          "size_bytes": 1322434,           // the rendered file
          "edit_hash": "b3:…",             // what it was rendered with
          "settings_hash": "b3:…",
        },
      ],
    },
  },
}
```

**Settings live in a separate file**, `app_data_dir/publish/<destination_id>.settings.json`
(`settings.rs`): the export preset id and the privacy of new albums, versioned on their own.
Settings are a few bytes the user edits and state is large and machine-written, so neither
rewrites the other. They stay out of `AppSettings` too, since only publishing reads them and that
struct and its frontend mirror are busy upstream files. A missing file means the defaults, but an
unreadable one is an error: quietly falling back to Public would publish more widely than chosen.

`account` is `null` until the first publish, and `web_url` is `null` where SmugMug returns none.
The account is read and written on its own (`PublishState::account_in` / `set_account_in`), so
connecting never loads — or migrates — the image records.

**Image records live under their link.** A photo in two RapidRAW albums is two uploads, one into
each remote album, each replaced or skipped on its own. Relinking an album to a *different*
remote album drops its records, which describe images in the old album. Version 1 keyed image
records on the virtual path alone, account-wide: a photo in two albums was skipped for the second
album's gallery as already published, or, once edited, replaced the copy in the first gallery.
Explicit linking would have made that common, which is why v2 nests the records (#16). A remote
album belongs to at most one link, so two RapidRAW albums can never publish into the same one.

**Pending uploads live under their link too**, so unlinking, or relinking elsewhere, drops them,
and a link whose album has gone clears them. `pending` is omitted when empty, so adding it needed
no version bump: a clean publish writes the same file as before. It lives here rather than in the
image sidecar, which would drop an unknown field on the next edit and would carry account-specific
ids into files that get synced and shared.

**Key on the full virtual path**, not the source path — virtual copies use a `vc=` suffix
(parsed in `export_images_impl`, which names them `_VCnn`) and are distinct publishable photos.

**The fingerprint is Lightroom's `metadataThatTriggersRepublish`, split in two:**
`edit_hash = blake3(source_mtime, source_size, adjustments_json)` and
`settings_hash = blake3(relevant_export_settings)` — `blake3` is already a dependency, fields are
length-prefixed, JSON is canonicalised. Both match ⇒ skip _before_ rendering, so an unchanged
album costs one file read and no GPU work. The split gives four outcomes: `New`, `Update` (edit
changed), `SettingsChanged` (only settings changed) and `Skip`. A publish takes a required
`SettingsChangePolicy`: `Republish` uploads `SettingsChanged` photos like `Update`, `KeepExisting`
marks them current without rendering. The settings come from the destination's export preset
(`preset.rs`), never the Export panel's current values. `relevant_export_settings`
(`state.rs`) leaves out `destination_type`, `subfolder`, `preserve_folders` and
`filename_template`, which decide where a file lands, not what is in it — otherwise renaming the
output template would re-upload the whole library. Border and padding, added to presets upstream
after phase 1, count as settings but are left out of the hash while unset, so records made before
they existed keep their hash rather than all turning "affected by settings".

**Migration from version 1** (phase 1's `containers` plus path-keyed `images`) runs on load and is
written as version 2 by the next save. Each container becomes a link. A v1 image record is copied
into every migrated link whose RapidRAW album contains that path — so loading takes the album tree
as input — and dropped, with a logged count, when it is in none. v1 records keep their combined
fingerprint as `legacy_fingerprint`, with both split hashes `null`; they are compared against the
v1 fingerprint of the current inputs (match ⇒ `Skip`, and the record takes split hashes; otherwise
`Update`). An unknown future version is rejected, never reset.

## Module layout

```
src-tauri/src/publish/
  mod.rs               PublishDestination trait, re-exports
  types.rs             PublishContext, PublishItem, PublishError and friends
  registry.rs          destination list, single running-session slot
  commands.rs          Tauri commands, startup spool sweep
  session.rs           chunked render→upload driver, progress events, cancellation
  spool.rs             temp dir lifecycle, Drop guard, startup sweep
  state.rs             links, per-link remote ID map, split fingerprints, pending uploads,
                       snapshots from refresh, v1 migration
  settings.rs          <id>.settings.json: export preset id, new-album privacy
  preset.rs            the destination's export preset → ExportSettings for the pipeline
  links.rs             link/unlink, list links, match and adopt photos already in an album
  oauth1.rs            OAuth 1.0a signing — generic, no SmugMug specifics
  credential_store.rs  keyring access for consumer keys and tokens
  smugmug/             mod.rs · auth.rs (OAuth flow) · api.rs (album tree, lookup, create, listing)
                       upload.rs (raw-body POST, retry, cancel, reconcile) · model.rs

src/components/panel/right/publish/
  PublishPanel.tsx     tab shell: link flow, confirmations, the settings-change question
  DestinationSection.tsx · LinkedAlbumRow.tsx (status, row actions)
  LinkAlbumFlow.tsx · RemoteAlbumBrowser.tsx · ExistingReview.tsx (pairs to adopt)
  PublishSummary.tsx · PublishProgress.tsx
  albumContextMenu.ts (Sources "Publish to ▸") · deletedAlbumNotice.tsx · publishRequests.ts
  output.ts (preset summary, format checks) · usePublishState.ts · publish.i18n.ts
  manager/             PublishManagerModal.tsx (portal, destination list, Save/Cancel)
                       SmugMugAccountSection.tsx · OutputSection.tsx · NewAlbumsSection.tsx
                       ManagerSection.tsx
```

## OAuth flow

```
1. User registers at api.smugmug.com/api/developer/apply → key + secret, pasted into settings
2. POST .../services/oauth/1.0a/getRequestToken   oauth_callback=oob
3. Open browser → .../authorize?oauth_token=…&Access=Full&Permissions=Add
4. SmugMug shows a six-digit verifier; user pastes it back
5. POST .../getAccessToken   oauth_verifier=<pasted>
6. Store in keyring: service "io.github.CyberTimon.RapidRAW", account "smugmug:<nickname>"
```

`Access=Full&Permissions=Add` is the minimum permitting album creation and upload — not `Modify` or
`Delete`. Least privilege, calmer consent screen.

## Upload request

```
POST https://upload.smugmug.com/
Authorization:           OAuth oauth_consumer_key="…", oauth_signature="…", …
Content-Length:          <file size>
Content-MD5:             <base64(md5(file))>
Content-Type:            image/jpeg
X-Smug-ResponseType:     JSON
X-Smug-Pretty:           false
X-Smug-AlbumUri:         /api/v2/album/AbCdEf
X-Smug-FileName:         DSC_1234_edited.jpg
X-Smug-Title:            …
X-Smug-Caption:          …                         (optional)
X-Smug-Keywords:         …                         (optional)
X-Smug-ImageUri:         /api/v2/image/XyZ123-0    (only when replacing)
X-Smug-UploadRequestId:  <uuid, stable across retries>
X-Smug-RetryCount:       <0, 1, 2, …>

<raw file bytes, read from the spool>
```

## Frontend

A separate **Publish** panel — not a third entry in the export destination dropdown. Publishing has
state exporting doesn't (auth, album choice, "12 unchanged, 3 to update, 1 new"), and hiding that
behind a select value makes the panel change shape drastically on one value. `destination_type`
(`ExportSettings`) stays for filesystem destinations.

Publish is a tab in RapidRAW's dockable panel system, `Panel.Publish`, after Export in the left
dock. It drags between regions and persists in the saved workspace like every other panel, works
in both the library and the editor, and reaches existing workspaces through `reconcileWorkspace`,
which appends panels a saved layout lacks. It is filtered out on Android, which has no `keyring`
backend.

A region mounts only its active tab, so the panel unmounts whenever another tab is chosen. The
backend reports a session only through events and cannot be asked about one afterwards, so
`usePublishState` keeps session and auth state, including a pending authorisation, in a
module-level zustand store with listeners registered once for the app's lifetime. This is the
same reason `useTetheringStore` exists.

**The Publish Manager** (`manager/PublishManagerModal.tsx`) holds everything needed to set a
destination up, in the shape of Lightroom Classic's Publishing Manager: destinations on the left
with their status, collapsible sections on the right, Save and Cancel. It is a modal opened from
the panel's ⚙, not a Settings category or in-panel pages (decision 11). Edits are a draft until
Save; Escape or Cancel discards them, and focus returns to ⚙. Saving a different export preset
first asks `publish_settings_impact` how many uploads used different settings, then asks whether to
republish them next time or keep the existing uploads (`publish_keep_existing_uploads`).

```
┌ Publish Manager ──────────────────────────────────────────────┐
│ SmugMug          │ ▾ Account      Connected as markallison     │
│  markallison ●   │                [Reconnect] [Disconnect]     │
│                  │ ▾ API key      •••••••• [Change]            │
│                  │ ▾ Output       Export preset [Web 2560 ▾]   │
│                  │                JPEG · q85 · 2560 px long    │
│                  │ ▾ New galleries Privacy [Public ▾]          │
│                  │                Applies only to galleries    │
│                  │                RapidRAW creates.            │
│                  │                          [Cancel] [Save]    │
└──────────────────┴──────────────────────────────────────────────┘
```

Account connects, reconnects and disconnects; disconnecting keeps the key, links and history. API
key links to SmugMug's developer page and says credentials live in the OS keyring. Output picks the
preset, summarises it in a line, warns when its format is one the destination refuses, and links
to the Export panel when there are no presets. New galleries sets the privacy of albums RapidRAW
creates, from the destination's `supported_privacy`.

**The panel** is built around **linked albums**, Lightroom's published collections. It prompts for
set-up when the destination is not connected or has no usable output preset, and publishing is
disabled until both are in place. Each link shows a status from `publish_preview`, checked one
album at a time while the tab is visible and again after a publish or an album change: up to date,
N changed, N new, N affected by settings, not found on the destination, not published yet. The row
for the album selected in Sources is highlighted. Right-click or the row's menu publishes, opens
the remote album, checks for photos already in it, links to a different one, creates a broken one
again or unlinks, each change confirmed. ↻ refreshes every link from the destination. A link whose
RapidRAW album was deleted can never be published again (album ids are never reused), so it is not
listed: one line under the list counts such links and removes them, deleting nothing remotely.

```
┌ Publish ──────────────────── ↻  ⚙ ┐
│ ▾ SmugMug · markallison          ● │
│    Landscapes 2026     ✓ Up to date│
│    Iceland             ↻ 4 changed │
│    Portfolio           ○ 12 new    │
│    Old Trip            ⚠ Not found │
│    + Publish an album…             │
│────────────────────────────────────│
│ Iceland → SmugMug "Iceland 2026"   │
│ 4 to update · 38 unchanged         │
│ Preset: Web 2560 · q85             │
│             [ Publish 4 ]          │
└────────────────────────────────────┘
```

"Publish an album…" links first and uploads nothing: choose a RapidRAW album, then create a new
remote album (its privacy stated before it is created, and an existing same-named album offered
instead) or browse to one that exists (`RemoteAlbumBrowser.tsx`, folders and a breadcrumb; albums
already linked are shown disabled with the album they belong to). Linking to an album that holds
photos runs the matcher: exact pairs are adopted, and anything less is offered for review in
`ExistingReview.tsx`, each local photo beside its remote match with the reasons. Publishing always
requires a link and the destination's preset; nothing is found or created by name, and nothing
falls back to the Export panel's settings. A publish whose album has settings-only changes asks
first — republish them too, or only upload edited and new photos. Progress, per-image status
including failed and ambiguous, and cancel follow. The spool is never surfaced.

**From elsewhere in the app.** Right-clicking an album in Sources offers "Publish to ▸", which
either publishes a linked album now or opens the link flow on an unlinked one
(`albumContextMenu.ts`). It sends the request through `publishRequests.ts`, so the panel asks the
same questions however a publish starts. Deleting an album, or a group holding albums, that was
linked raises a persistent notice (`deletedAlbumNotice.tsx`) naming each remote album left behind,
with a link to it (decision 17).

**The destination's own words.** RapidRAW's albums are always *albums*; a destination's containers
use its own term, SmugMug's being *gallery*. Strings that name a remote container have an i18next
`context: destinationId` variant in `publish.i18n.ts` (`heading_smugmug: 'New galleries'`), so a
second destination gets its own words without touching the components.

## Integration surface

Against upstream `main` at `1cc99d56` (2026-10-06), from `scripts/check-fork-surface.sh main`:

| File                                     | Change                                                                                                       | Lines    | Conflict risk                  |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------ | -------- | ------------------------------ |
| `src-tauri/Cargo.toml`                   | deps `async-trait`, `hmac`, `sha1`, `md-5`, `bytes`, `keyring`; dev-deps `tokio`, `wiremock` (with comments) | +21      | Low                            |
| `src-tauri/src/lib.rs`                   | `mod publish;` + spool sweep in setup + registry init                                                        | +3       | Very low                       |
| `src-tauri/src/lib.rs`                   | 22 `publish_*` commands in `generate_handler![]`, between markers                                            | +24      | **Medium — both sides append** |
| `src-tauri/src/app_state.rs`             | `publish_registry` field                                                                                     | +1       | Low                            |
| `src/App.tsx`                            | imports + `registerPublishResources()`; `case Panel.Publish` in `renderAppPanel`                             | +6       | Medium — busy file             |
| `src/components/ui/AppProperties.tsx`    | `Panel.Publish` enum member                                                                                  | +1       | Low                            |
| `src/components/panel/PanelSwitcher.tsx` | `Send` icon and tooltip key in `PANEL_ICONS` / `PANEL_TITLES`                                                | +3       | Low                            |
| `src/store/useUIStore.ts`                | `Panel.Publish` in `ALL_PANELS`, default regions and both default layouts; Android gating (`isPublishSupported` in the `allowedPanels` filter) | +25 −3   | Low                            |
| `src/hooks/useAppContextMenus.ts`        | import; `Publish to ▸` options for albums; deleted-link notice after an album delete                         | +3       | Low                            |
| `i18next.config.ts`                      | `extract.ignore` for the publish directory                                                                   | +3       | Low                            |
| `src/@types/i18next.d.ts`                | `PublishTranslations` in the type augmentation                                                               | +4 −1    | Low                            |
| **`src-tauri/src/export_processing.rs`** | **none**                                                                                                     | **0**    | **None**                       |
| `src/i18n/**`                            | **none**                                                                                                     | 0        | None                           |

**94 lines added and 4 removed, across 10 existing files** (`scripts/check-fork-surface.sh` prints
the live figure; `Cargo.lock` follows `Cargo.toml` and is not counted). The whole branch is about
20,900 lines across 50 files; everything outside the table is new, and new files never conflict.

**i18n with zero locale edits:** `registerPublishResources()` in `publish.i18n.ts`, called once from
`App.tsx`, runs `i18n.addResourceBundle(lang, 'translation', { publish: {…} }, true, true)` instead
of editing thirteen locale JSONs — thirteen conflict sites per merge. Runtime registration alone
fails CI's `i18next-cli extract --ci`, so `i18next.config.ts` ignores the publish directory, and
`PublishTranslations` joins the `i18next.d.ts` augmentation so keys stay type-checked.

**`generate_handler!`** is the one guaranteed recurring conflict: Tauri permits one
`invoke_handler` and both sides append. Chosen: the 22 `publish_*` commands in one contiguous
block between `// --- publish ---` and `// --- end publish ---`, one mechanical hunk per merge that
`rerere` learns. Fallback if that
proves painful: a single `publish_invoke(action, payload)` dispatch. Reversible.

## Fork maintenance

```
upstream/main ──► main                       mirror only; fast-forward, never commit
                    │ merge weekly
                    ▼
      feat/publish-destinations-smugmug      integration branch; what gets built and run
          ▲         ▲         ▲
     tweak/15   tweak/16   tweak/17          one per issue; merged locally, then deleted
```

**`main` mirrors upstream.** It only ever fast-forwards
(`git fetch upstream main:main && git push origin main`, which never checks `main` out)
and nothing of ours is committed to it, so it stays a clean base for upstream PRs and for the
surface guard. Avoid switching branches in the working tree while `tauri dev` runs: the watchers
rebuild against the swapped source and the app crashes. Use `git worktree add` when another
branch's files are needed.

**`feat/publish-destinations-smugmug` is the long-lived integration branch.** `main` is **merged**
in, never rebased onto, because merges preserve the conflict resolutions `rerere` (enabled)
replays. Merge weekly, not on demand: small frequent merges are far cheaper.

**Follow-up work** is one GitHub issue per change and one short-lived branch per issue
(`tweak/<issue>-<slug>`), cut from the integration branch and merged back locally, fast-forward
where possible. There are no PRs within the fork, and issues are closed by hand after merging.
Stack branches only where one change genuinely depends on another; independent changes on
independent branches can be dropped or reworked alone.

**`pr/publish-destinations`** is built only when going upstream: regenerated from the integration
branch, rebased onto `upstream/main`, never merged back. Conflating it with the integration branch
is why long-lived forks become painful. Two commits ordered by independence — trait/registry/spool,
then the SmugMug destination — so if upstream takes only the reusable half the fork's delta shrinks
to one commit.

`scripts/check-fork-surface.sh [base]` fails if the diff against `upstream/main` (or `main`, which
mirrors it) touches anything outside the table above; surface creep is invisible otherwise. Run it
before merging any follow-up branch.

For the PR, the only one this work produces: open a discussion issue first, lead with the integration surface, frame it as
infrastructure with SmugMug as the reference implementation, and offer to maintain it.

## Testing

RFC 5849 §1.2 vectors and SmugMug's [example signing code](https://gist.github.com/smugmug-api-docs/10046914)
for `oauth1.rs`, with a table-driven percent-encoding test. Property test for fingerprint
stability. Round-trip, migration and interrupted-write tests for the state file. Guard-runs-on-
every-exit-path and sweep tests for the spool. `wiremock` fixtures for `api.rs`/`upload.rs` and the
retry scenarios (500-then-success, 429 with `Retry-After`, timeout-then-found,
timeout-then-absent, a stalled link, a dead connection) — no live network in CI. Tests for refresh
snapshots, pending-upload settlement and the photo matcher. Manual end-to-end against a real
account: phase 1 on 2026-09-13, and the full Publish Manager walk on Linux on 2026-10-06
([#28](https://github.com/markallisongit/RapidRAW/issues/28)) and on Windows
([#29](https://github.com/markallisongit/RapidRAW/issues/29)). The project
had no Rust tests before this; `publish` founds the harness (`[dev-dependencies]`, `#[cfg(test)]`
modules) rather than extending one. The export pipeline is untouched, not "still tested".

## Phasing

1. Trait, registry, spool, state, OAuth, SmugMug upload, panel — the working feature. **Done.**
   - **Publish Manager batch** (#14): publishing as a native panel tab, the Publish Manager,
     explicit links with state v2, output from an export preset, new-album privacy, read-only
     refresh, adopting photos a gallery already holds, the Sources "Publish to ▸" menu, notices for
     deleted albums, and never uploading twice after an interrupted publish. **Done.**
2. `Group` → folder mirroring, titles/captions/keywords from metadata
   ([#32](https://github.com/markallisongit/RapidRAW/issues/32)).
3. Delete sync, comment/rating pull, rename/reparent sync.
4. A second destination. This matters more than its position suggests: an abstraction with one
   implementation hasn't been tested as an abstraction.

## Risks

| Risk                                                                                                   | Mitigation                                                                     |
| ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------ |
| **OAuth signing bugs — high likelihood**, percent-encoding and header-vs-query being the classic traps | RFC vectors before any SmugMug call; build `oauth1.rs` standalone and prove it |
| Duplicates after timeout — inherent to the protocol                                                    | Stable request id + end-of-session `reconcile()`                               |
| Spool left after a hard kill                                                                           | `Drop` guard + startup sweep; cache dir limits the blast radius                |
| Upstream declines                                                                                      | The fork strategy above; nothing wasted                                        |
| `keyring` unavailable on some Linux setups                                                             | Explicit error, no plaintext fallback                                          |
| SmugMug rate limits, undocumented                                                                      | Concurrency 3, honour `Retry-After`, log limit headers                         |

Open questions, and where they stand:

- **Rating/colour filters** — not implemented; a publish sends the whole album. Leaning towards
  respecting the library view's filters, with the count shown first.
- **AI tags as `X-Smug-Keywords`** — off; keywords are always empty. Probably opt-in later:
  publishing machine keywords silently is a surprising default.
- **Android** — settled: the panel is desktop-only, as there is no `keyring` backend.
- **Tuning** — `RENDER_CHUNK_SIZE = 8` and `UPLOAD_CONCURRENCY = 3` (`session.rs`) are still
  guesses worth measuring.
- **New-album privacy** — settled: a destination setting, default Public to match SmugMug's own
  Lightroom plugin, stated before an album is created and never applied to one that is linked
  (decision 15, [#17](https://github.com/markallisongit/RapidRAW/issues/17)).
- **Several accounts per destination** — deferred. One configuration per destination type; the
  state file belongs to one account and refuses to publish under another.
- **Downloading destination-only photos** — deferred. Photos found only on the destination are
  left alone; RapidRAW neither imports nor deletes them.

## References

[Upload reference](https://api.smugmug.com/api/v2/doc/reference/upload.html) ·
[OAuth FAQ](https://api.smugmug.com/api/v2/doc/tutorial/oauth/faq.html) ·
[OAuth example code](https://gist.github.com/smugmug-api-docs/10046914) ·
[Lightroom Publish service SDK](https://archive.stecman.co.nz/files/docs/lightroom-sdk/API-Reference/modules/SDK%20-%20Publish%20service%20provider.html) ·
[RFC 5849](https://datatracker.ietf.org/doc/html/rfc5849) ·
[`smugmug` crate](https://docs.rs/smugmug) (evaluated, not used) ·
SmugMug Lightroom plugin 3.5.19.1, disassembled string constants
