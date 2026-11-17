pub mod collectors;
pub mod core;
pub mod events_file;
pub mod installer;
pub mod platform;

use crate::core::history::History;
use crate::core::machine::Machine;
use crate::core::types::*;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

/// v1 无 config 门控（R8），Machine 固定用 15 分钟完成推断阈值。
const DEFAULT_DONE_AFTER_MS: u64 = 15 * 60 * 1000;
const TRAY_ID: &str = "agentboard-tray";

pub fn agentboard_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agentboard")
}
pub fn claude_settings_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("settings.json")
}
pub fn claude_projects_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}
pub fn zcode_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zcode")
        .join("cli")
        .join("db")
        .join("db.sqlite")
}
pub fn events_file_now_ms() -> u64 {
    crate::core::types::now_ms()
}
pub fn events_file_handle_input(input: &str, ts: u64) -> Option<String> {
    events_file::handle_input(input, ts)
}
pub fn events_file_append_event(line: &str, dir: &std::path::Path) -> std::io::Result<()> {
    events_file::append_event(line, dir)
}

pub struct AppState {
    pub machine: Arc<Mutex<Machine>>,
}

#[tauri::command]
fn get_sessions(state: tauri::State<AppState>) -> Vec<SessionSnapshot> {
    state
        .machine
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshots()
}

#[tauri::command]
fn get_source_status() -> serde_json::Value {
    serde_json::json!({
        "claude_hooks": installer::ours_registered(&claude_settings_path()),
        "claude_transcripts": claude_projects_dir().exists(),
        "zcode_db": zcode_db_path().exists(),
    })
}

// R1 裁定：hook exe 与主程序同目录（dev=target/debug，安装=安装目录）。
#[tauri::command]
fn jump(
    app: tauri::AppHandle,
    state: tauri::State<AppState>,
    session_id: String,
) -> Result<String, String> {
    let snap = state
        .machine
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshots()
        .into_iter()
        .find(|s| s.session_id == session_id)
        .ok_or("会话不存在")?;
    let p = platform::jump_payload(&snap);
    if let Some((prog, args)) = &p.spawn {
        std::process::Command::new(prog)
            .args(args)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard()
        .write_text(p.clipboard)
        .map_err(|e| e.to_string())?;
    Ok(p.note.to_string())
}

#[tauri::command]
fn get_config() -> core::config::Config {
    core::config::load()
}

#[tauri::command]
fn set_config(cfg: core::config::Config) -> Result<(), String> {
    core::config::save(&cfg).map_err(|e| e.to_string())
}

#[tauri::command]
fn install_hooks_cmd() -> Result<serde_json::Value, String> {
    let hook_exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("无法定位安装目录")?
        .join("agentboard-hook.exe");
    if !hook_exe.exists() {
        return Err("未找到 agentboard-hook.exe，请先完整构建".into());
    }
    let report = installer::install_hooks(
        &hook_exe,
        &claude_settings_path(),
        &agentboard_dir().join("backups"),
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "backupPath": report.backup_path.to_string_lossy(),
        "changed": report.changed,
    }))
}

// ---------- 装配 ----------

/// 采集线程 panic 不外扩散：整体 catch_unwind，只留一行日志。
fn spawn_guarded<F: FnOnce() + Send + 'static>(name: &'static str, f: F) {
    std::thread::spawn(move || {
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_ok();
        if !ok {
            eprintln!("[agentboard] 采集线程 {name} 异常退出");
        }
    });
}

fn send_all(tx: &mpsc::Sender<UnifiedEvent>, events: Vec<UnifiedEvent>) {
    for e in events {
        if tx.send(e).is_err() {
            break;
        } // 消费端已死（应用退出）
    }
}

/// ① 转录监听：启动时全量 poll 一次（发现既有会话），之后 notify-debouncer-full
///    递归 watch ~/.claude/projects，debounce 到手后再 poll；watch 建不起来则退化为 4s 轮询。
fn spawn_transcript_watcher(tx: mpsc::Sender<UnifiedEvent>) {
    spawn_guarded("claude-transcripts", move || {
        use notify_debouncer_full::{
            DebounceEventResult, new_debouncer, notify::RecursiveMode, notify::Watcher,
        };
        let root = claude_projects_dir();
        let mut poller = collectors::claude_transcripts::TranscriptPoller::new(root.clone());
        send_all(&tx, poller.poll(now_ms()));

        let (sig_tx, sig_rx) = mpsc::channel::<()>();
        let debouncer = new_debouncer(
            Duration::from_secs(2),
            None,
            move |res: DebounceEventResult| {
                if let Ok(events) = res {
                    if !events.is_empty() {
                        let _ = sig_tx.send(());
                    }
                }
            },
        );
        if let Ok(mut deb) = debouncer {
            if deb.watcher().watch(&root, RecursiveMode::Recursive).is_ok() {
                // watch 建立成功：信号驱动
                while sig_rx.recv().is_ok() {
                    send_all(&tx, poller.poll(now_ms()));
                }
                return;
            }
        }
        // 兜底：debouncer/watch 失败 → 4s 轮询
        loop {
            send_all(&tx, poller.poll(now_ms()));
            std::thread::sleep(Duration::from_secs(4));
        }
    });
}

