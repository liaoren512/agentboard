pub mod collectors;
pub mod core;
pub mod events_file;
pub mod installer;
pub mod platform;
pub mod zcode_hook;

use crate::core::history::History;
use crate::core::machine::Machine;
use crate::core::types::*;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

/// 通知开关与两个阈值（notify_enabled / running_fresh_ms / done_after_ms）均已接线：
/// 通知在 send_notification 读取；done_after_ms 在 core 循环每次注入 Tick 前重读 config
/// 注入 Machine（spawn_core），running_fresh_ms 在 zcode 采集线程每次 poll 前重读注入
/// ZcodePoller（spawn_zcode_poller）——设置页保存后实时生效。
const TRAY_ID: &str = "agentboard-tray";

/// 应用启动时刻（epoch ms，run() 里记一次）。启动首扫会把历史数据当事件回放，
/// 由此推导的状态迁移（如历史会话推断完成）一律静默，防止启动通知风暴。
static APP_START_MS: OnceLock<u64> = OnceLock::new();

/// 通知闸（纯函数，便于单测）：last_activity 早于应用启动时刻 → 历史回填，不发通知。
fn notification_allowed(last_activity: u64, app_start_ms: u64) -> bool {
    last_activity >= app_start_ms
}

/// 来源健康三态阈值：超过 10 分钟无数据 → 需要检查。
const SOURCE_STALE_MS: u64 = 10 * 60 * 1000;

/// 来源健康三态（纯函数，便于单测；前端 Settings 同规则换算）：
/// None → "none"（未接入）/ 距今 <10 分钟 → "ok"（正常）/ 否则 → "stale"（需要检查）。
fn health_state(last_event_at: Option<u64>, now: u64) -> &'static str {
    match last_event_at {
        None => "none",
        Some(t) if now.saturating_sub(t) < SOURCE_STALE_MS => "ok",
        Some(_) => "stale",
    }
}

/// 各来源"最后一次有新数据"的时刻（epoch ms）。采集线程每次 poll 有新数据时更新，
/// get_source_status 读取后换算三态。
#[derive(Default)]
struct SourceHealth {
    claude_transcripts_last: Option<u64>,
    claude_events_last: Option<u64>,
    /// ZCode db 轮询与 hooks 事件轮询共同写入（任一来源来数即更新）
    zcode_last: Option<u64>,
}

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
pub fn zcode_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zcode")
        .join("cli")
        .join("config.json")
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
/// ZCode hook 桥的事件落盘（events/zcode.jsonl）
pub fn zcode_hook_append_event(line: &str, dir: &std::path::Path) -> std::io::Result<()> {
    events_file::append_agent_event(line, dir, "zcode.jsonl")
}

pub struct AppState {
    pub machine: Arc<Mutex<Machine>>,
    pub health: Arc<Mutex<SourceHealth>>,
}

#[tauri::command]
fn get_sessions(state: tauri::State<AppState>) -> Vec<SessionSnapshot> {
    state
        .machine
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshots()
}

