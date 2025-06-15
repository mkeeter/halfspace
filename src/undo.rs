use crate::state::WorldState;
use log::debug;

#[derive(Clone)]
struct UndoState {
    state: WorldState,
    saved: bool,
}

pub struct Undo {
    undo: Vec<UndoState>,
    redo: Vec<UndoState>,
    last_changed: std::time::Instant,
}

impl Undo {
    pub fn new() -> Self {
        Undo {
            undo: vec![],
            redo: vec![],
            last_changed: std::time::Instant::now(),
        }
    }

    pub fn undo(&mut self, state: &WorldState) -> Option<WorldState> {
        while let Some(out) = self.undo.pop() {
            self.redo.push(out.clone());
            if &out.state != state {
                return Some(out.state);
            }
        }
        None
    }

    pub fn redo(&mut self, state: &WorldState) -> Option<WorldState> {
        while let Some(out) = self.redo.pop() {
            self.undo.push(out.clone());
            if &out.state != state {
                return Some(out.state);
            }
        }
        None
    }

    /// Update the state, creating a checkpoint when things are stable
    pub fn feed_state(&mut self, state: &WorldState) {
        if let Some(prev) = self.undo.last() {
            if state != &prev.state {
                if self.last_changed.elapsed()
                    > std::time::Duration::from_millis(200)
                {
                    debug!("creating undo point due to changes");
                    self.undo.push(UndoState {
                        state: state.clone(),
                        saved: false,
                    });
                    self.redo.clear();
                }
                self.last_changed = std::time::Instant::now();
            }
        } else {
            debug!("creating undo point due to empty buffer");
            self.undo.push(UndoState {
                state: state.clone(),
                saved: false,
            });
            self.last_changed = std::time::Instant::now();
        }
    }

    /// Forcibly create an undo point if the state has changed
    pub fn checkpoint(&mut self, state: &WorldState) {
        if let Some(prev) = self.undo.last() {
            if state != &prev.state {
                debug!("creating undo point due to checkpoint");
                self.undo.push(UndoState {
                    state: state.clone(),
                    saved: false,
                });
                self.redo.clear();
            }
            self.last_changed = std::time::Instant::now();
        } else {
            debug!("creating undo point due to checkpoint on empty buffer");
            self.undo.push(UndoState {
                state: state.clone(),
                saved: false,
            });
            self.last_changed = std::time::Instant::now();
        }
    }

    /// Mark the current state as *saved*
    pub fn mark_saved(&mut self, state: &WorldState) {
        if self.undo.last_mut().is_some_and(|v| &v.state == state) {
            self.undo.last_mut().unwrap().saved = true;
        } else {
            self.undo.push(UndoState {
                state: state.clone(),
                saved: true,
            });
        }
    }
}
