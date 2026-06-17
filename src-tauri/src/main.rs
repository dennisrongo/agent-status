// Prevent a console window from flashing on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    agent_status_lib::run();
}
