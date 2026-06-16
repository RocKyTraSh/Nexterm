//! `rrs-terminal`: terminal-side logic — output highlighting, alt-screen
//! detection, and (feature `pty`) a local-shell PTY backend.
//!
//! Full VT/xterm emulation (grid, cursor, scrollback) is performed by the
//! frontend's terminal widget; this crate provides the protocol-agnostic
//! pieces that are shared and unit-testable.

pub mod altscreen;
pub mod error;
pub mod highlight;

#[cfg(feature = "pty")]
pub mod pty;

pub use altscreen::AltScreenTracker;
pub use error::{Result, TerminalError};
pub use highlight::{
    builtin_profiles, HighlightProfile, HighlightRule, HighlightSpan, HighlightStyle,
    LineHighlighter,
};
