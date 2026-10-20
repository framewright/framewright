//! Timeline edits made of several commands, built here so the app, scripts
//! and tests make them the same way. Each returns the commands for one undo
//! step; applying them is the caller's business.

use fw_core::{NodeId, Time};

use crate::command::{Command, copy_id};
use crate::document::Document;
use crate::node::Node;
use crate::param::Param;
use crate::timeline::Placement;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EditError {
    #[error("{0} isn't a clip on a timeline")]
    NotAClip(NodeId),
    #[error("the split point isn't inside the clip")]
    OutsideClip,
    #[error("time out of range: {0}")]
    Time(#[from] fw_core::TimeError),
}

/// The clip's placement and the timeline it's on.
fn placement(doc: &Document, clip: NodeId) -> Result<(NodeId, &Placement), EditError> {
    let timeline = doc.parent(clip).ok_or(EditError::NotAClip(clip))?;
    let placement = doc
        .timeline(timeline)
        .and_then(|t| t.clips.get(&clip))
        .ok_or(EditError::NotAClip(clip))?;
    Ok((timeline, placement))
}

/// Splits `clip` at parent time `at` into two clips: `clip` keeps the part
/// before, and a copy called `new_id` takes the part after. Keyframes on the
/// clip and everything inside it are rewritten so every frame looks the
/// same as before.
pub fn split_clip(
    doc: &Document,
    clip: NodeId,
    at: Time,
    new_id: NodeId,
) -> Result<Vec<Command>, EditError> {
    let (_, whole) = placement(doc, clip)?;
    if !(whole.start < at && at < whole.end()) {
        return Err(EditError::OutsideClip);
    }
    let offset = at.checked_sub(whole.start)?;
    let before = Placement {
        duration: offset,
        ..whole.clone()
    };
    let after = Placement {
        start: at,
        duration: whole.duration.checked_sub(offset)?,
        source_in: whole
            .source_in
            .checked_add(offset.checked_mul(whole.speed)?)?,
        ..whole.clone()
    };
    let mut commands = vec![
        Command::SetPlacement {
            node: clip,
            placement: before,
        },
        Command::DuplicateNode {
            id: clip,
            new_id,
            placement: Some(after),
        },
    ];
    // Keys inside the clip's graph are measured on the clip's clock too.
    for id in doc.subtree(clip) {
        let Some(Node::Known(node)) = doc.node(id) else {
            continue;
        };
        for (param, value) in &node.params {
            if !matches!(value, Param::Keyframed(_)) {
                continue;
            }
            let (first, second) = value.split(offset, whole.duration)?;
            let set = |node, value| Command::SetParam {
                node,
                param: param.clone(),
                value: Some(value),
            };
            commands.push(set(id, first));
            commands.push(set(copy_id(clip, new_id, id), second));
        }
    }
    Ok(commands)
}

/// Removes `clip` and moves every later clip on its track earlier by its
/// length, closing the gap.
pub fn ripple_delete(doc: &Document, clip: NodeId) -> Result<Vec<Command>, EditError> {
    let (timeline, removed) = placement(doc, clip)?;
    let mut later: Vec<(NodeId, &Placement)> = doc
        .timeline(timeline)
        .expect("placement found it")
        .clips
        .iter()
        .filter(|(_, p)| p.track == removed.track && p.start >= removed.end())
        .map(|(id, p)| (*id, p))
        .collect();
    // Earliest first, so each moves into space already freed.
    later.sort_by_key(|(_, p)| p.start);
    let mut commands = vec![Command::RemoveNode { id: clip }];
    for (id, p) in later {
        commands.push(Command::SetPlacement {
            node: id,
            placement: Placement {
                start: p.start.checked_sub(removed.duration)?,
                ..p.clone()
            },
        });
    }
    Ok(commands)
}
