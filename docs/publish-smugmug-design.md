# RapidRAW → SmugMug publishing

**Design document** · 2026-09-12, updated 2026-09-13 · Mark Allison (with Claude)
Target: [CyberTimon/RapidRAW](https://github.com/CyberTimon/RapidRAW) · Fork: [markallisongit/RapidRAW](https://github.com/markallisongit/RapidRAW) · Branch: `feat/publish-destinations-smugmug`

**Implementation:** phase 1 tracked in [#13](https://github.com/markallisongit/RapidRAW/issues/13) (issues [#1](https://github.com/markallisongit/RapidRAW/issues/1)–[#12](https://github.com/markallisongit/RapidRAW/issues/12)); the Publish Manager batch in [#14](https://github.com/markallisongit/RapidRAW/issues/14) (issues [#15](https://github.com/markallisongit/RapidRAW/issues/15)–[#24](https://github.com/markallisongit/RapidRAW/issues/24)). Each issue carries its own task detail; this document is the rationale only.

**Status:** phase 1 complete: all twelve issues closed, and tested end-to-end against a live SmugMug account on 2026-09-13. The #14 batch is in progress; [#15](https://github.com/markallisongit/RapidRAW/issues/15) (Publish as a panel tab) is done. This document describes what is implemented, and #24 brings it fully up to date at the end of the batch.

## Summary

Publish RapidRAW albums to SmugMug from inside the app — no manual export step, no user-visible
intermediate files. Built on a general `PublishDestination` trait so further destinations (Flickr,
S3, Immich) are cheap. Images spool to a managed temp dir, upload, and are deleted.

**In scope:** one-action publish; republish replaces rather than duplicates; unchanged photos
skipped; full reuse of the export pipeline; resumable; guaranteed cleanup; no secrets in the binary.

**Out (phase 1):** delete sync, comment/rating pull, rename sync, `Group` tree mirroring, video,
other destinations.

## Decisions

| #   | Question      | Decision                                                                                           |
| --- | ------------- | -------------------------------------------------------------------------------------------------- |
| 1   | Sync model    | Publish, phased. Remote-ID map + skip-unchanged in phase 1                                         |
| 2   | Mapping       | One RapidRAW `Album` → one SmugMug album under the account root. `Group` path folded into the name |
| 3   | Credentials   | User supplies their own API key/secret. Nothing embedded                                           |
| 4   | Token storage | OS keyring (`keyring` 4.2). Explicit error where unavailable, no plaintext fallback                |
| 5   | OAuth         | Hand-rolled `oauth1` module over `hmac`/`sha1` (~150 lines)                                        |
| 6   | Callback      | Out-of-band verifier code. No loopback listener                                                    |
| 7   | Byte path     | Spool to managed temp dir, upload from disk, delete on success                                     |
| 8   | Pipeline      | Call existing `export_images_impl` unmodified with a temp output folder                            |
| 9   | Idempotency   | Stable `X-Smug-UploadRequestId` per image + post-timeout reconciliation                            |

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

**Pipeline.** `export_images_impl` (`export_processing.rs:862`) is already `pub(crate)` and takes a
completion channel; `run_headless_export` (`:1352`) is a working template for calling it outside
the command layer. A publish session does the same with a temp output folder.

**Chunking.** Process in chunks of 8 — render chunk _n+1_ while uploading chunk _n_, so the GPU and
the network overlap and the spool stays bounded at ~16 images (~320 MB at 45 MP). Tuning constant.

**Spool.** `app_cache_dir/publish-spool/<session_uuid>/` — cache, not data: regenerable, and the OS
already treats it as disposable. Write a `session.json` marker with pid and start time; delete each
file _immediately_ on upload success so the footprint tracks outstanding work; remove the directory
via a `Drop` guard on every exit path, mirroring `ExportTaskGuard` (`:307-409`). `Drop` can't run
after SIGKILL, so also sweep at startup for sessions >24 h old or with a dead pid. Never surfaced
to the user.

**Concurrency.** The export loop sizes its pool from cores and free RAM (`:900-915`, clamped 1–4) —
a GPU heuristic, wrong for network I/O. Publishing uses a separate semaphore, default 3.

**Retry.** Exponential backoff with jitter on 5xx and 429, honouring `Retry-After`, max 4 attempts,
same request id throughout. Timeouts are derived from measured throughput and file size — fixed
values are wrong when sizes vary 20× and uplinks 100×. Cancellation reuses the `AtomicBool` pattern
from `cancel_export` (`:1455`).

**Ambiguous failures.** A timeout may or may not have committed; blind retry risks a duplicate,
giving up risks a missing photo. Retry with the same request id; if exhausted, mark _ambiguous_,
not failed; at session end call `reconcile()` to list the album and match by filename and size;
record what's found, re-queue what isn't. Per-image failures never abort the batch, and the state
file records only confirmed successes.

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

    /// The remote album tree, a level at a time, for linking to an album that exists.
    async fn list_containers(&self, parent: Option<&RemoteNodeId>, ctx: &PublishContext)
        -> Result<Vec<RemoteNode>, PublishError>;
    /// Linking to a new album: find reports a same-named one, so create never reuses it.
    async fn find_container(&self, name: &str, ctx: &PublishContext)
        -> Result<Option<RemoteNode>, PublishError>;
    async fn create_container(&self, name: &str, ctx: &PublishContext)
        -> Result<RemoteNode, PublishError>;
    async fn container(&self, id: &RemoteContainerId, ctx: &PublishContext)
        -> Result<RemoteNode, PublishError>;

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
`max_bytes`, `accepted_mime_types`) means the session never special-cases on `id()`. `async-trait` is required: `dyn` async traits aren't
object-safe without boxing on Rust 1.98.

The registry is a plain `Vec<Arc<dyn PublishDestination>>` with an explicit constructor, not
`inventory`/`linkme` link-time magic — one obvious registration site, and a stable one-line seam
for the fork.

### Publish state

`app_data_dir/publish/<destination_id>.json`, mirroring `albums/albums.json`
(`file_management.rs:838-847`). Written atomically (temp + rename) after each success, so an
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
    },
  },
}
```

`account` is `null` until the first publish, and `web_url` is `null` where SmugMug returns none.
The account is read and written on its own (`PublishState::account_in` / `set_account_in`), so
connecting never loads — or migrates — the image records.

**Image records live under their link.** A photo in two RapidRAW albums is two uploads, one into
each remote album, each replaced or skipped on its own. Relinking an album to a *different*
remote album drops its records, which describe images in the old album.

**Key on the full virtual path**, not the source path — virtual copies use a `vc=` suffix
(`export_processing.rs:937-942`, named `_VCnn` at `:1034-1038`) and are distinct publishable photos.

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
output template would re-upload the whole library.

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
  state.rs             links, per-link remote ID map, split fingerprints, v1 migration
  oauth1.rs            OAuth 1.0a signing — generic, no SmugMug specifics
  credential_store.rs  keyring access for consumer keys and tokens
  smugmug/             mod.rs · auth.rs (OAuth flow) · api.rs (album lookup/create)
                       upload.rs (raw-body POST, retry, reconcile) · model.rs

src/components/panel/right/publish/
  PublishPanel.tsx     tab shell: link flow, confirmations, the settings-change question
  DestinationSection.tsx · LinkedAlbumRow.tsx (status, row actions)
  LinkAlbumFlow.tsx · RemoteAlbumBrowser.tsx · PublishSummary.tsx · PublishProgress.tsx
  output.ts (preset summary, format checks) · usePublishState.ts · publish.i18n.ts
  manager/             PublishManagerModal.tsx (portal, destination list, Save/Cancel)
                       SmugMugAccountSection.tsx · OutputSection.tsx · NewAlbumsSection.tsx
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
(`export_processing.rs:78`) stays for filesystem destinations.

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

The panel is built around **linked albums**, Lightroom's published collections. Set-up lives in
the Publish Manager; the panel prompts for it when the destination is not connected or has no
output preset, and publishing is disabled until both are in place. Each link shows a status from
`publish_preview`, checked one album at a time while the tab is visible and again after a publish
or an album change: up to date, N changed, N new, N affected by settings, not found on the
destination, not published yet. Right-click or the row's menu publishes, opens the remote album,
links to a different one or unlinks, each change confirmed. A link whose RapidRAW album was
deleted can never be published again (album ids are never reused), so it is not listed: one line
under the list counts such links and removes them, deleting nothing remotely.

"Publish an album…" links first and uploads nothing: choose a RapidRAW album, then create a new
remote album (its privacy stated before it is created, and an existing same-named album offered
instead) or browse to one that exists. Publishing always requires a link and the destination's
preset; nothing is found or created by name, and nothing falls back to the Export panel's
settings. A publish whose album has settings-only changes asks first — republish them too, or
only upload edited and new photos. Progress, per-image status including failed and ambiguous,
and cancel follow. The spool is never surfaced.

## Integration surface

| File                                     | Change                                                                                                       | Lines | Conflict risk                  |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------ | ----- | ------------------------------ |
| `src-tauri/Cargo.toml`                   | deps `async-trait`, `hmac`, `sha1`, `md-5`, `bytes`, `keyring`; dev-deps `tokio`, `wiremock` (with comments) | 21    | Low                            |
| `src-tauri/src/lib.rs`                   | `mod publish;` + spool sweep in setup + registry init                                                        | 3     | Very low                       |
| `src-tauri/src/lib.rs`                   | commands in `generate_handler![]`                                                                            | 10    | **Medium — both sides append** |
| `src-tauri/src/app_state.rs`             | `publish_registry` field                                                                                     | 1     | Low                            |
| `src/App.tsx`                            | import + `registerPublishResources()`; `case Panel.Publish` in `renderAppPanel`                              | 6     | Medium — busy file             |
| `src/components/ui/AppProperties.tsx`    | `Panel.Publish` enum member                                                                                  | 1     | Low                            |
| `src/components/panel/PanelSwitcher.tsx` | `Send` icon and tooltip key in `PANEL_ICONS` / `PANEL_TITLES`                                                | 3     | Low                            |
| `src/store/useUIStore.ts`                | `Panel.Publish` in `ALL_PANELS`, default regions and both default layouts                                    | 10    | Low                            |
| `src/store/useUIStore.ts`                | Android gating: `isPublishSupported` in the `allowedPanels` filter                                           | 15    | Low                            |
| `src/hooks/useAppContextMenus.ts`        | import; `Publish to ▸` options for albums; deleted-link notice after an album delete                         | 3     | Low                            |
| `i18next.config.ts`                      | `extract.ignore` for the publish directory                                                                   | 3     | Low                            |
| `src/@types/i18next.d.ts`                | `PublishTranslations` in the type augmentation                                                               | 5     | Low                            |
| **`src-tauri/src/export_processing.rs`** | **none**                                                                                                     | **0** | **None**                       |
| `src/i18n/**`                            | **none**                                                                                                     | 0     | None                           |

**~90 lines across 10 existing files** (90+/4− after #23; `scripts/check-fork-surface.sh` prints the live figure;
`Cargo.lock` follows `Cargo.toml` and is not counted). Everything else is new, and new files never
conflict.

**i18n with zero locale edits:** `registerPublishResources()` in `publish.i18n.ts`, called once from
`App.tsx`, runs `i18n.addResourceBundle(lang, 'translation', { publish: {…} }, true, true)` instead
of editing thirteen locale JSONs — thirteen conflict sites per merge. Runtime registration alone
fails CI's `i18next-cli extract --ci`, so `i18next.config.ts` ignores the publish directory, and
`PublishTranslations` joins the `i18next.d.ts` augmentation so keys stay type-checked.

**`generate_handler!`** is the one guaranteed recurring conflict: Tauri permits one
`invoke_handler` and both sides append. Chosen: the eight `publish_*` commands in one contiguous
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
timeout-then-absent) — no live network in CI. Manual end-to-end against a real account. The project
had no Rust tests before this; `publish` founds the harness (`[dev-dependencies]`, `#[cfg(test)]`
modules) rather than extending one. The export pipeline is untouched, not "still tested".

## Phasing

1. Trait, registry, spool, state, OAuth, SmugMug upload, panel — the working feature. **Done.**
2. `Group` → folder mirroring, keywords/captions from tags.
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

Open questions, and where phase 1 left them:

- **Rating/colour filters** — not implemented; a publish sends the whole album. Leaning towards
  respecting the library view's filters, with the count shown first.
- **AI tags as `X-Smug-Keywords`** — off; keywords are always empty. Probably opt-in later:
  publishing machine keywords silently is a surprising default.
- **Android** — settled: the panel is desktop-only, as there is no `keyring` backend.
- **Tuning** — `RENDER_CHUNK_SIZE = 8` and `UPLOAD_CONCURRENCY = 3` (`session.rs`) are still
  guesses worth measuring.
- **Album privacy** — new albums set none and inherit the account root's, which may be public.
  Decided for #14: new-album privacy becomes a destination setting, default Public to match
  SmugMug's own Lightroom plugin, shown before an album is created
  ([#17](https://github.com/markallisongit/RapidRAW/issues/17)).

## References

[Upload reference](https://api.smugmug.com/api/v2/doc/reference/upload.html) ·
[OAuth FAQ](https://api.smugmug.com/api/v2/doc/tutorial/oauth/faq.html) ·
[OAuth example code](https://gist.github.com/smugmug-api-docs/10046914) ·
[Lightroom Publish service SDK](https://archive.stecman.co.nz/files/docs/lightroom-sdk/API-Reference/modules/SDK%20-%20Publish%20service%20provider.html) ·
[RFC 5849](https://datatracker.ietf.org/doc/html/rfc5849) ·
[`smugmug` crate](https://docs.rs/smugmug) (evaluated, not used) ·
SmugMug Lightroom plugin 3.5.19.1, disassembled string constants
