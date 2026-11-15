use std::fmt;
use std::sync::mpsc::Sender;
use tej_db_core::{ChangeSet, Hook};

/// Built-in audit log hook invoking a callback on every committed mutation.
pub struct AuditLogHook {
    callback: Box<dyn Fn(&ChangeSet) + Send + Sync + 'static>,
}

impl fmt::Debug for AuditLogHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuditLogHook").finish_non_exhaustive()
    }
}

impl AuditLogHook {
    pub fn new(callback: impl Fn(&ChangeSet) + Send + Sync + 'static) -> Self {
        Self {
            callback: Box::new(callback),
        }
    }
}

impl Hook for AuditLogHook {
    fn after_commit(&self, changes: &ChangeSet) {
        (self.callback)(changes);
    }
}

/// Built-in change feed hook broadcasting ChangeSets across an MPSC channel.
pub struct ChangeFeedHook {
    sender: parking_lot::Mutex<Sender<ChangeSet>>,
}

impl fmt::Debug for ChangeFeedHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChangeFeedHook").finish_non_exhaustive()
    }
}

impl ChangeFeedHook {
    pub fn new(sender: Sender<ChangeSet>) -> Self {
        Self {
            sender: parking_lot::Mutex::new(sender),
        }
    }
}

impl Hook for ChangeFeedHook {
    fn after_commit(&self, changes: &ChangeSet) {
        let sender = self.sender.lock();
        let _ = sender.send(changes.clone());
    }
}
