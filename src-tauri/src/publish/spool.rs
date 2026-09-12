//! The publish spool: the managed temp directory rendered images pass through
//! on their way to a destination.
//!
//! It lives under `app_cache_dir`, never `app_data_dir` — every byte in it is
//! regenerable from the original file plus its adjustments, and the OS already
//! treats a cache directory as disposable. It is never surfaced to the user.
//!
//! Two mechanisms keep it from leaking, because neither is sufficient alone:
//! the [`Drop`] guard covers every ordinary exit path including a panic unwind,
//! mirroring `ExportTaskGuard` in `export_processing.rs`; and
//! [`sweep_orphaned_sessions`] at startup covers what `Drop` cannot — SIGKILL,
//! an OOM kill, or a power loss.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::publish::PublishError;

/// Parent of every session directory, under the app cache directory.
const SPOOL_DIR_NAME: &str = "publish-spool";

/// Written into each session directory so the sweep can judge it.
const SESSION_MARKER: &str = "session.json";

/// Beyond this a session is swept whether or not its pid still resolves. A pid
/// is reused eventually, so liveness alone would strand a directory forever
/// once an unrelated process inherited the number; age is the backstop.
const MAX_SESSION_AGE_HOURS: i64 = 24;

/// Fallback when a file name has no final component (`..`, `/`, or empty), so
/// [`Spool::slot`] always returns a path directly inside the spool.
const UNNAMED_SLOT: &str = "unnamed";

/// On-disk contents of `session.json`.
///
/// `started_at` is a string rather than a `DateTime<Utc>` because chrono's
/// `serde` feature is not enabled in this tree, and RFC 3339 round-trips
/// through `to_rfc3339`/`parse_from_rfc3339` without it.
#[derive(Debug, Serialize, Deserialize)]
struct SessionMarker {
    version: u32,
    pid: u32,
    started_at: String,
    session_id: Uuid,
}

impl SessionMarker {
    const VERSION: u32 = 1;
}

/// A session's spool directory, deleted when this value is dropped.
pub struct Spool {
    root: PathBuf,
    session_id: Uuid,
}

impl Spool {
    /// Creates the directory and writes its marker.
    pub fn create(app_handle: &AppHandle) -> Result<Self, PublishError> {
        Self::create_at(&spool_root(app_handle)?)
    }

    /// [`Self::create`] without an `AppHandle`, so the lifecycle is testable
    /// against a plain temp directory.
    pub fn create_at(base: &Path) -> Result<Self, PublishError> {
        let session_id = Uuid::new_v4();
        let root = base.join(session_id.to_string());

        std::fs::create_dir_all(&root)
            .map_err(|e| PublishError::Io(format!("creating spool {}: {e}", root.display())))?;

        let marker = SessionMarker {
            version: SessionMarker::VERSION,
            pid: std::process::id(),
            started_at: Utc::now().to_rfc3339(),
            session_id,
        };
        let encoded = serde_json::to_vec(&marker)
            .map_err(|e| PublishError::Io(format!("encoding {SESSION_MARKER}: {e}")))?;

        // A directory without a readable marker is swept, so a spool that
        // failed here would be cleaned up rather than stranded. Still, report
        // it: the caller should not start rendering into a broken spool.
        if let Err(e) = std::fs::write(root.join(SESSION_MARKER), encoded) {
            let _ = std::fs::remove_dir_all(&root);
            return Err(PublishError::Io(format!("writing {SESSION_MARKER}: {e}")));
        }

        Ok(Self { root, session_id })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Where to render an image. Does not create the file.
    ///
    /// Only the final component of `file_name` is used, so the returned path is
    /// always directly inside the spool — [`Self::release`] and [`Drop`] both
    /// rely on that being true.
    pub fn slot(&self, file_name: &str) -> PathBuf {
        let name = Path::new(file_name)
            .file_name()
            .unwrap_or_else(|| OsStr::new(UNNAMED_SLOT));
        self.root.join(name)
    }

    /// Deletes one spooled file, to be called the moment an upload is
    /// confirmed rather than batched at the end of the session: the footprint
    /// should track outstanding work, not total work.
    ///
    /// Succeeds if the file is already gone, so a retried upload can call it
    /// more than once.
    pub fn release(&self, path: &Path) -> Result<(), PublishError> {
        // Compared against the parent rather than with `starts_with`, which
        // accepts an unnormalised `<root>/../elsewhere.jpg`.
        if path.parent() != Some(self.root.as_path()) {
            return Err(PublishError::Io(format!(
                "refusing to release {}: outside the spool",
                path.display()
            )));
        }

        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PublishError::Io(format!(
                "releasing {}: {e}",
                path.display()
            ))),
        }
    }

    /// Current byte footprint, for the assertion that the spool stays bounded.
    /// Unreadable entries are skipped rather than failing the measurement.
    pub fn size_on_disk(&self) -> u64 {
        walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum()
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        // Logged, not propagated: this runs on unwind paths too, and a panic
        // during a panic aborts the process.
        if let Err(e) = std::fs::remove_dir_all(&self.root)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!(
                "Failed to remove publish spool {}: {e}. The startup sweep will retry.",
                self.root.display()
            );
        }
    }
}