/// 每源 `{ connected, last_event_at }`：connected 沿用既有判定
/// （转录目录存在 / Claude hooks 已注册 / ZCode db 存在 + hooks 已注册），
/// last_event_at 来自采集线程的 SourceHealth（三态换算由前端按 health_state 规则做）。
#[tauri::command]
fn get_source_status(state: tauri::State<AppState>) -> serde_json::Value {
    let h = state.health.lock().unwrap_or_else(|p| p.into_inner());
    serde_json::json!({
        "claude_transcripts": {
            "connected": claude_projects_dir().exists(),
            "last_event_at": h.claude_transcripts_last,
        },
        "claude_events": {
            "connected": installer::ours_registered(&claude_settings_path()),
            "last_event_at": h.claude_events_last,
        },
        "zcode": {
            "connected": zcode_db_path().exists()
                && installer::ours_registered_zcode(&zcode_config_path()),
            "last_event_at": h.zcode_last,
        },
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

// v0.1.3 交接项（Task 2 Concern #3）：ZCode hooks 安装/卸载的 Tauri 薄封装
// （照 install_hooks_cmd 模式：hook exe 同目录 + zcode config 路径 + backups/state）。
#[tauri::command]
fn install_zcode_hooks_cmd() -> Result<serde_json::Value, String> {
    let hook_exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("无法定位安装目录")?
        .join("agentboard-hook.exe");
    if !hook_exe.exists() {
        return Err("未找到 agentboard-hook.exe，请先完整构建".into());
    }
    let report = installer::install_zcode_hooks(
        &hook_exe,
        &zcode_config_path(),
        &agentboard_dir().join("backups"),
        &agentboard_dir().join("zcode-hooks-state.json"),
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "backupPath": report.backup_path.to_string_lossy(),
        "changed": report.changed,
    }))
}

#[tauri::command]
fn uninstall_zcode_hooks_cmd() -> Result<serde_json::Value, String> {
    let removed = installer::uninstall_zcode_hooks(
        &zcode_config_path(),
        &agentboard_dir().join("zcode-hooks-state.json"),
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "removed": removed }))
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
fn spawn_transcript_watcher(tx: mpsc::Sender<UnifiedEvent>, health: Arc<Mutex<SourceHealth>>) {
    spawn_guarded("claude-transcripts", move || {
        use notify_debouncer_full::{
            DebounceEventResult, new_debouncer, notify::RecursiveMode, notify::Watcher,
        };
        let root = claude_projects_dir();
        let mut poller = collectors::claude_transcripts::TranscriptPoller::new(root.clone());
        // 每次 poll 有新数据 → 更新来源健康时间戳（Task 4）
        let mark = |n: usize| {
            if n > 0 {
                health
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .claude_transcripts_last = Some(now_ms());
            }
        };
        let events = poller.poll();
        mark(events.len());
        send_all(&tx, events);

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
                    let events = poller.poll();
                    mark(events.len());
                    send_all(&tx, events);
                }
                return;
            }
        }
        // 兜底：debouncer/watch 失败 → 4s 轮询
        loop {
            let events = poller.poll();
            mark(events.len());
            send_all(&tx, events);
            std::thread::sleep(Duration::from_secs(4));
        }
    });
}

/// ② hook 事件：2s 间隔轮询 events 文件（文件不存在安全返回空）。
fn spawn_hook_poller(tx: mpsc::Sender<UnifiedEvent>, health: Arc<Mutex<SourceHealth>>) {
    spawn_guarded("claude-hooks", move || {
        let mut poller = collectors::claude_events::HookEventsPoller::new(
            agentboard_dir().join("events").join("claude-code.jsonl"),
            AgentKind::ClaudeCode,
        );
        loop {
            let events = poller.poll();
            if !events.is_empty() {
                health
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .claude_events_last = Some(now_ms());
            }
            send_all(&tx, events);
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

/// ③ ZCode：4s 间隔 poll；open 失败（源离线）退避 30s 重试。
fn spawn_zcode_poller(tx: mpsc::Sender<UnifiedEvent>, health: Arc<Mutex<SourceHealth>>) {
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
                // 运行中判定窗口实时生效：每次 poll 前读一次 config（本地小 JSON，4s 一次可接受）
                let cfg = core::config::load();
                p.set_running_fresh_ms(cfg.running_fresh_ms);
                let events = p.poll(now_ms());
                if !events.is_empty() {
                    health.lock().unwrap_or_else(|p| p.into_inner()).zcode_last = Some(now_ms());
                }
                send_all(&tx, events);
            }
            std::thread::sleep(Duration::from_secs(4));
        }
    });
}

