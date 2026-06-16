//! Terminal macro model: record input steps with optional delays, replay them,
//! and warn before persisting anything that looks like a secret.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::safety::looks_like_secret;

/// A single step in a macro.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MacroStep {
    /// Send literal input (e.g. a command plus newline).
    Input { text: String },
    /// Pause before the next step.
    Delay { millis: u64 },
    /// Wait until this substring is seen in the output (best-effort).
    WaitFor { needle: String },
}

/// A named, editable, serializable macro.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Macro {
    pub id: Uuid,
    pub name: String,
    pub steps: Vec<MacroStep>,
}

impl Macro {
    pub fn new(name: impl Into<String>) -> Self {
        Self { id: Uuid::new_v4(), name: name.into(), steps: Vec::new() }
    }

    /// Indices of steps whose input looks like a secret. The UI should warn
    /// (and offer to redact) before saving when this is non-empty.
    pub fn suspicious_steps(&self) -> Vec<usize> {
        self.steps
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                MacroStep::Input { text } if looks_like_secret(text) => Some(i),
                _ => None,
            })
            .collect()
    }
}

impl From<Duration> for MacroStep {
    fn from(d: Duration) -> Self {
        MacroStep::Delay { millis: d.as_millis() as u64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_secret_step() {
        let mut m = Macro::new("login");
        m.steps.push(MacroStep::Input { text: "whoami\n".into() });
        m.steps.push(MacroStep::Input {
            text: "export TOKEN=ghp_abcdefghijklmnopqrstuvwxyz123\n".into(),
        });
        assert_eq!(m.suspicious_steps(), vec![1]);
    }
}
