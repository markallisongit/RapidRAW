# RapidRAW → SmugMug publishing

**Design document** · 2026-09-12 · Mark Allison (with Claude)
Target: [CyberTimon/RapidRAW](https://github.com/CyberTimon/RapidRAW) · Fork: [markallisongit/RapidRAW](https://github.com/markallisongit/RapidRAW) · Branch: `feat/publish-destinations-smugmug`

**Implementation:** tracked in [#13](https://github.com/markallisongit/RapidRAW/issues/13), broken into ordered issues [#1](https://github.com/markallisongit/RapidRAW/issues/1)–[#12](https://github.com/markallisongit/RapidRAW/issues/12). Each issue carries its own task detail; this document is the rationale only.

## Summary

Publish RapidRAW albums to SmugMug from inside the app — no manual export step, no user-visible
intermediate files. Built on a general `PublishDestination` trait so further destinations (Flickr,
S3, Immich) are cheap. Images spool to a managed temp dir, upload, and are deleted.

**In scope:** one-action publish; republish replaces rather than duplicates; unchanged photos
skipped; full reuse of the export pipeline; resumable; guaranteed cleanup; no secrets in the binary.

**Out (phase 1):** delete sync, comment/rating pull, rename sync, `Group` tree mirroring, video,
other destinations.

## Decisions

| # | Question | Decision |
| --- | --- | --- |
| 1 | Sync model | Publish, phased. Remote-ID map + skip-unchanged in phase 1 |
| 2 | Mapping | One RapidRAW `Album` → one SmugMug album under a chosen root folder. `Group` flattened |
| 3 | Credentials | User supplies their own API key/secret. Nothing embedded |
| 4 | Token storage | OS keyring (`keyring` 4.2). Explicit error where unavailable, no plaintext fallback |
| 5 | OAuth | Hand-rolled `oauth1` module over `hmac`/`sha1` (~150 lines) |
| 6 | Callback | Out-of-band verifier code. No loopback listener |
| 7 | Byte path | Spool to managed temp dir, upload from disk, delete on success |
| 8 | Pipeline | Call existing `export_images_impl` unmodified with a temp output folder |
| 9 | Idempotency | Stable `X-Smug-UploadRequestId` per image + post-timeout reconciliation |

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
- Percent-encoding is RFC 3986 *unreserved only* (`-._~` survive, uppercase hex). Rust's URL
  helpers do not match this — hand-rolled and table-tested.
- **Uploads accept OAuth parameters only in the `Authorization` header.** The canonical failure:
  clients defaulting to query-string params work for ordinary API calls and fail opaquely on every
  upload.
- Uploads go to a different host, `POST https://upload.smugmug.com/`, and **the file is the raw
  request body** — not multipart, not form-encoded. So the body contributes no signature
  parameters; `Content-MD5` protects it.
- **`X-Smug-ImageUri` replaces** an existing image instead of creating one. This is what makes
  non-duplicating republish possible.

Undocumented headers, observed in SmugMug's own Lightroom plugin and worth adopting:
`X-Smug-UploadRequestId` (stable per-upload id — with `X-Smug-RetryCount`, lets the server
deduplicate retries), `X-Smug-AssetUri`, `X-Smug-Version`. The same plugin logs *"Album Upload
timeout previously, search for files that might have uploaded successfully"* — even SmugMug's
client reconciles rather than blindly retrying. It also sets per-upload timeouts from measured
bandwidth, and runs uploads on a pool separate from rendering.

## Architecture

**Pipeline.** `export_images_impl` (`export_processing.rs:862`) is already `pub(crate)` and takes a
completion channel; `run_headless_export` (`:1352`) is a working template for calling it outside
the command layer. A publish session does the same with a temp output folder.

**Chunking.** Process in chunks of 8 — render chunk *n+1* while uploading chunk *n*, so the GPU and
the network overlap and the spool stays bounded at ~16 images (~320 MB at 45 MP). Tuning constant.

**Spool.** `app_cache_dir/publish-spool/<session_uuid>/` — cache, not data: regenerable, and the OS
already treats it as disposable. Write a `session.json` marker with pid and start time; delete each
file *immediately* on upload success so the footprint tracks outstanding work; remove the directory
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
giving up risks a missing photo. Retry with the same request id; if exhausted, mark *ambiguous*,
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

    /// Idempotent.
    async fn ensure_container(&self, local: &LocalContainer, ctx: &PublishContext)
        -> Result<RemoteContainerId, PublishError>;

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

`publish_image` takes a **path**, not bytes — SmugMug reads once to hash for `Content-MD5` and
streams once to send, which is its business. `DestinationCapabilities` (`supports_replace`,
`supports_reconcile`, `supports_nested_containers`, `max_bytes`, `accepted_mime_types`) means the
session never special-cases on `id()`. `async-trait` is required: `dyn` async traits aren't
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
  "version": 1, "destination": "smugmug", "account": "markallison",
  "containers": { "<album_id>": { "remote_uri": "/api/v2/album/AbCdEf", "web_url": "…",
                                  "last_published": "2026-09-12T10:14:02Z" } },
  "images":     { "<virtual_path>": { "remote_uri": "/api/v2/image/XyZ123-0", "web_url": "…",
                                      "fingerprint": "b3:9f2c…", "last_published": "…" } }
}
```

**Key on the full virtual path**, not the source path — virtual copies use a `vc=` suffix
(`export_processing.rs:1030-1040`) and are distinct publishable photos.

**The fingerprint is Lightroom's `metadataThatTriggersRepublish`:**
`blake3(source_mtime, source_size, adjustments_json, relevant_export_settings)` — `blake3` is
already a dependency. Match ⇒ skip *before* rendering, so an unchanged album costs one file read
and no GPU work. `relevant_export_settings` excludes destination and filename-template fields,
which don't affect pixels.

## Module layout

```
src-tauri/src/publish/
  mod.rs        trait, registry, PublishContext, PublishError, commands
  session.rs    chunked render→upload driver, progress events, cancellation
  spool.rs      temp dir lifecycle, Drop guard, startup sweep
  state.rs      remote ID map, fingerprints, atomic persistence
  oauth1.rs     OAuth 1.0a signing — generic, no SmugMug specifics
  smugmug/      mod.rs · auth.rs (OAuth + keyring) · api.rs (album lookup/create)
                upload.rs (raw-body POST, retry, reconcile) · model.rs

src/components/panel/right/publish/
  PublishPanel.tsx · SmugMugAuthCard.tsx · PublishProgress.tsx
  usePublishState.ts · publish.i18n.ts
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

<raw file bytes, streamed from the spool>
```

## Frontend

A new **Publish** panel, sibling to Export — not a third entry in the export destination dropdown.
Publishing has state exporting doesn't (auth, album mapping, "12 unchanged, 3 to update, 1 new"),
and hiding that behind a select value makes the panel change shape drastically on one value.
`destination_type` (`export_processing.rs:78`) stays for filesystem destinations.

States: not configured (key/secret entry + link to the developer page) → not authorised (connect,
browser, verifier paste) → connected (account, root folder, album mapping, new/update/skip preview)
→ publishing (progress, per-image status, cancel). Export settings are reused from the existing
store with an explicit "publishing with your current export settings" line. The spool is never
surfaced.

## Integration surface

| File | Change | Lines | Conflict risk |
| --- | --- | --- | --- |
| `src-tauri/Cargo.toml` | 5 deps: `async-trait`, `hmac`, `sha1`, `md-5`, `keyring` | ~5 | Low |
| `src-tauri/src/lib.rs` | `mod publish;` + spool sweep in setup | 2 | Very low |
| `src-tauri/src/lib.rs` | commands in `generate_handler![]` (line 2093) | ~8 | **Medium — both sides append** |
| `src-tauri/src/app_state.rs` | `publish_registry` field + init | ~3 | Low |
| `src/App.tsx` | import + mount `PublishPanel` | ~6 | Medium — busy file |
| `src/store/useUIStore.ts` | `isPublishPanelVisible` | 2 | Low |
| `src/components/views/LibraryView.tsx` | toolbar button | ~4 | Low |
| `i18next.config.ts` | `extract.ignore` for the publish directory | 3 | Low |
| `src/@types/i18next.d.ts` | `PublishTranslations` in the type augmentation | ~4 | Low |
| **`src-tauri/src/export_processing.rs`** | **none** | **0** | **None** |
| `src/i18n/**` | **none** | 0 | None |

**~50 lines across 8 existing files** (`scripts/check-fork-surface.sh` prints the live figure;
`Cargo.lock` follows `Cargo.toml` and is not counted). Everything else is new, and new files never
conflict.

**i18n with zero edits:** `publish.i18n.ts` calls `i18n.addResourceBundle('en', 'translation',
{ publish: {…} }, true, true)` at import time, instead of editing thirteen locale JSONs — thirteen
conflict sites per merge.

**`generate_handler!`** is the one guaranteed recurring conflict: Tauri permits one
`invoke_handler` and both sides append. Chosen: ~8 commands in a contiguous block behind a
`// --- publish ---` marker, one mechanical hunk per merge that `rerere` learns. Fallback if that
proves painful: a single `publish_invoke(action, payload)` dispatch. Reversible.

## Fork maintenance

`smugmug` is the long-lived integration branch — upstream is **merged** in, never rebased, because
merges preserve the conflict history `rerere` needs. `pr/publish-destinations` carries clean
rebased history for the PR, regenerated from `smugmug`, never merged into. Conflating the two is
why long-lived forks become painful. `rerere` enabled. Merge upstream weekly, not on demand: small
frequent merges are far cheaper. Two commits ordered by independence — trait/registry/spool, then
the SmugMug destination — so if upstream takes only the reusable half the fork's delta shrinks to
one commit. `scripts/check-fork-surface.sh` fails if the diff against `upstream/main` touches
anything outside the table above; surface creep is invisible otherwise.

For the PR: open a discussion issue first, lead with the integration surface, frame it as
infrastructure with SmugMug as the reference implementation, and offer to maintain it.

## Testing

RFC 5849 §1.2 vectors and SmugMug's [example signing code](https://gist.github.com/smugmug-api-docs/10046914)
for `oauth1.rs`, with a table-driven percent-encoding test. Property test for fingerprint
stability. Round-trip, migration and interrupted-write tests for the state file. Guard-runs-on-
every-exit-path and sweep tests for the spool. `wiremock` fixtures for `api.rs`/`upload.rs` and the
retry scenarios (500-then-success, 429 with `Retry-After`, timeout-then-found,
timeout-then-absent) — no live network in CI. Manual end-to-end against a real account. The export
pipeline is untouched, so its tests remain valid unchanged — worth stating in the PR.

## Phasing

1. Trait, registry, spool, state, OAuth, SmugMug upload, panel — the working feature.
2. `Group` → folder mirroring, keywords/captions from tags.
3. Delete sync, comment/rating pull, rename/reparent sync.
4. A second destination. This matters more than its position suggests: an abstraction with one
   implementation hasn't been tested as an abstraction.

## Risks

| Risk | Mitigation |
| --- | --- |
| **OAuth signing bugs — high likelihood**, percent-encoding and header-vs-query being the classic traps | RFC vectors before any SmugMug call; build `oauth1.rs` standalone and prove it |
| Duplicates after timeout — inherent to the protocol | Stable request id + end-of-session `reconcile()` |
| Spool left after a hard kill | `Drop` guard + startup sweep; cache dir limits the blast radius |
| Upstream declines | The fork strategy above; nothing wasted |
| `keyring` unavailable on some Linux setups | Explicit error, no plaintext fallback |
| SmugMug rate limits, undocumented | Concurrency 3, honour `Retry-After`, log limit headers |

Open, not blocking: whether to respect the library view's rating/colour filters (leaning yes, with
the count shown first); whether AI tags (`tagging.rs`) feed `X-Smug-Keywords` (probably, off by
default — publishing machine keywords silently is a surprising default); Android has no `keyring`
backend, so phase 1 may gate the panel to desktop; the 8-image chunk is a guess worth measuring.

## References

[Upload reference](https://api.smugmug.com/api/v2/doc/reference/upload.html) ·
[OAuth FAQ](https://api.smugmug.com/api/v2/doc/tutorial/oauth/faq.html) ·
[OAuth example code](https://gist.github.com/smugmug-api-docs/10046914) ·
[Lightroom Publish service SDK](https://archive.stecman.co.nz/files/docs/lightroom-sdk/API-Reference/modules/SDK%20-%20Publish%20service%20provider.html) ·
[RFC 5849](https://datatracker.ietf.org/doc/html/rfc5849) ·
[`smugmug` crate](https://docs.rs/smugmug) (evaluated, not used) ·
SmugMug Lightroom plugin 3.5.19.1, disassembled string constants
