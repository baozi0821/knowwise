// Desktop entry point; hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    knowwise_lib::app_shell::command_registry::run();
}
