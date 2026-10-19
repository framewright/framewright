//! Atomic file writes: a crash at any point leaves either the old file or the new one.
//!
//! TODO: rolling backups (design §9).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fw_plugin::Registry;

use crate::document::Document;
use crate::format::{self, LoadError};

/// The steps of an atomic write, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStep {
    WriteTemp,
    SyncTemp,
    Rename,
    SyncDir,
}

impl SaveStep {
    pub const ALL: [SaveStep; 4] = [
        SaveStep::WriteTemp,
        SaveStep::SyncTemp,
        SaveStep::Rename,
        SaveStep::SyncDir,
    ];
}

pub fn save(doc: &Document, path: &Path) -> io::Result<()> {
    write_atomic(path, format::to_json(doc).as_bytes())
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("cannot load {path}: {source}")]
    Load { path: PathBuf, source: LoadError },
}

pub fn load(path: &Path, registry: &Registry) -> Result<Document, OpenError> {
    let text = fs::read_to_string(path).map_err(|source| OpenError::Io {
        path: path.to_owned(),
        source,
    })?;
    format::from_json(&text, registry).map_err(|source| OpenError::Load {
        path: path.to_owned(),
        source,
    })
}

/// Writes `bytes` to `path` via a temporary file in the same directory.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, |_| Ok(()))
}

/// [`write_atomic`] with a hook called before each step, so tests can fail any step.
pub fn write_atomic_with(
    path: &Path,
    bytes: &[u8],
    mut before: impl FnMut(SaveStep) -> io::Result<()>,
) -> io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_owned(),
        _ => PathBuf::from("."),
    };
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(".saving");
    let temp = dir.join(temp_name);

    let result = (|| {
        before(SaveStep::WriteTemp)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        before(SaveStep::SyncTemp)?;
        file.sync_all()?;
        drop(file);
        before(SaveStep::Rename)?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return result;
    }
    before(SaveStep::SyncDir)?;
    File::open(&dir)?.sync_all()
}
