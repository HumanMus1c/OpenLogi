//! Cross-platform single-instance process guard.
//!
//! On startup a process tries to acquire an exclusive, non-blocking lock on a
//! named file under the user's data dir. Holding the lock keeps a second
//! invocation of the *same* role from running — the GUI uses it to avoid a
//! duplicate window, the background agent to avoid two processes fighting over
//! the same devices and IPC socket. Each [`Role`] has its own lock file, so
//! the GUI and the agent don't lock each other out. The lock is released by the
//! OS when the process exits, so crash-recovery is free: the next launch
//! reclaims the lock on the leftover file without any cleanup ceremony.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
};

use thiserror::Error;
use tracing::debug;

use crate::paths::{self, PathsError};

/// Held by `main` for the duration of the run; dropped on exit (the OS
/// releases the underlying file lock at the same time). The `_handle` field
/// is intentionally unused — the value is alive only for its `Drop` side
/// effect of closing the fd.
pub struct InstanceGuard {
    pid_path: PathBuf,
    _handle: File,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pid_path);
    }
}

/// Whether a [`Role`]'s single-instance lock in the active config directory
/// is currently held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleOccupancy {
    /// No live process holds this config directory's lock.
    Free,
    /// A live process holds the lock.
    Held {
        /// Process id recorded by the active lock holder, when available.
        pid: Option<u32>,
    },
}

