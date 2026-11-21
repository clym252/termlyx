//! First launch after the Tether → Termlyx rename: moves the old app id's data
//! dirs (vault, webview storage = device settings) to the new id, before any
//! window or the vault opens. Rename only: never copies, never deletes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The app id Tether used.
pub const OLD_ID: &str = "dev.tether.client";

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Moved,
    KeptNew,
    NothingToMove,
}

/// Moves `old` to `new` with one `rename`. Old missing: nothing. Both exist:
/// touches neither and keeps `new`. On error `new` does not exist (only its
/// parent may have been created).
pub fn move_dir(old: &Path, new: &Path) -> Result<Outcome, String> {
    if !present(old)? {
        return Ok(Outcome::NothingToMove);
    }
    if present(new)? {
        tracing::warn!(old = %old.display(), new = %new.display(), "both data dirs exist; using the new one, the old one stays");
        return Ok(Outcome::KeptNew);
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::rename(old, new)
        .map_err(|e| format!("move {} to {}: {e}", old.display(), new.display()))?;
    tracing::info!(old = %old.display(), new = %new.display(), "moved data dir");
    Ok(Outcome::Moved)
}

/// Whether something is at `path` (a symlink is not followed).
fn present(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("check {}: {e}", path.display())),
    }
}

/// (old, new) data dirs for this platform, from the environment: the dirs
/// Tauri uses for app data and webview storage. Empty on iOS and Android.
pub fn data_pairs(new_id: &str) -> Vec<(PathBuf, PathBuf)> {
    data_pairs_from(|key| std::env::var_os(key), new_id)
}

