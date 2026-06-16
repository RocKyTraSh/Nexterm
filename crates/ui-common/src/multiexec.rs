//! Multi-execution: fan one input out to several sessions, with per-session
//! exclusions and a danger gate.

use std::collections::HashSet;

use uuid::Uuid;

use crate::safety::{scan_dangerous, DangerWarning};

/// A set of sessions that receive master input, minus any excluded ones.
#[derive(Debug, Default, Clone)]
pub struct MultiExecGroup {
    members: HashSet<Uuid>,
    excluded: HashSet<Uuid>,
}

impl MultiExecGroup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, id: Uuid) {
        self.members.insert(id);
    }

    pub fn remove(&mut self, id: Uuid) {
        self.members.remove(&id);
        self.excluded.remove(&id);
    }

    /// Temporarily exclude a session without removing it from the group.
    pub fn set_excluded(&mut self, id: Uuid, excluded: bool) {
        if excluded {
            self.excluded.insert(id);
        } else {
            self.excluded.remove(&id);
        }
    }

    /// Sessions that should currently receive input.
    pub fn active_targets(&self) -> Vec<Uuid> {
        self.members.difference(&self.excluded).copied().collect()
    }
}

/// Outcome of preparing a multi-exec broadcast.
#[derive(Debug)]
pub enum BroadcastDecision {
    /// Safe to send to these targets.
    Ready { targets: Vec<Uuid> },
    /// Flagged; the UI must confirm before proceeding.
    NeedsConfirmation { targets: Vec<Uuid>, warnings: Vec<DangerWarning> },
}

/// Decide whether `input` can be broadcast immediately or needs confirmation.
///
/// `confirm_dangerous` mirrors the user setting; when true, flagged commands
/// require explicit confirmation in the UI before they are sent anywhere.
pub fn prepare_broadcast(
    group: &MultiExecGroup,
    input: &str,
    confirm_dangerous: bool,
) -> BroadcastDecision {
    let targets = group.active_targets();
    let warnings = scan_dangerous(input);
    if confirm_dangerous && !warnings.is_empty() {
        BroadcastDecision::NeedsConfirmation { targets, warnings }
    } else {
        BroadcastDecision::Ready { targets }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_and_targets() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut g = MultiExecGroup::new();
        g.add(a);
        g.add(b);
        g.set_excluded(b, true);
        assert_eq!(g.active_targets(), vec![a]);
    }

    #[test]
    fn dangerous_needs_confirmation() {
        let mut g = MultiExecGroup::new();
        g.add(Uuid::new_v4());
        match prepare_broadcast(&g, "reboot", true) {
            BroadcastDecision::NeedsConfirmation { warnings, .. } => assert!(!warnings.is_empty()),
            _ => panic!("expected confirmation"),
        }
    }
}