/// Failure acquiring the single-instance lock.
/// [`InstanceError::AlreadyRunning`] is the expected "another copy is open"
/// signal; every other variant indicates filesystem trouble.
#[derive(Debug, Error)]
pub enum InstanceError {
    /// The lock file's directory could not be resolved (no home directory).
    #[error("could not resolve lock path")]
    Path(#[from] PathsError),
    /// Creating or opening the lock file failed.
    #[error("could not open lock file at {path}")]
    Open {
        /// The lock file being opened.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Another process of the same role already holds the lock — surface it
    /// politely and exit with a non-error status.
    #[error("another instance already holds the lock at {path}")]
    AlreadyRunning {
        /// The contested lock file.
        path: PathBuf,
    },
    /// The lock syscall itself failed, as opposed to the lock being held.
    #[error("lock attempt at {path} failed")]
    LockFailed {
        /// The lock file the attempt targeted.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// The processes that must run at most once per user, each on its own lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The background agent — real or mock — that owns the devices and the IPC
    /// socket. A tool that must keep every agent away from the hardware while
    /// it works takes this lock itself.
    Agent,
    /// The GUI: one settings window per user.
    Gui,
}

impl Role {
    /// The lock file's name under [`paths::config_dir`].
    const fn lock_file(self) -> &'static str {
        match self {
            Self::Agent => "agent.lock",
            Self::Gui => "openlogi.lock",
        }
    }

    /// Sidecar file recording the lock holder's PID under [`paths::config_dir`].
    const fn pid_file(self) -> &'static str {
        match self {
            Self::Agent => "agent.pid",
            Self::Gui => "openlogi.pid",
        }
    }
}

/// Acquire the single-instance lock for `role` (a file under
/// [`paths::config_dir`]). Returns `Ok(guard)` on success — keep the guard
/// alive until the process is about to exit.
///
/// `AlreadyRunning` is the polite "another copy is open" signal callers
/// surface to the user (and exit with a non-error status). Other variants
/// indicate filesystem trouble.
///
/// # Errors
///
/// Returns [`InstanceError`] if the lock path can't be resolved, the lock file
/// can't be opened, another instance already holds the lock, or the lock
/// syscall itself fails.
pub fn acquire(role: Role) -> Result<InstanceGuard, InstanceError> {
    let dir = paths::config_dir()?;
    acquire_in(&dir, role)
}

/// Probe whether `role`'s single-instance lock in [`paths::config_dir`] is
/// currently held by a live process in this config directory.
///
/// Uses the OS file lock rather than trusting stale PID files left behind by
/// crashed processes; when locked, reads the sidecar PID if present.
///
/// # Errors
///
/// Returns [`InstanceError`] if [`paths::config_dir`] cannot be resolved, or
/// opening/probing an existing lock file fails unexpectedly.
pub fn occupancy(role: Role) -> Result<RoleOccupancy, InstanceError> {
    let dir = paths::config_dir()?;
    occupancy_in(&dir, role)
}

fn acquire_in(dir: &Path, role: Role) -> Result<InstanceGuard, InstanceError> {
    let path = dir.join(role.lock_file());
    let pid_path = dir.join(role.pid_file());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| InstanceError::Open {
            path: path.clone(),
            source,
        })?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| InstanceError::Open {
            path: path.clone(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => {
            // Sidecar is read from another handle on Windows where mandatory
            // file locks forbid reading the lock file itself.
            let _ = std::fs::write(&pid_path, format!("{}\n", std::process::id()));
            debug!(path = %path.display(), "single-instance lock acquired");
            Ok(InstanceGuard {
                pid_path,
                _handle: file,
            })
        }
        Err(TryLockError::WouldBlock) => Err(InstanceError::AlreadyRunning { path }),
        Err(TryLockError::Error(source)) => Err(InstanceError::LockFailed { path, source }),
    }
}

fn occupancy_in(dir: &Path, role: Role) -> Result<RoleOccupancy, InstanceError> {
    let path = dir.join(role.lock_file());
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(RoleOccupancy::Free);
        }
        Err(source) => return Err(InstanceError::Open { path, source }),
    };
    match file.try_lock() {
        Ok(()) => Ok(RoleOccupancy::Free),
        Err(TryLockError::WouldBlock) => Ok(RoleOccupancy::Held {
            pid: read_pid(&dir.join(role.pid_file())),
        }),
        Err(TryLockError::Error(source)) => Err(InstanceError::LockFailed { path, source }),
    }
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|&pid| pid > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vacant_dir_is_free_and_writes_no_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            occupancy_in(dir.path(), Role::Gui).expect("occupancy"),
            RoleOccupancy::Free
        );
        assert!(!dir.path().join(Role::Gui.lock_file()).exists());
        assert!(!dir.path().join(Role::Gui.pid_file()).exists());
    }

    #[test]
    fn held_lock_reports_pid_and_isolates_distinct_config_dirs() {
        let wayland_dir = tempfile::tempdir().expect("wayland tempdir");
        let x11_dir = tempfile::tempdir().expect("x11 tempdir");

        let guard_wayland = acquire_in(wayland_dir.path(), Role::Gui).expect("acquire wayland");
        assert_eq!(
            occupancy_in(wayland_dir.path(), Role::Gui).expect("wayland held"),
            RoleOccupancy::Held {
                pid: Some(std::process::id())
            }
        );
        assert_eq!(
            occupancy_in(x11_dir.path(), Role::Gui).expect("x11 still free"),
            RoleOccupancy::Free
        );
        assert!(matches!(
            acquire_in(wayland_dir.path(), Role::Gui),
            Err(InstanceError::AlreadyRunning { .. })
        ));

        let guard_x11 = acquire_in(x11_dir.path(), Role::Gui).expect("acquire x11");
        assert_eq!(
            occupancy_in(x11_dir.path(), Role::Gui).expect("x11 held"),
            RoleOccupancy::Held {
                pid: Some(std::process::id())
            }
        );

        drop(guard_wayland);
        assert!(!wayland_dir.path().join(Role::Gui.pid_file()).exists());
        assert_eq!(
            occupancy_in(wayland_dir.path(), Role::Gui).expect("wayland freed"),
            RoleOccupancy::Free
        );
        assert_eq!(
            occupancy_in(x11_dir.path(), Role::Gui).expect("x11 unaffected"),
            RoleOccupancy::Held {
                pid: Some(std::process::id())
            }
        );

        drop(guard_x11);
        assert_eq!(
            occupancy_in(x11_dir.path(), Role::Gui).expect("x11 freed"),
            RoleOccupancy::Free
        );
    }

    #[test]
    fn stale_pid_without_live_lock_still_reports_free() {
        let dir = tempfile::tempdir().expect("tempdir");
        let guard = acquire_in(dir.path(), Role::Gui).expect("acquire");
        drop(guard);
        std::fs::write(dir.path().join(Role::Gui.pid_file()), "999999\n").expect("write stale pid");

        assert_eq!(
            occupancy_in(dir.path(), Role::Gui).expect("stale pid ignored"),
            RoleOccupancy::Free
        );
    }

    #[test]
    fn held_lock_without_pid_file_reports_unknown_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let guard = acquire_in(dir.path(), Role::Gui).expect("acquire");
        std::fs::remove_file(dir.path().join(Role::Gui.pid_file())).expect("remove pid file");

        assert_eq!(
            occupancy_in(dir.path(), Role::Gui).expect("held without pid"),
            RoleOccupancy::Held { pid: None }
        );
        drop(guard);
    }
}
