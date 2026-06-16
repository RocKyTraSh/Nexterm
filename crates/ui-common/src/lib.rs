//! `rrs-ui-common`: UI-agnostic application logic shared by all frontends —
//! the [`AppCore`] facade plus multi-exec, macros, edit-conflict detection, and
//! command-safety helpers.

pub mod app;
pub mod conflict;
pub mod macros;
pub mod multiexec;
pub mod safety;

pub use app::AppCore;
pub use conflict::{check_conflict, ConflictCheck, RemoteSnapshot};
pub use macros::{Macro, MacroStep};
pub use multiexec::{prepare_broadcast, BroadcastDecision, MultiExecGroup};
pub use safety::{looks_like_secret, scan_dangerous, DangerWarning};