/// Removes spool directories left behind by a previous run. Call once at app
/// setup, before any session exists.
///
/// Returns how many were removed. Not optional: [`Drop`] cannot run after a
/// SIGKILL or a power loss, and this is the only thing that cleans up after one.
pub fn sweep_orphaned_sessions(app_handle: &AppHandle) -> Result<usize, PublishError> {
    sweep_orphaned_sessions_at(&spool_root(app_handle)?, Utc::now(), process_is_alive)
}

/// [`sweep_orphaned_sessions`] with the clock and the liveness check injected,
/// so the decision logic is testable without spawning processes or waiting a
/// day.
pub fn sweep_orphaned_sessions_at(
    base: &Path,
    now: DateTime<Utc>,
    is_alive: impl Fn(u32) -> bool,
) -> Result<usize, PublishError> {
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        // Nothing has ever been spooled.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(PublishError::Io(format!(
                "reading spool root {}: {e}",
                base.display()
            )));
        }
    };

    let mut removed = 0;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !is_orphaned(&path, now, &is_alive) {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            // A failure here is not fatal: the next startup tries again.
            Err(e) => log::warn!(
                "Failed to sweep orphaned publish spool {}: {e}",
                path.display()
            ),
        }
    }
    Ok(removed)
}

/// A session directory is orphaned when it is too old, when its process is
/// gone, or when its marker cannot be read — an unreadable marker means the
/// directory was never fully created, so there is nothing to preserve.
fn is_orphaned(path: &Path, now: DateTime<Utc>, is_alive: &impl Fn(u32) -> bool) -> bool {
    let Some(marker) = read_marker(path) else {
        return true;
    };

    let Ok(started_at) = DateTime::parse_from_rfc3339(&marker.started_at) else {
        return true;
    };
    if now.signed_duration_since(started_at.with_timezone(&Utc))
        >= Duration::hours(MAX_SESSION_AGE_HOURS)
    {
        return true;
    }

    !is_alive(marker.pid)
}

fn read_marker(path: &Path) -> Option<SessionMarker> {
    let bytes = std::fs::read(path.join(SESSION_MARKER)).ok()?;
    let marker: SessionMarker = serde_json::from_slice(&bytes).ok()?;
    (marker.version == SessionMarker::VERSION).then_some(marker)
}

/// Whether a pid still resolves to a running process.
///
/// `sysinfo` rather than `libc::kill(pid, 0)` plus `OpenProcess`: it is already
/// a dependency, works the same way on all three desktop targets, and refreshes
/// only the one pid asked about. Injected into
/// [`sweep_orphaned_sessions_at`], so it is never exercised by the tests.
fn process_is_alive(pid: u32) -> bool {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    system.process(pid).is_some()
}

