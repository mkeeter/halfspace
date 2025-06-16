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

struct ChangedState {
    time: std::time::Instant,
    state: WorldState,
}

pub struct Undo {
    /// The undo buffer always contains >= 1 item
    undo: nonempty::NonEmpty<UndoState>,
    redo: Vec<UndoState>,
    last_changed: Option<ChangedState>,
}

const CHANGE_TIME: std::time::Duration = std::time::Duration::from_millis(500);

impl Undo {
    pub fn new(world: &World) -> Self {
        Undo {
            undo: nonempty::NonEmpty::new(UndoState {
                state: world.into(),
                saved: false,
            }),
            redo: vec![],
            last_changed: None,
        }
    }

    pub fn has_undo(&self, world: &World) -> bool {
        match self.undo.len() {
            0 => unreachable!("nonempty vector cannot be empty"),
            1 => world != &self.undo.last().state,
            _ => true,
        }
    }

    pub fn has_redo(&self, world: &World) -> bool {
        !self.redo.is_empty() && world == &self.undo.last().state
    }

    pub fn undo(&mut self, world: &World) -> Option<&WorldState> {
        if self.has_undo(world) {
            let last = self.undo.last();
            if world == &last.state {
                self.undo.pop().unwrap();
            }
            self.redo.push(UndoState {
                state: world.into(),
                saved: false,
            });
            Some(&self.undo.last().state)
        } else {
            None
        }
    }

    pub fn redo(&mut self, world: &World) -> Option<&WorldState> {
        // If the current state of the world differs from the value on top of
        // the undo stack, then we've changed and the redo stack is no longer
        // valid.
        if !self.undo.is_empty() && world != &self.undo.last().state {
            self.redo.clear();
            None
        } else if let Some(state) = self.redo.pop() {
            self.undo.push(state);
            Some(&self.undo.last().state)
        } else {
            None
        }
    }

    /// Update the state, creating a checkpoint when things are stable
    pub fn feed_state(&mut self, world: &World) {
        let prev = self.undo.last();
        if world != &prev.state {
            match &mut self.last_changed {
                None => {
                    debug!("creating last_changed");
                    self.last_changed = Some(ChangedState {
                        time: std::time::Instant::now(),
                        state: world.into(),
                    })
                }
                Some(t) => {
                    if world != &t.state {
                        // If the value is in flux, then reset the timer
                        t.state = world.into();
                        t.time = std::time::Instant::now();
                    } else if t.time.elapsed() > CHANGE_TIME {
                        // If the value is stable, then create an undo point
                        debug!("creating undo point due to changes");
                        let t = self.last_changed.take().unwrap();
                        self.undo.push(UndoState {
                            state: t.state,
                            saved: false,
                        });
                        self.redo.clear();
                        self.last_changed = None;
                    }
                }
            }
        }
    }

    /// Forcibly create an undo point if the state has changed
    pub fn checkpoint(&mut self, world: &World) {
        let prev = self.undo.last();
        if world != &prev.state {
            debug!("creating undo point due to checkpoint");
            self.undo.push(UndoState {
                state: WorldState::from(world),
                saved: false,
            });
            self.redo.clear();
        }
        self.last_changed = None;
    }

    /// Mark the current state as *saved*
    ///
    /// Note that this takes a [`WorldState`] instead of a `&World`; we have to
    /// make a `WorldState` when saving the file to disk, so we might as well
    /// reuse it.
    pub fn mark_saved(&mut self, state: WorldState) {
        let prev = self.undo.last_mut();
        if state == prev.state {
            debug!("marking previous undo point as saved");
            prev.saved = true;
        } else {
            debug!("pushing a new saved undo point");
            self.undo.push(UndoState { state, saved: true });
            self.redo.clear();
            self.last_changed = None;
        }
    }
}
