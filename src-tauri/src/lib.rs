pub mod collectors;
pub mod core;
pub mod events_file;
pub mod installer;

pub fn agentboard_dir() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join(".agentboard")
}
pub fn events_file_now_ms() -> u64 { crate::core::types::now_ms() }
pub fn events_file_handle_input(input: &str, ts: u64) -> Option<String> { events_file::handle_input(input, ts) }
pub fn events_file_append_event(line: &str, dir: &std::path::Path) -> std::io::Result<()> { events_file::append_event(line, dir) }

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
