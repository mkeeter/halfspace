//! Undo and redo
//!
//! The implementation is inspired by [`egui::util::undoer::Undoer`], but is
//! specialized to our use case.
use crate::{state::WorldState, world::World};
use log::debug;

#[derive(Clone)]
struct UndoState {
    state: WorldState,
    saved: bool,
}

pub struct Undo {
    /// The undo buffer always contains >= 1 item
    undo: Vec<UndoState>,
    redo: Vec<UndoState>,
    last_changed: std::time::Instant,
}

impl Undo {
    pub fn new(world: &World) -> Self {
        Undo {
            undo: vec![UndoState {
                state: world.into(),
                saved: false,
            }],
            redo: vec![],
            last_changed: std::time::Instant::now(),
        }
    }

    pub fn has_undo(&self, world: &World) -> bool {
        match self.undo.len() {
            0 => panic!("undo must always have >= 1 item"),
            1 => world != &self.undo.last().unwrap().state,
            _ => true,
        }
    }

    pub fn has_redo(&self, world: &World) -> bool {
        !self.redo.is_empty() && world == &self.undo.last().unwrap().state
    }

    pub fn undo(&mut self, world: &World) -> Option<&WorldState> {
        if self.has_undo(world) {
            let last = self.undo.last().unwrap();
            if world == &last.state {
                self.undo.pop();
                assert!(!self.undo.is_empty());
            }
            self.redo.push(UndoState {
                state: world.into(),
                saved: false,
            });
            Some(&self.undo.last().unwrap().state)
        } else {
            None
        }
    }

    pub fn redo(&mut self, world: &World) -> Option<&WorldState> {
        // If the current state of the world differs from the value on top of
        // the undo stack, then we've changed and the redo stack is no longer
        // valid.
        if !self.undo.is_empty() && world != &self.undo.last().unwrap().state {
            self.redo.clear();
            None
        } else if let Some(state) = self.redo.pop() {
            self.undo.push(state);
            Some(&self.undo.last().unwrap().state)
        } else {
            None
        }
    }

    /// Update the state, creating a checkpoint when things are stable
    pub fn feed_state(&mut self, world: &World) {
        let prev = self.undo.last().unwrap();
        if world != &prev.state {
            if self.last_changed.elapsed()
                > std::time::Duration::from_millis(2000)
            {
                debug!("creating undo point due to changes");
                self.undo.push(UndoState {
                    state: WorldState::from(world),
                    saved: false,
                });
                self.redo.clear();
            }
            self.last_changed = std::time::Instant::now();
        }
    }

    /// Forcibly create an undo point if the state has changed
    pub fn checkpoint(&mut self, world: &World) {
        let prev = self.undo.last().unwrap();
        if world != &prev.state {
            debug!("creating undo point due to checkpoint");
            self.undo.push(UndoState {
                state: WorldState::from(world),
                saved: false,
            });
            self.redo.clear();
        }
        self.last_changed = std::time::Instant::now();
    }

    /// Mark the current state as *saved*
    ///
    /// Note that this takes a [`WorldState`] instead of a `&World`; we have to
    /// make a `WorldState` when saving the file to disk, so we might as well
    /// reuse it.
    pub fn mark_saved(&mut self, state: WorldState) {
        let prev = self.undo.last_mut().unwrap();
        if state == prev.state {
            debug!("marking previous undo point as saved");
            prev.saved = true;
        } else {
            debug!("pushing a new saved undo point");
            self.undo.push(UndoState { state, saved: true });
            self.redo.clear();
        }
    }
}
