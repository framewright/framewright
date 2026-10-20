//! Watches the plugin folders, so a saved shader shows at once.
//!
//! A thread looks at the files every half second and says when any changed.
//! TODO: inotify, once there's more to watch than a few small folders.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const EVERY: Duration = Duration::from_millis(500);

/// What the files in the plugin folders look like: each one's path, when it
/// was last written and its size.
type Fingerprint = Vec<(PathBuf, Option<SystemTime>, u64)>;

fn fingerprint(folders: &[PathBuf]) -> Fingerprint {
    fn add(found: &mut Fingerprint, folder: &Path, depth: u32) {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                // A plugin folder, or a folder of them.
                if depth < 2 {
                    add(found, &entry.path(), depth + 1);
                }
            } else {
                found.push((entry.path(), meta.modified().ok(), meta.len()));
            }
        }
    }
    let mut found = vec![];
    for folder in folders {
        add(&mut found, folder, 0);
    }
    found.sort();
    found
}

/// Calls `changed` on a thread of its own whenever a file in `folders`
/// changes, appears or goes.
pub fn watch(folders: Vec<PathBuf>, changed: impl Fn() + Send + 'static) {
    if folders.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("fw-plugin-watch".into())
        .spawn(move || {
            let mut last = fingerprint(&folders);
            loop {
                std::thread::sleep(EVERY);
                let now = fingerprint(&folders);
                if now != last {
                    last = now;
                    changed();
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("framewright: not watching plugins: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_edited_added_and_removed_files() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("tint");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join("shader.wgsl"), "return c;").unwrap();
        let folders = vec![dir.path().to_owned(), dir.path().join("missing")];
        let before = fingerprint(&folders);
        assert_eq!(before.len(), 1);
        std::fs::write(plugin.join("shader.wgsl"), "return c * 2.0;").unwrap();
        let edited = fingerprint(&folders);
        assert_ne!(edited, before, "a different size at least");
        std::fs::write(plugin.join("plugin.toml"), "").unwrap();
        assert_eq!(fingerprint(&folders).len(), 2);
        std::fs::remove_dir_all(&plugin).unwrap();
        assert!(fingerprint(&folders).is_empty());
    }

    #[test]
    fn says_when_a_saved_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("tint");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join("shader.wgsl"), "return c;").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        watch(vec![dir.path().to_owned()], move || {
            let _ = tx.send(());
        });
        std::thread::sleep(EVERY * 2);
        assert!(rx.try_recv().is_err(), "nothing changed yet");
        std::fs::write(plugin.join("shader.wgsl"), "return c * 2.0;").unwrap();
        assert!(rx.recv_timeout(EVERY * 6).is_ok(), "the change was noticed");
    }
}
