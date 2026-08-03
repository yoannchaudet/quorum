#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(exit_code) = quorum_lib::owned_process_entry() {
        std::process::exit(i32::from(exit_code != std::process::ExitCode::SUCCESS));
    }
    quorum_lib::run();
}
