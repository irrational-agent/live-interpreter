//! The window version of live-interpreter: no console, started from the desktop shortcut.
#![windows_subsystem = "windows"]

fn main() -> eframe::Result {
    live_interpreter::gui::run()
}
