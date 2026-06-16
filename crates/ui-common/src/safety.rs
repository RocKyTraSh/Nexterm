//! Safety heuristics shared by multi-exec and the macro recorder.
//!
//! These are guard rails, not security guarantees: they reduce the chance of a
//! catastrophic fan-out command, or of accidentally recording a secret into a
//! macro. Conservative and easy to extend with more patterns.

use regex::Regex;
use std::sync::OnceLock;

/// A flagged command pattern and why it is risky.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DangerWarning {
    pub matched: String,
    pub reason: String,
}

fn danger_patterns() -> &'static [(&'static str, &'static str)] {
    &[
        (r"(?i)\brm\s+-[a-z]*r[a-z]*f", "recursive force delete"),
        (r"(?i)\bmkfs\b", "filesystem format"),
        (r"(?i)\bdd\b\s+if=", "raw disk write"),
        (r"(?i)\b(reboot|shutdown|poweroff|halt)\b", "power state change"),
        (r"(?i)\bsystemctl\s+(restart|stop|disable)\b", "service disruption"),
        (r":\(\)\s*\{.*\};\s*:", "fork bomb"),
        (r"(?i)>\s*/dev/sd", "write to block device"),
        (r"(?i)\bchmod\s+-R\s+777\b", "world-writable recursive chmod"),
    ]
}

fn compiled() -> &'static Vec<(Regex, &'static str)> {
    static CELL: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    CELL.get_or_init(|| {
        danger_patterns()
            .iter()
            .filter_map(|(p, reason)| Regex::new(p).ok().map(|r| (r, *reason)))
            .collect()
    })
}

/// Return warnings for any dangerous patterns found in `command`.
/// Used before broadcasting input to multiple sessions in multi-exec mode.
pub fn scan_dangerous(command: &str) -> Vec<DangerWarning> {
    compiled()
        .iter()
        .filter(|(re, _)| re.is_match(command))
        .map(|(re, reason)| DangerWarning {
            matched: re.find(command).map(|m| m.as_str().to_string()).unwrap_or_default(),
            reason: (*reason).to_string(),
        })
        .collect()
}

/// Heuristic: does this input look like it contains a secret we should not
/// store in a macro (token, key, or a `password`-ish assignment)?
pub fn looks_like_secret(input: &str) -> bool {
    static CELL: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = CELL.get_or_init(|| {
        [
            r"(?i)(password|passwd|secret|token|api[_-]?key)\s*[:=]",
            r"(?i)bearer\s+[a-z0-9._-]{10,}",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
            r"\bgh[pousr]_[A-Za-z0-9]{20,}\b",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
    });
    res.iter().any(|re| re.is_match(input))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_rm_rf() {
        let w = scan_dangerous("sudo rm -rf /var/log/old");
        assert!(w.iter().any(|w| w.reason.contains("delete")));
    }

    #[test]
    fn ignores_safe_command() {
        assert!(scan_dangerous("ls -la /etc").is_empty());
    }

    #[test]
    fn detects_secret_assignment() {
        assert!(looks_like_secret("export API_KEY=abc123"));
        assert!(!looks_like_secret("echo hello"));
    }
}
