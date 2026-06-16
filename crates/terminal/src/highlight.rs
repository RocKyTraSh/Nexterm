//! Terminal output highlighting model.
//!
//! Highlighting is applied **only to plain text lines** and is suppressed while
//! the alternate screen buffer is active (see [`crate::AltScreenTracker`]), so
//! it never corrupts full-screen TUIs like `vim`, `top`, or `htop`. Robust
//! SGR-aware highlighting of mixed control/text streams is a v0.3 item.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{Result, TerminalError};

/// A logical style applied to a matched span. Mapped to concrete colors by the
/// frontend, so themes stay a UI concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightStyle {
    Info,
    Warning,
    Error,
    Success,
    Address, // IP / MAC / network identifiers
    Accent,
}

/// A single highlight rule (serializable; can be toggled off).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightRule {
    pub name: String,
    pub pattern: String,
    pub style: HighlightStyle,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// A named collection of rules (e.g. a vendor preset).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightProfile {
    pub name: String,
    pub rules: Vec<HighlightRule>,
}

struct CompiledRule {
    style: HighlightStyle,
    regex: Regex,
}

/// A span of a line that matched a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub style: HighlightStyle,
}

/// Compiles enabled rules and produces highlight spans for plain text lines.
pub struct LineHighlighter {
    rules: Vec<CompiledRule>,
}

impl LineHighlighter {
    /// Compile the enabled rules from a profile.
    pub fn from_profile(profile: &HighlightProfile) -> Result<Self> {
        let mut rules = Vec::new();
        for r in profile.rules.iter().filter(|r| r.enabled) {
            let regex = Regex::new(&r.pattern).map_err(|source| TerminalError::BadRegex {
                name: r.name.clone(),
                source,
            })?;
            rules.push(CompiledRule {
                style: r.style,
                regex,
            });
        }
        Ok(Self { rules })
    }

    /// Return non-overlapping spans for a single plain-text line.
    ///
    /// `line` must not contain escape sequences — callers gate this on
    /// `AltScreenTracker::in_alt_screen()` being false and on the chunk being
    /// cooked text. First matching rule wins on overlap.
    pub fn spans(&self, line: &str) -> Vec<HighlightSpan> {
        let mut spans: Vec<HighlightSpan> = Vec::new();
        for rule in &self.rules {
            for m in rule.regex.find_iter(line) {
                let candidate = HighlightSpan {
                    start: m.start(),
                    end: m.end(),
                    style: rule.style,
                };
                if !spans.iter().any(|s| overlaps(s, &candidate)) {
                    spans.push(candidate);
                }
            }
        }
        spans.sort_by_key(|s| s.start);
        spans
    }
}

fn overlaps(a: &HighlightSpan, b: &HighlightSpan) -> bool {
    a.start < b.end && b.start < a.end
}

/// Built-in highlight presets. Network-vendor presets (Cisco, MikroTik, Huawei,
/// Juniper, Eltex, ZTE, ...) are added here as additional [`HighlightProfile`]s.
pub fn builtin_profiles() -> Vec<HighlightProfile> {
    vec![HighlightProfile {
        name: "generic-network".into(),
        rules: vec![
            HighlightRule {
                name: "ipv4".into(),
                pattern: r"\b(?:\d{1,3}\.){3}\d{1,3}\b".into(),
                style: HighlightStyle::Address,
                enabled: true,
            },
            HighlightRule {
                name: "mac".into(),
                pattern: r"\b(?:[0-9A-Fa-f]{2}[:-]){5}[0-9A-Fa-f]{2}\b".into(),
                style: HighlightStyle::Address,
                enabled: true,
            },
            HighlightRule {
                name: "error".into(),
                pattern: r"(?i)\b(error|failed|down|denied)\b".into(),
                style: HighlightStyle::Error,
                enabled: true,
            },
            HighlightRule {
                name: "warning".into(),
                pattern: r"(?i)\b(warn(?:ing)?|deprecated)\b".into(),
                style: HighlightStyle::Warning,
                enabled: true,
            },
            HighlightRule {
                name: "up-ok".into(),
                pattern: r"(?i)\b(up|ok|success|established)\b".into(),
                style: HighlightStyle::Success,
                enabled: true,
            },
        ],
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ip_and_status() {
        let profile = &builtin_profiles()[0];
        let hl = LineHighlighter::from_profile(profile).expect("compile");
        let spans = hl.spans("iface eth0 is up at 10.0.0.1");
        assert!(spans.iter().any(|s| s.style == HighlightStyle::Address));
        assert!(spans.iter().any(|s| s.style == HighlightStyle::Success));
    }

    #[test]
    fn rejects_bad_regex() {
        let profile = HighlightProfile {
            name: "bad".into(),
            rules: vec![HighlightRule {
                name: "oops".into(),
                pattern: "(".into(),
                style: HighlightStyle::Info,
                enabled: true,
            }],
        };
        assert!(LineHighlighter::from_profile(&profile).is_err());
    }
}
