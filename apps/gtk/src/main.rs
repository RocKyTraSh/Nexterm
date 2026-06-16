//! GTK frontend — placeholder.
//!
//! A secondary GTK4 frontend (gtk4-rs / Relm4 + vte4 terminal widget) is a v0.3
//! item, sharing the same `rrs-ui-common::AppCore`. This binary only prints a
//! pointer so the workspace builds end-to-end.

fn main() {
    println!(
        "{} - GTK frontend is not implemented yet (planned for v0.3). \
         Use the `rrs` CLI to exercise the core.",
        rrs_platform::APP_NAME
    );
}
