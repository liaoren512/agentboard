//! agentboard-hook.exe：Claude Code Notification hook。
//! 铁律：任何情况不 panic、不阻塞、永远 exit 0（Notification 事件忽略 exit code/stderr）。
fn main() {
    let _ = run();
    std::process::exit(0);
}

fn run() -> std::io::Result<()> {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let ts = agentboard::events_file_now_ms();
    let Some(line) = agentboard::events_file_handle_input(&input, ts) else {
        return Ok(());
    };
    let dir = agentboard::agentboard_dir().join("events");
    agentboard::events_file_append_event(&line, &dir)
}