/// ⑤ ZCode hooks 事件（v0.1.3）：2s 间隔轮询 events/zcode.jsonl
/// （agentboard-hook.exe zcode 桥写入；文件不存在安全返回空）。
fn spawn_zcode_hook_poller(tx: mpsc::Sender<UnifiedEvent>, health: Arc<Mutex<SourceHealth>>) {
    spawn_guarded("zcode-hooks", move || {
        let mut poller = collectors::claude_events::HookEventsPoller::new(
            agentboard_dir().join("events").join("zcode.jsonl"),
            AgentKind::ZCode,
        );
        loop {
            let events = poller.poll();
            if !events.is_empty() {
                health.lock().unwrap_or_else(|p| p.into_inner()).zcode_last = Some(now_ms());
            }
            send_all(&tx, events);
            std::thread::sleep(Duration::from_secs(2));
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

/// 横幅主体点击判定。action_id 语义来自插件 desktop.rs：notify-rust 的
/// NotificationResponse::Default（toast 主体点击）→ "tap"（desktop.rs:724）。
fn is_notification_tap(action_id: &str) -> bool {
    action_id == "tap"
}

/// 通知横幅点击处理：toast 主体点击（action_id = "tap"）唤起看板窗口并聚焦，
/// 与托盘左键/菜单"打开看板"同一行为（show_main）。其他 action（如未来加的按钮）不处理。
fn handle_notification_action(app: &AppHandle, action_id: &str) {
    if is_notification_tap(action_id) {
        show_main(app);
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
/// 另受启动回填闸门控：last_activity 早于 APP_START 的迁移不通知（看板显示不受影响）。
fn send_notification(app: &AppHandle, snap: &SessionSnapshot) {
    if !notification_allowed(snap.last_activity, *APP_START_MS.get_or_init(now_ms)) {
        return;
    }
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
                        // 完成推断阈值实时生效：每次注入 Tick 前读一次 config
                        //（本地小 JSON，5s 一次可接受）
                        let cfg = core::config::load();
                        machine
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .set_done_after_ms(cfg.done_after_ms);
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
    let _ = APP_START_MS.set(now_ms()); // 启动时刻记一次：回填闸基准
    let (tx, rx) = mpsc::channel::<UnifiedEvent>();
    // 启动时读一次 config 作初始阈值（load 失败退默认 15 分钟）；此后 core 循环每次
    // 注入 Tick 前重读 config 覆盖，设置页改动实时生效
    let machine = Arc::new(Mutex::new(Machine::new(core::config::load().done_after_ms)));
    let health = Arc::new(Mutex::new(SourceHealth::default()));
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
            health: health.clone(),
        })
        .setup(move |app| {
            // 通知横幅点击：toast 主体点击（tap）→ 唤起看板窗口。
            // 插件 desktop.rs：handler 注册进全局 shared，须尽早注册；
            // "Notifications shown before the first handler existed report nothing"。
            let notif_app = app.handle().clone();
            app.notification()
                .on_action(move |action| {
                    handle_notification_action(&notif_app, action.action_id());
                })
                .expect("register notification click handler");

            // ⑤ 托盘：默认图标 + 初始菜单（等待 0 · 运行 0 / 打开看板 / 退出）
            let mut tray = tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
                .tooltip("agentboard")
                // tray-icon 默认左键弹菜单（抢焦点）——关掉：左键=纯唤回窗口，右键=菜单
                .show_menu_on_left_click(false)
                .menu(&build_tray_menu(app.handle(), 0, 0)?)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                // 托盘左键单击（松开）= 唤回主窗口，与"打开看板"菜单同一行为（show_main）
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            // 关窗 = 隐藏到托盘常驻（托盘"退出"菜单才是真退出）
            if let Some(w) = app.get_webview_window("main") {
                let handle = w.clone();
                w.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = handle.hide();
                    }
                });
            }

            // ①②③ + ④ + ⑤：采集线程 + 核心消费线程
            spawn_transcript_watcher(tx.clone(), health.clone());
            spawn_hook_poller(tx.clone(), health.clone());
            spawn_zcode_poller(tx.clone(), health.clone());
            spawn_zcode_hook_poller(tx.clone(), health.clone());
            spawn_core(app.handle().clone(), rx, machine.clone(), history.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_sessions,
            get_source_status,
            jump,
            get_config,
            set_config,
            install_hooks_cmd,
            install_zcode_hooks_cmd,
            uninstall_zcode_hooks_cmd
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::claude_transcripts::TranscriptPoller;

    const DONE_AFTER: u64 = 15 * 60 * 1000;

    #[test]
    fn gate_blocks_only_backfill() {
        let start = 1_000_000u64;
        assert!(
            !notification_allowed(start - 1, start),
            "早于启动时刻 → 静默"
        );
        assert!(
            notification_allowed(start, start),
            "启动后的活动 → 允许通知"
        );
    }

    /// 验收：历史转录（mtime 早于 15 分钟阈值）首扫 → 首个 Tick 推断 Done，
    /// machine 层会请求通知，但经 APP_START 闸（last_activity < 启动时刻）后不通知。
    #[test]
    fn historical_transcript_first_scan_infers_done_silently() {
        let base = std::env::temp_dir().join(format!("ab-lib-{}-hist", std::process::id()));
        let proj = base.join("C--Apps-repo-x");
        std::fs::create_dir_all(&proj).unwrap();
        let now = 10_000_000_000_000u64;
        let old_mtime = now - DONE_AFTER - 60_000; // 早于 15 分钟阈值的历史写入
        std::fs::write(
            proj.join("s-old.jsonl"),
            r#"{"type":"user","sessionId":"s-old","cwd":"C:\\Apps\\repo\\x","message":{"content":"历史任务"}}"#,
        )
        .unwrap();

        // 首扫（模拟启动回填）：事件 ts = 文件 mtime（旧）
        let mut poller = TranscriptPoller::new(base.clone());
        let events = poller.poll_with_ts(|_| old_mtime);
        let _ = std::fs::remove_dir_all(&base);

        let mut m = Machine::new(DONE_AFTER);
        let mut ups = Vec::new();
        for e in &events {
            ups.extend(m.process(e));
        }
        assert!(
            ups.iter().all(|u| u.snapshot.last_activity == old_mtime),
            "首扫事件的时间戳来自 mtime 而非轮询时刻"
        );

        // 首个 Tick 即推断 Done（不再显示 15 分钟错误的"运行中"）
        let ups = m.process(&UnifiedEvent::Tick { now });
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
        assert!(ups[0].snapshot.done_inferred);
        assert!(ups[0].should_notify, "machine 层不知道启动时刻，会请求通知");

        // 但经 APP_START 闸后不发通知（历史回填一律静默）
        let app_start = now;
        assert!(!notification_allowed(
            ups[0].snapshot.last_activity,
            app_start
        ));
    }

    /// 验收修复回归（ZCode db 启动通知风暴）：首扫把无 presence 的历史会话判 Closed，
    /// 事件 ts = 会话 time_updated（旧）→ machine 产出 should_notify=true 的 Done，
    /// 但 last_activity 停在历史时间 → APP_START 闸拦截，重启不再弹「ZCode 任务完成」。
    #[test]
    fn zcode_historical_closed_first_scan_is_silent() {
        let now = 10_000_000_000_000u64;
        let old_updated = now - 3_600_000; // 历史会话最后更新于 1 小时前
        let con = rusqlite::Connection::open_in_memory().unwrap();
        con.execute_batch(
            r#"CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, directory TEXT,
                time_created INTEGER, time_updated INTEGER, last_outcome TEXT,
                time_deleted INTEGER, time_archived INTEGER);
            CREATE TABLE session_runtime_presence (session_id TEXT, owner_id TEXT, process_id INTEGER);
            CREATE TABLE turn_usage (id INTEGER PRIMARY KEY, session_id TEXT, status TEXT,
                error_type TEXT, started_at INTEGER, completed_at INTEGER);"#,
        )
        .unwrap();
        con.execute(
            "INSERT INTO session (id,title,directory,time_created,time_updated,last_outcome) VALUES ('z1','t',?1,?2,?2,NULL)",
            rusqlite::params![r"C:\repo\z", old_updated],
        )
        .unwrap();

        // 启动首扫：历史会话 → Discovered + Closed（ts 均为 time_updated）
        let mut p = crate::collectors::zcode_db::ZcodePoller::with_connection(con).unwrap();
        let events = p.poll(now);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, UnifiedEvent::Closed { ts, .. } if *ts == old_updated as u64)),
            "首扫 Closed 带会话历史时间"
        );

        let mut m = Machine::new(DONE_AFTER);
        let mut ups = Vec::new();
        for e in &events {
            ups.extend(m.process(e));
        }
        let done = ups
            .iter()
            .find(|u| u.snapshot.status == SessionStatus::Done)
            .expect("Closed 应把历史会话判为 Done");
        assert!(
            ups.iter().any(|u| u.should_notify),
            "machine 不知道启动时刻，会请求通知"
        );
        assert_eq!(
            done.snapshot.last_activity, old_updated as u64,
            "last_activity 停在历史时间，不被轮询时刻顶起"
        );
        assert!(
            !notification_allowed(done.snapshot.last_activity, now),
            "APP_START 闸拦截历史回填，不发通知"
        );
    }

    /// 验收修复回归（首扫回填语义）：首扫把 presence 存活 + turn 全完成的会话判
    /// 等待·空闲，Waiting 事件 ts = 会话 time_updated（历史时间）→ machine 请求通知
    /// 但被 APP_START 闸拦截；首扫结束后 turn 变为进行中，Activity ts=now（实时）
    /// → 通知放行。既不风暴也不漏报。
    #[test]
    fn zcode_first_scan_waiting_historical_ts_is_silent_then_realtime_notifies() {
        let now = 10_000_000_000_000u64;
        let old_updated = now - 1_800_000; // 会话 30 分钟前更新
        // 两轮 poll 之间要改库，用临时文件库（poller 只读打开，改动走另一连接）
        let dir = std::env::temp_dir().join(format!("ab-lib-zc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("scan.sqlite");
        let con = rusqlite::Connection::open(&db).unwrap();
        con.execute_batch(
            r#"CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, directory TEXT,
                time_created INTEGER, time_updated INTEGER, last_outcome TEXT,
                time_deleted INTEGER, time_archived INTEGER);
            CREATE TABLE session_runtime_presence (session_id TEXT, owner_id TEXT, process_id INTEGER);
            CREATE TABLE turn_usage (id INTEGER PRIMARY KEY, session_id TEXT, status TEXT,
                error_type TEXT, started_at INTEGER, completed_at INTEGER);"#,
        )
        .unwrap();
        con.execute(
            "INSERT INTO session (id,title,directory,time_created,time_updated) VALUES ('z1','t',?1,?2,?2)",
            rusqlite::params![r"C:\repo\z", old_updated],
        )
        .unwrap();
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        drop(con);

        // 首扫：presence 存活 + turn 全完成 → Waiting（ts = time_updated 历史时间）
        let mut p = crate::collectors::zcode_db::ZcodePoller::open(&db).unwrap();
        let events = p.poll(now);
        assert!(
            events.iter().any(|e| matches!(
                e,
                UnifiedEvent::Waiting { ts, .. } if *ts == old_updated as u64
            )),
            "首扫 Waiting 带会话历史时间而非轮询时刻"
        );

        let mut m = Machine::new(DONE_AFTER);
        let mut ups = Vec::new();
        for e in &events {
            ups.extend(m.process(e));
        }
        let waiting = ups
            .iter()
            .find(|u| u.snapshot.status == SessionStatus::Waiting(WaitingKind::Idle))
            .expect("首扫活会话应判等待·空闲而非 Done");
        assert!(
            ups.iter().any(|u| u.should_notify),
            "machine 不知道启动时刻，会请求通知"
        );
        assert_eq!(waiting.snapshot.last_activity, old_updated as u64);
        assert!(
            !notification_allowed(waiting.snapshot.last_activity, now),
            "APP_START 闸拦截首扫 Waiting，不发通知"
        );

        // 首扫结束后（first_scan=false）turn 变为进行中 → Activity ts=now，实时信号放行
        let con2 = rusqlite::Connection::open(&db).unwrap();
        con2.execute(
            "INSERT INTO turn_usage (session_id,status,started_at,completed_at) VALUES ('z1','completed',?1,NULL)",
            [now as i64],
        )
        .unwrap();
        drop(con2);
        let events2 = p.poll(now + 4_000);
        assert!(events2.iter().any(|e| matches!(
            e,
            UnifiedEvent::Activity { ts, .. } if *ts == now + 4_000
        )));
        let mut ups2 = Vec::new();
        for e in &events2 {
            ups2.extend(m.process(e));
        }
        let running = ups2
            .iter()
            .find(|u| u.snapshot.status == SessionStatus::Running)
            .expect("实时 Activity 应回到运行中");
        assert_eq!(running.snapshot.last_activity, now + 4_000);
        assert!(
            notification_allowed(running.snapshot.last_activity, now),
            "实时活动（晚于启动时刻）放行通知"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 横幅点击分派契约：用插件自身类型定义反序列化，验证插件 desktop 上报的
    /// "横幅主体点击"（notify-rust Default → "tap"）恰好命中 is_notification_tap 的判定；
    /// 其他 action（dismiss/未知）不触发唤起。（tauri mock runtime 在本机测试进程
    /// 启动失败 0xc0000139，窗口级断言以构建后人工点验为准。）
    #[test]
    fn notification_banner_click_maps_to_tap_dispatch() {
        let click: tauri_plugin_notification::ActionPerformed =
            serde_json::from_str(r#"{"actionId":"tap"}"#).unwrap();
        assert!(
            is_notification_tap(click.action_id()),
            "横幅主体点击 → 唤起"
        );

        let dismiss: tauri_plugin_notification::ActionPerformed =
            serde_json::from_str(r#"{"actionId":"dismiss"}"#).unwrap();
        assert!(!is_notification_tap(dismiss.action_id()));
        assert!(!is_notification_tap(""), "未知 action 不唤起");
    }

    // v0.1.3 Task 4：来源健康三态纯函数（None→未接入 / 10 分钟内→正常 / 超时→需要检查）
    #[test]
    fn health_state_three_states() {
        let now = 10_000_000_000_000u64;
        assert_eq!(health_state(None, now), "none", "从无数据 → 未接入");
        assert_eq!(health_state(Some(now), now), "ok");
        assert_eq!(
            health_state(Some(now - 599_999), now),
            "ok",
            "阈值内 → 正常"
        );
        assert_eq!(
            health_state(Some(now - 600_000), now),
            "stale",
            "恰好 10 分钟 → 需要检查"
        );
        assert_eq!(health_state(Some(now - 3_600_000), now), "stale");
    }
}