/// ② hook 事件：2s 间隔轮询 events 文件（文件不存在安全返回空）。
fn spawn_hook_poller(tx: mpsc::Sender<UnifiedEvent>) {
    spawn_guarded("claude-hooks", move || {
        let mut poller = collectors::claude_events::HookEventsPoller::new(
            agentboard_dir().join("events").join("claude-code.jsonl"),
        );
        loop {
            send_all(&tx, poller.poll());
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

/// ③ ZCode：4s 间隔 poll；open 失败（源离线）退避 30s 重试。
fn spawn_zcode_poller(tx: mpsc::Sender<UnifiedEvent>) {
    spawn_guarded("zcode", move || {
        let mut poller: Option<collectors::zcode_db::ZcodePoller> = None;
        loop {
            if poller.is_none() {
                poller = collectors::zcode_db::ZcodePoller::open(&zcode_db_path()).ok();
                if poller.is_none() {
                    std::thread::sleep(Duration::from_secs(30));
                    continue;
                }
            }
            if let Some(p) = poller.as_mut() {
                send_all(&tx, p.poll(now_ms()));
            }
            std::thread::sleep(Duration::from_secs(4));
        }
    });
}

fn build_tray_menu(
    app: &AppHandle,
    waiting: usize,
    running: usize,
) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    tauri::menu::MenuBuilder::new(app)
        .item(
            &tauri::menu::MenuItemBuilder::with_id(
                "counts",
                format!("等待 {waiting} · 运行 {running}"),
            )
            .enabled(false)
            .build(app)?,
        )
        .separator()
        .item(&tauri::menu::MenuItemBuilder::with_id("open", "打开看板").build(app)?)
        .separator()
        .item(&tauri::menu::MenuItemBuilder::with_id("quit", "退出").build(app)?)
        .build()
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn update_tray(app: &AppHandle, snaps: &[SessionSnapshot]) {
    let waiting = snaps
        .iter()
        .filter(|s| matches!(s.status, SessionStatus::Waiting(_)))
        .count();
    let running = snaps
        .iter()
        .filter(|s| s.status == SessionStatus::Running)
        .count();
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Ok(menu) = build_tray_menu(app, waiting, running) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

/// 通知：should_notify 即发，受设置页 notify_enabled 门控（每次通知读一次小 JSON，频率低可接受）；body 只放 repo。
fn send_notification(app: &AppHandle, snap: &SessionSnapshot) {
    if !core::config::load().notify_enabled {
        return;
    }
    let title = match &snap.status {
        SessionStatus::Waiting(_) => format!("{} 在等你处理", snap.agent.label()),
        SessionStatus::Failed => format!("{} 任务失败", snap.agent.label()),
        SessionStatus::Done => format!("{} 任务完成", snap.agent.label()),
        SessionStatus::Running => return,
    };
    let _ = app
        .notification()
        .builder()
        .title(title)
        .body(&snap.repo)
        .show();
}

/// ④ 核心：单事件处理（含每 5s 注入的 Tick）。
/// 每个 Update：history.upsert（每 200 次 prune(1000)）→ should_notify 发通知；
/// 一批 Update 结束后统一 emit "sessions-changed" + 重建托盘菜单计数。
fn handle_event(
    app: &AppHandle,
    machine: &Mutex<Machine>,
    history: &Mutex<History>,
    ev: &UnifiedEvent,
    upserts: &mut usize,
) {
    let updates = machine
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .process(ev);
    if updates.is_empty() {
        return;
    }
    let history = history.lock().unwrap_or_else(|p| p.into_inner());
    for u in &updates {
        if history.upsert(&u.snapshot).is_ok() {
            *upserts += 1;
            if *upserts % 200 == 0 {
                let _ = history.prune(1000);
            }
        }
        if u.should_notify {
            send_notification(app, &u.snapshot);
        }
    }
    drop(history);
    let snaps = machine
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshots();
    let _ = app.emit("sessions-changed", snaps.clone());
    update_tray(app, &snaps);
}

fn spawn_core(
    app: AppHandle,
    rx: mpsc::Receiver<UnifiedEvent>,
    machine: Arc<Mutex<Machine>>,
    history: Arc<Mutex<History>>,
) {
    spawn_guarded("core", move || {
        let mut last_tick = Instant::now();
        let mut upserts = 0usize;
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(ev) => handle_event(&app, &machine, &history, &ev, &mut upserts),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if last_tick.elapsed() >= Duration::from_secs(5) {
                        last_tick = Instant::now();
                        handle_event(
                            &app,
                            &machine,
                            &history,
                            &UnifiedEvent::Tick { now: now_ms() },
                            &mut upserts,
                        );
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break, // 所有发送端已 drop（应用退出）
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (tx, rx) = mpsc::channel::<UnifiedEvent>();
    let machine = Arc::new(Mutex::new(Machine::new(DEFAULT_DONE_AFTER_MS)));
    let history = Arc::new(Mutex::new(
        match History::open(&agentboard_dir().join("history.sqlite")) {
            Ok(h) => h,
            // 磁盘打不开就退化为内存历史，看板功能不受损
            Err(_) => History::with_connection(
                rusqlite::Connection::open_in_memory().expect("in-memory sqlite"),
            )
            .expect("history schema"),
        },
    ));

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            machine: machine.clone(),
        })
        .setup(move |app| {
            // ⑤ 托盘：默认图标 + 初始菜单（等待 0 · 运行 0 / 打开看板 / 退出）
            let mut tray = tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
                .tooltip("agentboard")
                .menu(&build_tray_menu(app.handle(), 0, 0)?)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            // ①②③ + ④：三个采集线程 + 核心消费线程
            spawn_transcript_watcher(tx.clone());
            spawn_hook_poller(tx.clone());
            spawn_zcode_poller(tx.clone());
            spawn_core(app.handle().clone(), rx, machine.clone(), history.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_sessions,
            get_source_status,
            jump,
            get_config,
            set_config,
            install_hooks_cmd
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
