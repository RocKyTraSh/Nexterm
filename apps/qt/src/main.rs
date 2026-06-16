//! Qt frontend — placeholder.
//!
//! The Qt/QML frontend is built on top of `rrs-ui-common::AppCore` and is
//! introduced in v0.2 (see apps/qt/README.md for the chosen approach and
//! rationale). This binary exists so the workspace builds end-to-end; it does
//! not yet start a GUI.

fn main() {
    println!(
        "{} - Qt frontend is not implemented yet.\n\
         See apps/qt/README.md for the architecture (cxx-qt + QML over the Rust core).\n\
         Use the `rrs` CLI (apps/cli) to exercise the core in the meantime.",
        rrs_platform::APP_NAME
    );
}
