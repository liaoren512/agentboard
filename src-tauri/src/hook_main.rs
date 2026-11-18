//! agentboard-hook.exe：coding agent hooks 桥。
//! 用法：`agentboard-hook.exe zcode <event>`（ZCode 桥）或无参（Claude Notification）。
//! 铁律：任何情况不 panic、不阻塞、无 stdout、毫秒级返回、永远 exit 0
//!（Notification 事件忽略 exit code/stderr；ZCode PermissionRequest 原生弹窗并行等待）。
fn main() {
    let _ = run();
    std::process::exit(0);
}

fn run() -> std::io::Result<()> {
    use std::io::Read;
    let args: Vec<String> = std::env::args().collect();
    // `zcode <event>` 子命令：走 ZCode 桥（进程链 + db 校验后落 events/zcode.jsonl）
    if args.get(1).map(String::as_str) == Some("zcode") {
        return run_zcode(&args);
    }
    // 无参：既有 Claude Notification 路径
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let ts = agentboard::events_file_now_ms();
    let Some(line) = agentboard::events_file_handle_input(&input, ts) else {
        return Ok(());
    };
    let dir = agentboard::agentboard_dir().join("events");
    agentboard::events_file_append_event(&line, &dir)
}

fn run_zcode(args: &[String]) -> std::io::Result<()> {
    use std::io::Read;
    let Some(event) = args.get(2) else {
        return Ok(()); // 缺事件名：吞掉不做事
    };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let Some(line) = agentboard::zcode_hook::run_zcode_hook_real(
        event,
        &input,
        agentboard::events_file_now_ms(),
    ) else {
        return Ok(());
    };
    let dir = agentboard::agentboard_dir().join("events");
    agentboard::zcode_hook_append_event(&line, &dir)
}