fn data_pairs_from(
    env: impl Fn(&str) -> Option<OsString>,
    new_id: &str,
) -> Vec<(PathBuf, PathBuf)> {
    // A missing or relative variable skips its pair.
    let var = |key: &str| env(key).map(PathBuf::from).filter(|p| p.is_absolute());
    #[cfg(target_os = "macos")]
    let bases: Vec<PathBuf> = var("HOME")
        .map(|home| {
            vec![
                home.join("Library/Application Support"),
                home.join("Library/WebKit"),
            ]
        })
        .unwrap_or_default();
    #[cfg(windows)]
    let bases: Vec<PathBuf> = ["APPDATA", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(var)
        .collect();
    #[cfg(target_os = "linux")]
    let bases: Vec<PathBuf> = var("XDG_DATA_HOME")
        .or_else(|| var("HOME").map(|home| home.join(".local/share")))
        .into_iter()
        .collect();
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    let bases: Vec<PathBuf> = {
        let _ = var;
        Vec::new()
    };
    bases
        .into_iter()
        .map(|base| (base.join(OLD_ID), base.join(new_id)))
        .collect()
}

/// Moves every pair for `new_id`, retrying a failed move every 500 ms for up
/// to 10 s (Windows: a lingering old process still holds a file open).
pub fn run(new_id: &str) -> Result<(), String> {
    run_pairs(&data_pairs(new_id), 20, Duration::from_millis(500))
}

fn run_pairs(pairs: &[(PathBuf, PathBuf)], tries: u32, wait: Duration) -> Result<(), String> {
    for (old, new) in pairs {
        let mut attempt = 1;
        // Sleeps only after a failure, so a normal launch never waits.
        while let Err(e) = move_dir(old, new) {
            if attempt >= tries {
                return Err(format!(
                    "Quit Tether, then open {} again. ({e})",
                    crate::APP_NAME
                ));
            }
            tracing::warn!(attempt, "data dir move failed, retrying: {e}");
            attempt += 1;
            std::thread::sleep(wait);
        }
    }
    Ok(())
}

/// Shows `message` (Windows: a message box; elsewhere stderr) and exits
/// before any window is built.
pub fn fail(message: &str) -> ! {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (text, title) = (wide(message), wide(crate::APP_NAME));
        // SAFETY: both are NUL-terminated UTF-16 buffers that outlive the call; no owner window.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            )
        };
    }
    #[cfg(not(windows))]
    eprintln!("{message}");
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const NEW_ID: &str = "ca.clym.termlyx";

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn moves_old_dir_with_contents() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (root.path().join(OLD_ID), root.path().join(NEW_ID));
        write(&old.join("vault.sqlite"), "vault");
        write(&old.join("sub/x"), "x");
        assert_eq!(move_dir(&old, &new), Ok(Outcome::Moved));
        assert_eq!(read(&new.join("vault.sqlite")), "vault");
        assert_eq!(read(&new.join("sub/x")), "x");
        assert!(!old.exists());
    }

    #[test]
    fn keeps_both_when_new_exists() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (root.path().join(OLD_ID), root.path().join(NEW_ID));
        write(&old.join("vault.sqlite"), "old");
        write(&new.join("other"), "new");
        assert_eq!(move_dir(&old, &new), Ok(Outcome::KeptNew));
        assert_eq!(read(&old.join("vault.sqlite")), "old");
        assert_eq!(read(&new.join("other")), "new");
        assert!(!new.join("vault.sqlite").exists());
    }

    #[test]
    fn nothing_when_old_missing() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (root.path().join(OLD_ID), root.path().join(NEW_ID));
        assert_eq!(move_dir(&old, &new), Ok(Outcome::NothingToMove));
        assert!(!new.exists());
    }

    #[test]
    fn creates_missing_parent() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("a").join(OLD_ID);
        let new = root.path().join("b/c").join(NEW_ID);
        write(&old.join("vault.sqlite"), "vault");
        assert_eq!(move_dir(&old, &new), Ok(Outcome::Moved));
        assert_eq!(read(&new.join("vault.sqlite")), "vault");
    }

    /// Old's parent read-only, so the rename fails.
    #[cfg(unix)]
    fn locked_pair(root: &Path) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let old = root.join("ro").join(OLD_ID);
        write(&old.join("vault.sqlite"), "vault");
        fs::set_permissions(root.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
        (old, root.join(NEW_ID))
    }

    #[cfg(unix)]
    fn unlock(root: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failure_creates_nothing() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = locked_pair(root.path());
        let result = move_dir(&old, &new);
        unlock(root.path());
        assert!(result.is_err(), "{result:?}");
        assert!(!new.exists());
        assert_eq!(read(&old.join("vault.sqlite")), "vault");
    }

    /// The Windows case the retry is for: a lingering process (WebView2) holds
    /// a file inside the old dir open without sharing.
    #[cfg(windows)]
    #[test]
    fn failure_creates_nothing_while_a_file_is_open() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (root.path().join(OLD_ID), root.path().join(NEW_ID));
        write(&old.join("vault.sqlite"), "vault");
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(old.join("vault.sqlite"))
            .unwrap();
        let result = run_pairs(&[(old.clone(), new.clone())], 2, Duration::from_millis(1));
        drop(held);
        let message = result.unwrap_err();
        assert!(
            message.starts_with("Quit Tether, then open Termlyx again. ("),
            "{message}"
        );
        assert!(!new.exists());
        assert_eq!(read(&old.join("vault.sqlite")), "vault");
        // Once the other process lets go, the next launch moves it.
        assert_eq!(
            run_pairs(&[(old.clone(), new.clone())], 1, Duration::ZERO),
            Ok(())
        );
        assert_eq!(read(&new.join("vault.sqlite")), "vault");
    }

    #[cfg(unix)]
    #[test]
    fn run_gives_up_with_the_quit_message() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = locked_pair(root.path());
        let result = run_pairs(&[(old, new.clone())], 3, Duration::from_millis(1));
        unlock(root.path());
        let message = result.unwrap_err();
        assert!(
            message.starts_with("Quit Tether, then open Termlyx again. ("),
            "{message}"
        );
        assert!(message.ends_with(')'), "{message}");
        assert!(!new.exists());
    }

    #[test]
    fn run_moves_every_pair_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let pairs: Vec<_> = ["a", "b"]
            .iter()
            .map(|base| {
                (
                    root.path().join(base).join(OLD_ID),
                    root.path().join(base).join(NEW_ID),
                )
            })
            .collect();
        write(&pairs[0].0.join("vault.sqlite"), "vault");
        write(&pairs[1].0.join("LocalStorage"), "settings");
        assert_eq!(run_pairs(&pairs, 1, Duration::ZERO), Ok(()));
        assert_eq!(run_pairs(&pairs, 1, Duration::ZERO), Ok(()));
        assert_eq!(read(&pairs[0].1.join("vault.sqlite")), "vault");
        assert_eq!(read(&pairs[1].1.join("LocalStorage")), "settings");
    }

    fn env_of(vars: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> {
        let vars: Vec<(String, OsString)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_os_str().to_owned()))
            .collect();
        move |key| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    fn pair(base: &Path) -> (PathBuf, PathBuf) {
        (base.join(OLD_ID), base.join(NEW_ID))
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn data_pairs_mac_paths() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        assert_eq!(
            data_pairs_from(env_of(&[("HOME", h)]), NEW_ID),
            vec![
                pair(&h.join("Library/Application Support")),
                pair(&h.join("Library/WebKit"))
            ]
        );
        assert_eq!(data_pairs_from(env_of(&[]), NEW_ID), vec![]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn data_pairs_linux_paths() {
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let h = home.path();
        assert_eq!(
            data_pairs_from(env_of(&[("HOME", h)]), NEW_ID),
            vec![pair(&h.join(".local/share"))]
        );
        assert_eq!(
            data_pairs_from(
                env_of(&[("HOME", h), ("XDG_DATA_HOME", xdg.path())]),
                NEW_ID
            ),
            vec![pair(xdg.path())]
        );
        // A relative XDG_DATA_HOME is ignored (XDG base dir spec).
        assert_eq!(
            data_pairs_from(
                env_of(&[("HOME", h), ("XDG_DATA_HOME", Path::new("rel"))]),
                NEW_ID
            ),
            vec![pair(&h.join(".local/share"))]
        );
        assert_eq!(data_pairs_from(env_of(&[]), NEW_ID), vec![]);
    }

    #[cfg(windows)]
    #[test]
    fn data_pairs_windows_paths() {
        let roaming = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        assert_eq!(
            data_pairs_from(
                env_of(&[("APPDATA", roaming.path()), ("LOCALAPPDATA", local.path())]),
                NEW_ID
            ),
            vec![pair(roaming.path()), pair(local.path())]
        );
        // A missing variable skips only its pair.
        assert_eq!(
            data_pairs_from(env_of(&[("LOCALAPPDATA", local.path())]), NEW_ID),
            vec![pair(local.path())]
        );
    }
}
