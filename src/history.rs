//! undo and redo per buffer. every step has an id, so "saved" means a
//! step, not a stack length.

use crate::edit::Change;

/// one undo step: changes in apply order.
#[derive(Clone, Debug)]
pub(crate) struct Step {
    pub id: u64,
    pub changes: Vec<Change>,
    /// a later commit may fold into this step.
    pub open: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    next_id: u64,
    saved: u64,
}

/// state with nothing applied.
const ROOT: u64 = 0;

impl History {
    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// id of the state the text is in now.
    pub fn state(&self) -> u64 {
        self.undo.last().map_or(ROOT, |s| s.id)
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.state();
        if let Some(last) = self.undo.last_mut() {
            last.open = false;
        }
    }

    pub fn is_saved(&self) -> bool {
        self.state() == self.saved
    }

    /// records a commit. `join` folds it into the last step if that one is open.
    pub fn record(&mut self, changes: Vec<Change>, join: bool) {
        self.redo.clear();
        let id = self.take_id();
        match self.undo.last_mut() {
            Some(last) if join && last.open => {
                last.changes.extend(changes);
                // new content, new id, so an old saved mark can't match it
                last.id = id;
            }
            _ => self.undo.push(Step {
                id,
                changes,
                open: join,
            }),
        }
    }

    /// stops the last step from taking more joins.
    pub fn close(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.open = false;
        }
    }

    /// changes that undo the last step, in apply order.
    pub fn undo(&mut self) -> Option<Vec<Change>> {
        let mut step = self.undo.pop()?;
        step.open = false;
        let back = step.changes.iter().rev().map(Change::inverted).collect();
        self.redo.push(step);
        Some(back)
    }

    /// changes that redo the last undone step, in apply order.
    pub fn redo(&mut self) -> Option<Vec<Change>> {
        let step = self.redo.pop()?;
        let forward = step.changes.clone();
        self.undo.push(step);
        Some(forward)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}
