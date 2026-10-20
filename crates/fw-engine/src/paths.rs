//! Project files on disk: making a new one, and the media paths inside, which
//! are relative to the project's folder so a project and its media can move
//! together.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use fw_core::id::{IdGen, RandomIds};
use fw_doc::{Command, Document, Editor, Param, TimelineSettings, Track, TrackId, TrackKind};
use fw_plugin::{MediaRef, ParamValue};

use crate::EngineError;

/// Writes a new, empty video project to `path`: a timeline with one video
/// track and one audio track.
pub fn create_project(path: &Path, settings: TimelineSettings) -> Result<(), EngineError> {
    let mut ids = RandomIds::default();
    let root = ids.next_id();
    let mut editor = Editor::new(
        Document::new_timeline(root, settings),
        Arc::new(fw_ops::builtin_registry()),
    );
    let track = |ids: &mut RandomIds, kind, name: &str, index| Command::AddTrack {
        timeline: root,
        track: Track {
            id: TrackId(ids.next_id()),
            kind,
            name: name.into(),
        },
        index,
    };
    let commands = vec![
        track(&mut ids, TrackKind::Video, "V1", 0),
        track(&mut ids, TrackKind::Audio, "A1", 1),
    ];
    editor.apply("New Project", commands)?;
    fw_doc::save::save(editor.doc(), path).map_err(|source| EngineError::Save {
        path: path.to_owned(),
        source,
    })
}

/// `path` without `.` and with `..` taken against what comes before, as far
/// as that goes without asking the file system.
fn tidy(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// An absolute, tidied form of `path`, resolving symbolic links where the
/// path exists.
fn absolute(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    tidy(&joined)
}

pub(crate) fn same_folder(a: &Path, b: &Path) -> bool {
    absolute(a) == absolute(b)
}

/// How a project in `folder` refers to `file`: relative to the folder.
pub(crate) fn reference(file: &Path, folder: &Path) -> String {
    let (file, folder) = (absolute(file), absolute(folder));
    let shared = file
        .components()
        .zip(folder.components())
        .take_while(|(a, b)| a == b)
        .count();
    // Only the root in common: a relative path would climb to it anyway.
    if shared <= 1 {
        return file.to_string_lossy().into_owned();
    }
    let mut relative = PathBuf::new();
    for _ in folder.components().skip(shared) {
        relative.push("..");
    }
    for part in file.components().skip(shared) {
        relative.push(part);
    }
    relative.to_string_lossy().into_owned()
}

/// Commands that rewrite every relative media path in `doc` from `old`, the
/// folder it was in, to `new`.
pub(crate) fn rebase(doc: &Document, old: &Path, new: &Path) -> Vec<Command> {
    let moved = |path: &str| -> Option<String> {
        let p = Path::new(path);
        (!path.is_empty() && p.is_relative()).then(|| reference(&old.join(p), new))
    };
    let mut commands = vec![];
    for (id, node) in doc.nodes() {
        let Some(known) = node.as_known() else {
            continue;
        };
        for (param, value) in &known.params {
            let Param::Constant(ParamValue::Media(media)) = value else {
                continue;
            };
            if let Some(path) = moved(&media.path) {
                commands.push(Command::SetParam {
                    node: id,
                    param: param.clone(),
                    value: Some(Param::Constant(ParamValue::Media(MediaRef { path }))),
                });
            }
        }
    }
    for path in doc.media_pool() {
        if let Some(new_path) = moved(path) {
            commands.push(Command::RemoveMedia { path: path.into() });
            commands.push(Command::AddMedia { path: new_path });
        }
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_are_relative_to_the_project() {
        let at = |file: &str, folder: &str| reference(Path::new(file), Path::new(folder));
        assert_eq!(at("/p/media/a.mp4", "/p"), "media/a.mp4");
        assert_eq!(at("/p/a.mp4", "/p/projects"), "../a.mp4");
        assert_eq!(at("/x/a.mp4", "/y"), "/x/a.mp4");
        assert_eq!(at("/p/./media/../a.mp4", "/p"), "a.mp4");
    }
}