fn spool_root(app_handle: &AppHandle) -> Result<PathBuf, PublishError> {
    app_handle
        .path()
        .app_cache_dir()
        .map(|dir| dir.join(SPOOL_DIR_NAME))
        .map_err(|e| PublishError::Io(format!("resolving the app cache directory: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a session directory by hand so the sweep can be driven with an
    /// arbitrary pid and start time. Deliberately not routed through
    /// [`Spool::create_at`]: that would make the test depend on a spool that
    /// deletes itself on drop, and could not forge an age.
    fn write_session(base: &Path, pid: u32, started_at: DateTime<Utc>) -> PathBuf {
        let session_id = Uuid::new_v4();
        let dir = base.join(session_id.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let marker = serde_json::json!({
            "version": 1,
            "pid": pid,
            "started_at": started_at.to_rfc3339(),
            "session_id": session_id,
        });
        std::fs::write(
            dir.join("session.json"),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn drop_removes_the_spool_directory() {
        let base = tempfile::tempdir().unwrap();
        let path = {
            let spool = Spool::create_at(base.path()).unwrap();
            let p = spool.path().to_path_buf();
            assert!(p.exists());
            assert!(p.join("session.json").exists());
            p
        };
        assert!(!path.exists(), "spool survived Drop");
    }

    #[test]
    fn drop_removes_the_directory_even_with_files_still_in_it() {
        let base = tempfile::tempdir().unwrap();
        let path = {
            let spool = Spool::create_at(base.path()).unwrap();
            std::fs::write(spool.slot("a.jpg"), b"xxxx").unwrap();
            std::fs::write(spool.slot("b.jpg"), b"yyyy").unwrap();
            spool.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    #[test]
    fn drop_runs_on_panic_unwind() {
        let base = tempfile::tempdir().unwrap();
        let base_path = base.path().to_path_buf();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(PathBuf::new()));
        let sink = captured.clone();
        let _ = std::panic::catch_unwind(move || {
            let spool = Spool::create_at(&base_path).unwrap();
            *sink.lock().unwrap() = spool.path().to_path_buf();
            panic!("simulated failure mid-publish");
        });
        let path = captured.lock().unwrap().clone();
        assert_ne!(path, PathBuf::new(), "the spool was never created");
        assert!(!path.exists(), "spool survived a panic");
    }

    #[test]
    fn release_deletes_immediately_and_shrinks_footprint() {
        let base = tempfile::tempdir().unwrap();
        let spool = Spool::create_at(base.path()).unwrap();
        let a = spool.slot("a.jpg");
        std::fs::write(&a, vec![0u8; 1024]).unwrap();
        assert!(spool.size_on_disk() >= 1024);
        spool.release(&a).unwrap();
        assert!(!a.exists());
        assert!(spool.size_on_disk() < 1024);
    }

    #[test]
    fn sweep_removes_old_and_dead_but_keeps_live_sessions() {
        let base = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let old_and_dead = write_session(base.path(), 1001, now - chrono::Duration::hours(48));
        let fresh_and_live = write_session(base.path(), 1002, now - chrono::Duration::hours(1));
        let old_but_live = write_session(base.path(), 1003, now - chrono::Duration::hours(48));

        let removed = sweep_orphaned_sessions_at(base.path(), now, |pid| pid != 1001).unwrap();

        assert_eq!(removed, 2);
        assert!(!old_and_dead.exists());
        assert!(fresh_and_live.exists(), "a live recent session was swept");
        assert!(!old_but_live.exists(), "age must win over a live pid");
    }

    #[test]
    fn sweep_removes_a_session_with_no_readable_marker() {
        let base = tempfile::tempdir().unwrap();
        let orphan = base.path().join("no-marker");
        std::fs::create_dir_all(orphan.join("nested")).unwrap();
        std::fs::write(orphan.join("nested/a.jpg"), b"xx").unwrap();

        let removed = sweep_orphaned_sessions_at(base.path(), Utc::now(), |_| true).unwrap();

        assert_eq!(removed, 1);
        assert!(!orphan.exists());
    }

    #[test]
    fn sweep_is_a_no_op_when_the_spool_root_has_never_existed() {
        let base = tempfile::tempdir().unwrap();
        let missing = base.path().join("publish-spool");

        let removed = sweep_orphaned_sessions_at(&missing, Utc::now(), |_| true).unwrap();

        assert_eq!(removed, 0);
    }

    #[test]
    fn slot_keeps_a_traversing_file_name_inside_the_spool() {
        let base = tempfile::tempdir().unwrap();
        let spool = Spool::create_at(base.path()).unwrap();

        let slot = spool.slot("../../escaped.jpg");

        assert_eq!(slot.parent(), Some(spool.path()));
    }

    #[test]
    fn release_refuses_a_path_outside_the_spool() {
        let base = tempfile::tempdir().unwrap();
        let spool = Spool::create_at(base.path()).unwrap();
        let outsider = base.path().join("not-ours.jpg");
        std::fs::write(&outsider, b"precious").unwrap();

        assert!(spool.release(&outsider).is_err());
        assert!(
            outsider.exists(),
            "release deleted a file outside the spool"
        );
    }

    #[test]
    fn release_is_idempotent_so_a_retried_upload_can_call_it_twice() {
        let base = tempfile::tempdir().unwrap();
        let spool = Spool::create_at(base.path()).unwrap();
        let a = spool.slot("a.jpg");
        std::fs::write(&a, b"xxxx").unwrap();

        spool.release(&a).unwrap();
        spool
            .release(&a)
            .expect("releasing an already-released file must succeed");
    }
}
