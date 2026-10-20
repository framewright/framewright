//! Applying commands with undo and redo.

use std::sync::Arc;

use fw_core::NodeId;
use fw_plugin::Registry;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError};
use crate::document::Document;

/// Sent whenever the document changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Changed {
        revision: u64,
        cause: Cause,
        /// Nodes added, removed or edited. May contain duplicates.
        nodes: Vec<NodeId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    Edit { label: String },
    Undo { label: String },
    Redo { label: String },
}

/// One undoable step: a labelled group of commands.
#[derive(Debug, Clone)]
struct Step {
    label: String,
    /// Later edits in the same gesture join this step.
    gesture: Option<u64>,
    forward: Vec<Command>,
    /// Applied in order to undo the step.
    inverse: Vec<Command>,
}

/// A document plus its undo history.
pub struct Editor {
    doc: Document,
    registry: Arc<Registry>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    revision: u64,
}

impl Editor {
    pub fn new(doc: Document, registry: Arc<Registry>) -> Editor {
        Editor {
            doc,
            registry,
            undo: vec![],
            redo: vec![],
            revision: 0,
        }
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// Swaps in new node types, such as reloaded plugins. The document and
    /// history stay as they are: a node whose type changed is checked again
    /// when it's next edited.
    pub fn set_registry(&mut self, registry: Arc<Registry>) {
        self.registry = registry;
    }

    /// Increases by one with every change, including undo and redo.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Applies `commands` as one undoable step. Either all apply or none do.
    pub fn apply(
        &mut self,
        label: impl Into<String>,
        commands: Vec<Command>,
    ) -> Result<Event, CommandError> {
        self.apply_in_gesture(label, commands, None)
    }

    /// Like [`Editor::apply`], but edits that carry the same `gesture`, one
    /// straight after another, make one undoable step: a slider drag sends
    /// many, and is undone in one go. The step keeps its first label.
    pub fn apply_in_gesture(
        &mut self,
        label: impl Into<String>,
        commands: Vec<Command>,
        gesture: Option<u64>,
    ) -> Result<Event, CommandError> {
        let label = label.into();
        let mut inverse = self.run(&commands)?;
        let nodes = touched(&commands);
        match self.undo.last_mut() {
            Some(step) if gesture.is_some() && step.gesture == gesture && self.redo.is_empty() => {
                step.forward.extend(commands);
                // Undo this edit first, then the ones before it.
                inverse.append(&mut step.inverse);
                step.inverse = inverse;
            }
            _ => self.undo.push(Step {
                label: label.clone(),
                gesture,
                forward: commands,
                inverse,
            }),
        }
        self.redo.clear();
        Ok(self.changed(Cause::Edit { label }, nodes))
    }

    /// Undoes the last step. Returns `Ok(None)` when there is nothing to undo.
    pub fn undo(&mut self) -> Result<Option<Event>, CommandError> {
        let Some(step) = self.undo.pop() else {
            return Ok(None);
        };
        match self.run(&step.inverse) {
            Ok(_) => {
                let nodes = touched(&step.inverse);
                let label = step.label.clone();
                self.redo.push(step);
                Ok(Some(self.changed(Cause::Undo { label }, nodes)))
            }
            Err(e) => {
                self.undo.push(step);
                Err(e)
            }
        }
    }

    /// Redoes the last undone step. Returns `Ok(None)` when there is nothing to redo.
    pub fn redo(&mut self) -> Result<Option<Event>, CommandError> {
        let Some(step) = self.redo.pop() else {
            return Ok(None);
        };
        match self.run(&step.forward) {
            Ok(inverse) => {
                let nodes = touched(&step.forward);
                let label = step.label.clone();
                // A redone step never merges with later edits.
                self.undo.push(Step {
                    inverse,
                    gesture: None,
                    ..step
                });
                Ok(Some(self.changed(Cause::Redo { label }, nodes)))
            }
            Err(e) => {
                self.redo.push(step);
                Err(e)
            }
        }
    }

    /// Runs commands against a copy and swaps it in only if all succeed.
    /// Returns the inverse commands, in the order that undoes them.
    fn run(&mut self, commands: &[Command]) -> Result<Vec<Command>, CommandError> {
        let mut next = self.doc.clone();
        let mut inverse = Vec::with_capacity(commands.len());
        for command in commands {
            inverse.push(command.apply(&mut next, &self.registry)?);
        }
        inverse.reverse();
        self.doc = next;
        Ok(inverse)
    }

    fn changed(&mut self, cause: Cause, nodes: Vec<NodeId>) -> Event {
        self.revision += 1;
        Event::Changed {
            revision: self.revision,
            cause,
            nodes,
        }
    }
}

fn touched(commands: &[Command]) -> Vec<NodeId> {
    commands.iter().flat_map(Command::touched).collect()
}
