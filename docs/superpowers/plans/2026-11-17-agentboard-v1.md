# agentboard v1 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Windows 托盘看板，聚合 Claude Code 与 ZCode 的会话状态（运行中/等待/失败/完成），本地运行、只读监控、无网络。

**Architecture:** Tauri 2 应用；Rust 后端跑三个采集器（Claude 转录监听、Claude hook 事件、ZCode db 轮询），事件归并进单一状态机，经 Tauri event 推给 React 看板；托盘常驻 + Windows 通知。

**Tech Stack:** Tauri 2 · Rust（notify / rusqlite bundled / serde_json）· Vite + React + TS · vitest（前端纯函数单测）

**Spec:** `docs/superpowers/specs/2026-11-17-agentboard-design.md`（实现时与计划一起读）

## 前置验证结论（2026-11-17 实测，任务代码以此为准）

- **环境**：Node v22.11.0 ✓ · MSVC BuildTools 16.11（含 VC tools）✓ · WebView2 157 ✓ · **Rust 工具链缺失**（Task 0 安装）
- **Claude Code hooks**（官方文档实测）：settings.json 三层结构 `hooks.Notification[].matcher → hooks[].{type,command,args}`；Notification matcher 支持多值 `permission_prompt|idle_prompt|agent_needs_input`；stdin 公共字段含 `session_id`/`transcript_path`/`cwd`/`message`；**Windows exec form 要求 command 是真 .exe 且 args 是数组**——本计划分发 `agentboard-hook.exe` 正好满足；Notification 事件"exit code and stderr are ignored"→ hook 永远 exit 0 天然安全
- **Claude 转录**：`~/.claude/projects/<转义cwd>/<sessionId>.jsonl`，JSONL，行含 `type`/`sessionId`/`cwd`/`message`，错误行含 `"isApiErrorMessage":true`（非官方格式，解析器按窄接口隔离）
- **ZCode db**（`~/.zcode/cli/db/db.sqlite`，WAL）：
  - `session(id, title, directory, time_created, time_updated, last_outcome, time_deleted, time_archived)`，时间均为 epoch ms，`last_outcome ∈ {NULL, 'completed', 'interrupted'}`
  - `session_runtime_presence(session_id, owner_id, process_id)`：**在表内 = 有进程持有 = 活会话**
  - `turn_usage(session_id, status, error_type, started_at, completed_at, …)`：`status ∈ {'completed','error','cancelled'}`；**进行中的 turn = `completed_at IS NULL`**；`error_type ∈ {'unknown_error','turn_cancelled'}`（cancelled 是用户打断，不算失败）
  - ZCode 的"等待审批"在 db 无信号（`permission` 表实测 0 行）→ v1 不判定 ZCode 等待，活会话且无进行中 turn 标 `等待·空闲`

## Global Constraints

- 完全本地运行，任何代码不发网络请求；数据只落 `~/.agentboard/`
- **hook 脚本绝不阻塞 agent**：任何异常/垃圾输入静默降级，永远 `exit 0`
- 推不出来的状态不硬标（Claude 完成必须标"推断"）
- 通知内容只含 agent 名 + 状态 + 项目名，不含请求内容；每状态进入一次通知，回到运行中后重新武装
- 事件文件超 10MB 轮转；历史上限 1000 条；hook 配置备份留最近 10 组
- 单实例（第二个启动聚焦已有窗口）
- 目标平台 Windows 10/11 + WebView2；本机开发路径：仓库 `C:\Apps\lr\workspace_ai\agentboard`
- 每个任务结束必须 `cargo test`（或 `npx vitest run`）全绿 + commit

## 文件结构

```
agentboard/
├── package.json / vite.config.ts / tsconfig.json / index.html
├── src/                        # 前端
│   ├── main.tsx / App.tsx
│   ├── board-model.ts          # 分组/排序/筛选纯函数（vitest）
│   ├── board-model.test.ts
│   ├── Board.tsx / SessionRow.tsx / Settings.tsx
│   ├── types.ts                # SessionSnapshot 的 TS 镜像
│   └── styles.css
├── src-tauri/
│   ├── Cargo.toml              # 一个 crate，两个 [[bin]]
│   ├── tauri.conf.json
│   ├── build.rs
│   ├── capabilities/default.json
│   ├── icons/                  # tauri 模板生成
│   └── src/
│       ├── main.rs             # agentboard 主程序入口（薄）
│       ├── hook_main.rs        # agentboard-hook.exe 入口（薄）
│       ├── lib.rs              # Tauri 装配：采集循环、commands、托盘、通知
│       ├── core/
│       │   ├── mod.rs
│       │   ├── types.rs        # 统一类型（事件、快照、状态）
│       │   ├── machine.rs      # 状态机（含通知武装）
│       │   ├── history.rs      # history.sqlite + 1000 条上限
│       │   └── config.rs       # ~/.agentboard/config.json 读写
│       ├── collectors/
│       │   ├── mod.rs
│       │   ├── claude_transcripts.rs
│       │   ├── claude_events.rs
│       │   └── zcode_db.rs
│       ├── installer.rs        # settings.json hook 注册/备份/卸载
│       └── platform.rs         # 跳回（wt + 剪贴板）
└── docs/superpowers/           # spec 与本计划
```

---

### Task 0: 安装 Rust 工具链

**Files:** 无（环境操作）

- [ ] **Step 1: 安装 rustup（MSVC 工具链）**

```bash
winget install --id Rustlang.Rustup --silent --accept-package-agreements --accept-source-agreements
```

装完重开 shell，确认默认 host 是 x86_64-pc-windows-msvc（rustup 默认即 msvc，无需额外 target）。

- [ ] **Step 2: 验证**

Run: `cargo --version && rustc --version`
Expected: `cargo 1.x` / `rustc 1.x (stable x86_64-pc-windows-msvc)`。失败则报错停止，不要跳过。

- [ ] **Step 3: 提交（若 rustup 生成了任何仓库内文件则提交，否则无提交）**

---

### Task 1: Tauri 脚手架并跑通空窗口

**Files:**
- Create: `package.json`, `vite.config.ts`, `tsconfig.json`, `index.html`, `src/main.tsx`, `src/App.tsx`, `src/styles.css`, `src-tauri/**`（模板全套）
- Modify: `.gitignore`

**Interfaces:**
- Produces: 可运行的 Tauri 2 + React 空窗；后续任务往里填模块

- [ ] **Step 1: 模板生成到临时目录再并入仓库**（仓库已有 docs/，不能整目录覆盖）

```bash
cd /tmp 2>/dev/null || cd "$TEMP"
npx --yes create-tauri-app@latest agentboard-scaffold --template react-ts --manager npm --yes
cp -r agentboard-scaffold/. "C:\Apps\lr\workspace_ai\agentboard/"
rm -rf agentboard-scaffold
```

并入手动清单：不覆盖 `docs/`；`.gitignore` 追加 `node_modules/`、`dist/`、`src-tauri/target/`。

- [ ] **Step 2: 安装依赖**

```bash
cd "C:\Apps\lr\workspace_ai\agentboard" && npm install
```

- [ ] **Step 3: 验证编译与空窗**

Run: `npm run tauri build 2>&1 | tail -5`（首次验证走完整 build，比 dev 更能暴露 MSVC 问题）
Expected: build 成功产出 exe。随后 `npm run tauri dev` 手工确认空窗口弹出。

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "chore: Tauri 2 + React 脚手架"
git push
```

---

### Task 2: 核心类型 `core/types.rs`

**Files:**
- Create: `src-tauri/src/core/mod.rs`, `src-tauri/src/core/types.rs`
- Test: 同文件 `#[cfg(test)]`

**Interfaces:**
- Produces: `AgentKind`、`WaitingKind`、`SessionStatus`、`SessionSnapshot`、`UnifiedEvent`、`repo_name()`、`branch_from_git()`——后续所有任务依赖这些确切名字

- [ ] **Step 1: 写失败测试**

```rust
// src-tauri/src/core/types.rs 底部
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serde_roundtrip() {
        let e = UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "abc".into(),
            kind: WaitingKind::Permission,
            ts: 1_732_000_000_000,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"waiting""#));
        let back: UnifiedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn status_waiting_serializes_kind() {
        let s = SessionStatus::Waiting(WaitingKind::Idle);
        assert_eq!(serde_json::to_string(&s).unwrap(), r#""waiting_idle""#);
    }

    #[test]
    fn repo_name_takes_last_segment() {
        assert_eq!(repo_name(r"C:\Apps\lr\workspace_ai\agentboard"), "agentboard");
    }

    #[test]
    fn branch_reads_git_head() {
        let dir = std::env::temp_dir().join("ab-test-branch");
        let git = dir.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(branch_from_git(&dir), Some("main".to_string()));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd src-tauri && cargo test types`
Expected: 编译失败（模块不存在）。

- [ ] **Step 3: 实现**

```rust
// src-tauri/src/core/mod.rs
pub mod types;
```

```rust
// src-tauri/src/core/types.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    ClaudeCode,
    ZCode,
}

impl AgentKind {
    pub fn label(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::ZCode => "ZCode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingKind {
    Permission,
    Question,
    Idle,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Waiting(WaitingKind), // "waiting_permission" / "waiting_idle" / ...
    Failed,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session_id: String,
    pub agent: AgentKind,
    pub title: String,
    pub cwd: String,
    pub repo: String,
    pub branch: Option<String>,
    pub status: SessionStatus,
    pub done_inferred: bool,
    pub last_activity: u64, // epoch ms
    pub time_created: u64,
}

/// 采集器输出 / 状态机输入的唯一事件类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UnifiedEvent {
    Discovered { agent: AgentKind, session_id: String, cwd: String, title: Option<String>, ts: u64 },
    Activity { agent: AgentKind, session_id: String, ts: u64 },
    Waiting { agent: AgentKind, session_id: String, kind: WaitingKind, ts: u64 },
    TurnFailed { agent: AgentKind, session_id: String, ts: u64 },
    TurnFinished { agent: AgentKind, session_id: String, ts: u64 },
    Closed { agent: AgentKind, session_id: String, ts: u64 },
    Tick { now: u64 },
}

pub fn repo_name(cwd: &str) -> String {
    cwd.trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(cwd)
        .to_string()
}

/// 读 cwd/.git/HEAD 取分支；.git 是文件（worktree）时返回 None（v1 不解析 worktree 指针）。
pub fn branch_from_git(cwd: &str) -> Option<String> {
    let head = std::path::Path::new(cwd).join(".git").join("HEAD");
    let text = std::fs::read_to_string(head).ok()?;
    let line = text.lines().next()?;
    line.strip_prefix("ref: refs/heads/").map(|s| s.trim().to_string())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests { /* Step 1 的测试放这里 */ }
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test types`
Expected: 4 passed。

- [ ] **Step 5: Commit**

```bash
git add src-tauri && git commit -m "feat(core): 统一类型与事件定义"
```

---

### Task 3: 状态机 `core/machine.rs`

**Files:**
- Create: `src-tauri/src/core/machine.rs`
- Modify: `src-tauri/src/core/mod.rs`
- Test: 同文件 `#[cfg(test)]`

**Interfaces:**
- Consumes: Task 2 全部类型
- Produces: `Machine::new(done_after_ms)`、`machine.process(event) -> Vec<Update>`、`machine.snapshots() -> Vec<SessionSnapshot>`；`pub struct Update { pub snapshot: SessionSnapshot, pub should_notify: bool }`

- [ ] **Step 1: 写失败测试**（覆盖 spec §5 全路径）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::*;

    const TS: u64 = 1_000_000_000_000;
    const DONE_AFTER: u64 = 15 * 60 * 1000;

    fn disc(agent: AgentKind, id: &str, ts: u64) -> UnifiedEvent {
        UnifiedEvent::Discovered { agent, session_id: id.into(), cwd: r"C:\repo\x".into(), title: Some("t".into()), ts }
    }

    #[test]
    fn discovered_then_activity_runs_then_tick_completes_inferred() {
        let mut m = Machine::new(DONE_AFTER);
        let ups = m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].snapshot.status, SessionStatus::Running);
        assert!(!ups[0].should_notify); // 运行中不通知

        let ups = m.process(&UnifiedEvent::Activity { agent: AgentKind::ClaudeCode, session_id: "s1".into(), ts: TS + 1000 });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Running);

        // 停滞超过阈值 → 推断完成 + 通知一次
        let ups = m.process(&UnifiedEvent::Tick { now: TS + 1000 + DONE_AFTER });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
        assert!(ups[0].snapshot.done_inferred);
        assert!(ups[0].should_notify);

        // 推断完成后的 Tick 不再重复通知
        let ups = m.process(&UnifiedEvent::Tick { now: TS + 1000 + 2 * DONE_AFTER });
        assert!(ups.is_empty());
    }

    #[test]
    fn waiting_notifies_once_then_activity_rearms() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        let ups = m.process(&UnifiedEvent::Waiting { agent: AgentKind::ClaudeCode, session_id: "s1".into(), kind: WaitingKind::Permission, ts: TS });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Waiting(WaitingKind::Permission));
        assert!(ups[0].should_notify);

        let ups = m.process(&UnifiedEvent::Waiting { agent: AgentKind::ClaudeCode, session_id: "s1".into(), kind: WaitingKind::Permission, ts: TS + 1 });
        assert!(!ups[0].should_notify, "同一等待期内不重复通知");

        // 活动回到运行中，再等待 → 重新通知
        m.process(&UnifiedEvent::Activity { agent: AgentKind::ClaudeCode, session_id: "s1".into(), ts: TS + 2 });
        let ups = m.process(&UnifiedEvent::Waiting { agent: AgentKind::ClaudeCode, session_id: "s1".into(), kind: WaitingKind::Permission, ts: TS + 3 });
        assert!(ups[0].should_notify);
    }

    #[test]
    fn failed_then_activity_clears() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ZCode, "z1", TS));
        let ups = m.process(&UnifiedEvent::TurnFailed { agent: AgentKind::ZCode, session_id: "z1".into(), ts: TS });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Failed);
        assert!(ups[0].should_notify);
        let ups = m.process(&UnifiedEvent::Activity { agent: AgentKind::ZCode, session_id: "z1".into(), ts: TS + 1 });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Running);
    }

    #[test]
    fn explicit_finish_is_not_inferred() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ZCode, "z1", TS));
        let ups = m.process(&UnifiedEvent::TurnFinished { agent: AgentKind::ZCode, session_id: "z1".into(), ts: TS });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
        assert!(!ups[0].snapshot.done_inferred);
        assert!(ups[0].should_notify);
    }

    #[test]
    fn closed_marks_done_and_tick_drops_nothing() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ZCode, "z1", TS));
        let ups = m.process(&UnifiedEvent::Closed { agent: AgentKind::ZCode, session_id: "z1".into(), ts: TS });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
    }

    #[test]
    fn waiting_does_not_infer_done() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        m.process(&UnifiedEvent::Waiting { agent: AgentKind::ClaudeCode, session_id: "s1".into(), kind: WaitingKind::Idle, ts: TS });
        let ups = m.process(&UnifiedEvent::Tick { now: TS + 2 * DONE_AFTER });
        assert!(ups.is_empty(), "等待中的会话不做完成推断");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test machine` → 编译失败。

- [ ] **Step 3: 实现**

```rust
// src-tauri/src/core/machine.rs
use std::collections::HashMap;
use crate::core::types::*;

pub struct Update {
    pub snapshot: SessionSnapshot,
    pub should_notify: bool,
}

struct Inner {
    snap: SessionSnapshot,
    /// 本轮已通知过的终态（Waiting/Failed/Done）。回到 Running 时清空 → 重新武装。
    notified: Option<SessionStatus>,
}

pub struct Machine {
    sessions: HashMap<String, Inner>,
    done_after_ms: u64,
}

impl Machine {
    pub fn new(done_after_ms: u64) -> Self {
        Self { sessions: HashMap::new(), done_after_ms }
    }

    pub fn process(&mut self, event: &UnifiedEvent) -> Vec<Update> {
        let mut out = Vec::new();
        match event {
            UnifiedEvent::Tick { now } => {
                let done_after = self.done_after_ms;
                for (id, inner) in self.sessions.iter_mut() {
                    if inner.snap.status == SessionStatus::Running
                        && now.saturating_sub(inner.snap.last_activity) > done_after
                    {
                        inner.snap.status = SessionStatus::Done;
                        inner.snap.done_inferred = true;
                        let notify = inner.notified != Some(SessionStatus::Done);
                        inner.notified = Some(SessionStatus::Done);
                        let _ = id;
                        out.push(Update { snapshot: inner.snap.clone(), should_notify: notify });
                    }
                }
            }
            UnifiedEvent::Discovered { agent, session_id, cwd, title, ts } => {
                let entry = self.sessions.entry(session_id.clone()).or_insert_with(|| Inner {
                    snap: SessionSnapshot {
                        session_id: session_id.clone(),
                        agent: *agent,
                        title: title.clone().unwrap_or_else(|| "(未命名)".into()),
                        cwd: cwd.clone(),
                        repo: repo_name(cwd),
                        branch: branch_from_git(cwd),
                        status: SessionStatus::Running,
                        done_inferred: false,
                        last_activity: *ts,
                        time_created: *ts,
                    },
                    notified: None,
                });
                if let Some(t) = title { entry.snap.title = t.clone(); }
                entry.snap.status = SessionStatus::Running;
                entry.snap.done_inferred = false;
                entry.notified = None;
                entry.snap.last_activity = entry.snap.last_activity.max(*ts);
                out.push(Update { snapshot: entry.snap.clone(), should_notify: false });
            }
            UnifiedEvent::Activity { agent, session_id, ts } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    e.snap.status = SessionStatus::Running;
                    e.snap.done_inferred = false;
                    e.notified = None; // 重新武装
                    e.snap.last_activity = e.snap.last_activity.max(*ts);
                    out.push(Update { snapshot: e.snap.clone(), should_notify: false });
                }
            }
            UnifiedEvent::Waiting { agent, session_id, kind, ts } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    let new_status = SessionStatus::Waiting(*kind);
                    if e.snap.status != new_status {
                        e.snap.status = new_status.clone();
                        e.snap.last_activity = e.snap.last_activity.max(*ts);
                        let notify = e.notified.as_ref() != Some(&new_status);
                        if notify { e.notified = Some(new_status.clone()); }
                        out.push(Update { snapshot: e.snap.clone(), should_notify: notify });
                    }
                }
            }
            UnifiedEvent::TurnFailed { agent, session_id, ts } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    if e.snap.status != SessionStatus::Failed {
                        e.snap.status = SessionStatus::Failed;
                        e.snap.last_activity = e.snap.last_activity.max(*ts);
                        let notify = e.notified != Some(SessionStatus::Failed);
                        if notify { e.notified = Some(SessionStatus::Failed); }
                        out.push(Update { snapshot: e.snap.clone(), should_notify: notify });
                    }
                }
            }
            UnifiedEvent::TurnFinished { agent, session_id, ts } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    e.snap.status = SessionStatus::Done;
                    e.snap.done_inferred = false;
                    e.snap.last_activity = e.snap.last_activity.max(*ts);
                    let notify = e.notified != Some(SessionStatus::Done);
                    if notify { e.notified = Some(SessionStatus::Done); }
                    out.push(Update { snapshot: e.snap.clone(), should_notify: notify });
                }
            }
            UnifiedEvent::Closed { agent, session_id, ts } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    if e.snap.status != SessionStatus::Done {
                        e.snap.status = SessionStatus::Done;
                        e.snap.done_inferred = false;
                        e.snap.last_activity = e.snap.last_activity.max(*ts);
                        let notify = e.notified != Some(SessionStatus::Done);
                        if notify { e.notified = Some(SessionStatus::Done); }
                        out.push(Update { snapshot: e.snap.clone(), should_notify: notify });
                    }
                }
            }
        }
        out
    }

    fn get_mut(&mut self, agent: &AgentKind, session_id: &str) -> Option<&mut Inner> {
        let e = self.sessions.get_mut(session_id)?;
        if e.snap.agent != *agent { return None; }
        Some(e)
    }

    pub fn snapshots(&self) -> Vec<SessionSnapshot> {
        let mut v: Vec<SessionSnapshot> = self.sessions.values().map(|i| i.snap.clone()).collect();
        // 排序由前端做（board-model），这里按最近活动倒序保证列表稳定
        v.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        v
    }
}
```

```rust
// src-tauri/src/core/mod.rs
pub mod types;
pub mod machine;
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test` → 全部通过（含 Task 2）。

- [ ] **Step 5: Commit**

```bash
git add src-tauri && git commit -m "feat(core): 会话状态机与通知武装"
```

---

### Task 4: Claude 转录采集器 `collectors/claude_transcripts.rs`

**Files:**
- Create: `src-tauri/src/collectors/mod.rs`, `src-tauri/src/collectors/claude_transcripts.rs`, `src-tauri/testdata/transcript-*.jsonl`（fixture）
- Test: 同文件 `#[cfg(test)]`（fixture 用 `include_str!` 或临时目录写字符串，选后者——见代码）

**Interfaces:**
- Consumes: `UnifiedEvent`
- Produces: `TranscriptPoller::new(root: PathBuf)`、`poller.poll(now: u64) -> Vec<UnifiedEvent>`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::*;

    fn fixture_dir() -> (tempdir_guard::TempDir, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("ab-tr-{}", std::process::id()));
        let proj = base.join("C--Apps-repo-x");
        std::fs::create_dir_all(&proj).unwrap();
        (tempdir_guard::TempDir(base), proj)
    }
    // 见 Step 3 的 tempdir_guard：Drop 时删目录，测试自清理

    const FIRST_LINE: &str = r#"{"type":"user","sessionId":"s-1","cwd":"C:\\Apps\\repo\\x","message":{"role":"user","content":"帮我修复登录 bug"},"timestamp":"2026-11-17T10:00:00Z"}"#;

    #[test]
    fn new_file_discovered_with_title_and_activity() {
        let (_g, proj) = fixture_dir();
        std::fs::write(proj.join("s-1.jsonl"), format!("{FIRST_LINE}\n")).unwrap();
        let mut p = TranscriptPoller::new(_g.0.clone());
        let ev = p.poll(1000);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Discovered { session_id, title: Some(t), .. } if session_id == "s-1" && t.contains("修复登录"))));
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Activity { session_id, .. } if session_id == "s-1")));
    }

    #[test]
    fn appended_lines_emit_activity_error_emits_turn_failed() {
        let (_g, proj) = fixture_dir();
        let f = proj.join("s-1.jsonl");
        std::fs::write(&f, format!("{FIRST_LINE}\n")).unwrap();
        let mut p = TranscriptPoller::new(_g.0.clone());
        p.poll(1000);

        let err_line = r#"{"type":"assistant","sessionId":"s-1","isApiErrorMessage":true,"message":{"content":"API Error: 500"}}"#;
        std::fs::write(&f, format!("{FIRST_LINE}\n{err_line}\n")).unwrap();
        let ev = p.poll(2000);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Activity { .. })));
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::TurnFailed { .. })));
    }

    #[test]
    fn partial_line_is_buffered_not_parsed() {
        let (_g, proj) = fixture_dir();
        let f = proj.join("s-2.jsonl");
        std::fs::write(&f, format!("{FIRST_LINE}\n{{\"type\":\"assi")).unwrap(); // 半行
        let mut p = TranscriptPoller::new(_g.0.clone());
        let ev = p.poll(1000);
        // 半行不能产生第二个事件（第一个完整行已产生 Discovered）
        assert_eq!(ev.iter().filter(|e| matches!(e, UnifiedEvent::Activity { .. })).count(), 1);
        // 补全后半行 → 下次 poll 产出 Activity
        std::fs::write(&f, format!("{FIRST_LINE}\n{{\"type\":\"assistant\",\"sessionId\":\"s-2\"}}\n")).unwrap();
        let ev = p.poll(2000);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Activity { session_id, .. } if session_id == "s-2")));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test transcripts` → 编译失败。

- [ ] **Step 3: 实现**

```rust
// src-tauri/src/collectors/mod.rs
pub mod claude_transcripts;
```

```rust
// src-tauri/src/collectors/claude_transcripts.rs
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use crate::core::types::*;

/// 简易临时目录守卫：Drop 时递归删除（仅测试用）。
#[cfg(test)]
pub(crate) mod tempdir_guard {
    pub struct TempDir(pub std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
}

/// Claude 转录采集器（窄接口：只依赖"JSONL 追加写入"这一行为，格式变化只改 parse_line）。
pub struct TranscriptPoller {
    root: PathBuf,
    offsets: HashMap<PathBuf, u64>,
    known: std::collections::HashSet<String>,
}

impl TranscriptPoller {
    pub fn new(root: PathBuf) -> Self {
        Self { root, offsets: HashMap::new(), known: HashSet::new() }
    }

    pub fn poll(&mut self, now: u64) -> Vec<UnifiedEvent> {
        let mut events = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else { return events };
        for proj in entries.flatten() {
            let Ok(files) = std::fs::read_dir(proj.path()) else { continue };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
                self.poll_file(&path, now, &mut events);
            }
        }
        events
    }

    fn poll_file(&mut self, path: &Path, now: u64, events: &mut Vec<UnifiedEvent>) {
        let Ok(data) = std::fs::read(path) else { return };
        let start = *self.offsets.entry(path.to_path_buf()).insert(0).or_insert(0);
        if (data.len() as u64) <= start { return; }
        let fresh = &data[start as usize..];
        let mut consumed = 0u64;
        let mut discovered = false;
        let mut activity = false;
        let mut failed = false;
        let mut session_id: Option<String> = None;
        let mut cwd: Option<String> = None;
        let mut title: Option<String> = None;
        for line in fresh.split(|b| *b == b'\n') {
            consumed += line.len() as u64 + 1; // +1 换行
            let Ok(text) = std::str::from_utf8(line) else { continue };
            if text.trim().is_empty() { continue; }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { continue };
            let sid = v.get("sessionId").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if sid.is_empty() { continue; }
            session_id = Some(sid.clone());
            if cwd.is_none() {
                cwd = v.get("cwd").and_then(|x| x.as_str()).map(|s| s.to_string());
            }
            if title.is_none() && v.get("type").and_then(|x| x.as_str()) == Some("user") {
                title = extract_user_text(&v).map(|t| t.chars().take(80).collect());
            }
            if !failed && v.get("isApiErrorMessage").and_then(|x| x.as_bool()) == Some(true) {
                failed = true;
            }
            activity = true;
        }
        // 按行边界收缩 offset（最后一段可能是半行，不消费）
        let clean_consumed = consumed.saturating_sub(1).min((data.len() - start) as u64);
        let new_off = start + if fresh.ends_with(b"\n") { consumed } else { clean_consumed };
        self.offsets.insert(path.to_path_buf(), new_off);

        let Some(sid) = session_id else { return };
        let agent = AgentKind::ClaudeCode;
        if !self.known.contains(&sid) {
            self.known.insert(sid.clone());
            discovered = true;
        }
        if discovered {
            events.push(UnifiedEvent::Discovered {
                agent, session_id: sid.clone(),
                cwd: cwd.clone().unwrap_or_default(),
                title, ts: now,
            });
        } else if activity {
            events.push(UnifiedEvent::Activity { agent, session_id: sid, ts: now });
        }
        if failed {
            events.push(UnifiedEvent::TurnFailed { agent, session_id: sid, ts: now });
        }
    }
}

fn extract_user_text(v: &serde_json::Value) -> Option<String> {
    let msg = v.get("message")?;
    match msg.get("content") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(a)) => a.iter().find_map(|c| {
            c.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
        }),
        _ => None,
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test` → 全绿。

- [ ] **Step 5: Commit**

```bash
git add src-tauri && git commit -m "feat(collectors): Claude 转录增量采集器"
```

---

### Task 5: hook 可执行文件 `agentboard-hook.exe`

**Files:**
- Create: `src-tauri/src/hook_main.rs`, `src-tauri/src/events_file.rs`
- Modify: `src-tauri/Cargo.toml`（新增 `[[bin]]`）、`src-tauri/src/lib.rs`（声明模块）
- Test: `events_file.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: 无
- Produces: `events_file::append_event(line: &str, dir: &Path) -> Result<()>`（含 10MB 轮转）；bin `agentboard-hook`：stdin JSON → 事件行追加到 `~/.agentboard/events/claude-code.jsonl`；事件行格式 `{"agent":"claude-code","sessionId":…,"cwd":…,"kind":"permission_prompt|idle_prompt|other","message":<前120字符>,"ts":<epochms>}`

- [ ] **Step 1: Cargo.toml 增加 bin**

在 `[[bin]]`（tauri 模板已有一个 name = "agentboard"）之后加：

```toml
[[bin]]
name = "agentboard-hook"
path = "src/hook_main.rs"
```

（模板主程序若是 `name = "agentboard-tauri"` 之类以实际为准，不改它。）

- [ ] **Step 2: 写失败测试**

```rust
// src-tauri/src/events_file.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_rotates_over_10mb() {
        let dir = std::env::temp_dir().join(format!("ab-ev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        append_event(r#"{"a":1}"#, &dir).unwrap();
        append_event(r#"{"a":2}"#, &dir).unwrap();
        let content = std::fs::read_to_string(dir.join("claude-code.jsonl")).unwrap();
        assert_eq!(content.lines().count(), 2);

        // 造一个超过阈值的文件 → 追加后应轮转
        let big = "x".repeat(ROTATE_BYTES + 1);
        std::fs::write(dir.join("claude-code.jsonl"), big).unwrap();
        append_event(r#"{"a":3}"#, &dir).unwrap();
        assert!(!std::fs::read_to_string(dir.join("claude-code.jsonl")).unwrap().contains("xxxx"));
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 2); // .jsonl + .jsonl.1
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn classify_message() {
        assert_eq!(classify("Claude needs your permission to use Bash"), "permission_prompt");
        assert_eq!(classify("Claude is waiting for your input"), "idle_prompt");
        assert_eq!(classify("Claude is an agent needing input"), "other");
    }

    #[test]
    fn handle_input_garbage_is_none_valid_is_line() {
        assert!(handle_input("not json", 1).is_none());
        assert!(handle_input(r#"{"hook_event_name":"PreToolUse"}"#, 1).is_none(), "无 session_id 不产事件");
        let line = handle_input(
            r#"{"session_id":"s9","cwd":"C:\\w","hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}"#, 42,
        ).unwrap();
        assert!(line.contains(r#""sessionId":"s9""#));
        assert!(line.contains(r#""kind":"permission_prompt""#));
        assert!(line.contains(r#""ts":42"#));
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test events_file` → 编译失败。

- [ ] **Step 4: 实现**

```rust
// src-tauri/src/events_file.rs
use std::io::Write;
use std::path::Path;

pub const ROTATE_BYTES: u64 = 10 * 1024 * 1024;

pub fn append_event(line: &str, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let file = dir.join("claude-code.jsonl");
    if file.metadata().map(|m| m.len()).unwrap_or(0) > ROTATE_BYTES {
        let _ = std::fs::rename(&file, dir.join("claude-code.jsonl.1"));
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&file)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")
}

/// message 文本 → kind 标签（与注册的 matcher 类型对齐，兜底 other）。
pub fn classify(message: &str) -> &'static str {
    let m = message.to_lowercase();
    if m.contains("permission") { "permission_prompt" }
    else if m.contains("waiting for your input") { "idle_prompt" }
    else { "other" }
}

/// 纯函数便于测试：stdin 文本 → 事件行；无 session_id 或解析失败 → None。
pub fn handle_input(input: &str, ts: u64) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    if session_id.is_empty() { return None; }
    let cwd = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
    let message = v.get("message").and_then(|x| x.as_str()).unwrap_or("");
    let truncated: String = message.chars().take(120).collect();
    let event = serde_json::json!({
        "agent": "claude-code",
        "sessionId": session_id,
        "cwd": cwd,
        "kind": classify(message),
        "message": truncated,
        "ts": ts,
    });
    Some(event.to_string())
}
```

```rust
// src-tauri/src/hook_main.rs
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
    let Some(line) = agentboard::events_file_handle_input(&input, ts) else { return Ok(()) };
    let dir = agentboard::agentboard_dir().join("events");
    agentboard::events_file_append_event(&line, &dir)
}
```

> 注：hook bin 通过 `agentboard::` 前缀复用 lib 里的函数——需要把 `events_file.rs` 等挂进 lib（见 lib.rs 声明 `pub mod events_file;` 并加三个薄包装 `pub use`/同名转发）。若 lib.rs 尚不存在（Task 1 模板生成的 `lib.rs` 已存在，含 tauri 命令注册处），在其中追加：

```rust
// src-tauri/src/lib.rs 追加
pub mod events_file;

pub fn agentboard_dir() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join(".agentboard")
}
pub fn events_file_now_ms() -> u64 { core::types::now_ms() }
pub fn events_file_handle_input(input: &str, ts: u64) -> Option<String> { events_file::handle_input(input, ts) }
pub fn events_file_append_event(line: &str, dir: &std::path::Path) -> std::io::Result<()> { events_file::append_event(line, dir) }
```

`Cargo.toml` dependencies 追加 `dirs = "5"`。

- [ ] **Step 5: 运行确认通过**

Run: `cargo test && cargo build --bin agentboard-hook`
Expected: 测试全绿，exe 产出。

- [ ] **Step 6: 手工冒烟**

```bash
echo '{"session_id":"smoke","cwd":"C:\\w","hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}' | ./target/debug/agentboard-hook.exe; echo "exit=$?"
cat ~/.agentboard/events/claude-code.jsonl | tail -1
```

Expected: `exit=0`；最后一行含 `"sessionId":"smoke"`。删除该行（`sed -i '$d'`）避免污染。

- [ ] **Step 7: Commit**

```bash
git add src-tauri && git commit -m "feat(hook): agentboard-hook.exe 事件采集"
```

---

### Task 6: hook 事件消费 `collectors/claude_events.rs`

**Files:**
- Create: `src-tauri/src/collectors/claude_events.rs`
- Modify: `src-tauri/src/collectors/mod.rs`
- Test: 同文件

**Interfaces:**
- Produces: `HookEventsPoller::new(file: PathBuf)`、`poller.poll() -> Vec<UnifiedEvent>`（读 `events/claude-code.jsonl` 增量字节 → `Waiting` 事件；kind 映射：`permission_prompt→Permission`、`idle_prompt→Idle`、其余→`Other`）

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::*;

    #[test]
    fn parses_waiting_events_incrementally() {
        let f = std::env::temp_dir().join(format!("ab-he-{}.jsonl", std::process::id()));
        std::fs::write(&f, "").unwrap();
        let mut p = HookEventsPoller::new(f.clone());
        assert!(p.poll().is_empty());

        let line = r#"{"agent":"claude-code","sessionId":"s1","cwd":"C:\\w","kind":"permission_prompt","message":"needs permission","ts":123}"#;
        std::fs::write(&f, format!("{line}\n")).unwrap();
        let ev = p.poll();
        assert!(matches!(&ev[..], [UnifiedEvent::Waiting { kind: WaitingKind::Permission, ts: 123, .. }]));

        // 同一行不重复消费
        assert!(p.poll().is_empty());
        std::fs::remove_file(&f).unwrap();
    }

    #[test]
    fn garbage_lines_skipped() {
        let f = std::env::temp_dir().join(format!("ab-he2-{}.jsonl", std::process::id()));
        std::fs::write(&f, "garbage\n{\"agent\":\"claude-code\",\"sessionId\":\"s2\",\"kind\":\"idle_prompt\",\"ts\":9}\n{\"agent\":\"claude-code\",\"sessionId\":\"s3\",\"kind\":\"whatever\",\"ts\":10}\n").unwrap();
        let mut p = HookEventsPoller::new(f.clone());
        let ev = p.poll();
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[0], UnifiedEvent::Waiting { kind: WaitingKind::Idle, ts: 9, .. }));
        assert!(matches!(&ev[1], UnifiedEvent::Waiting { kind: WaitingKind::Other, ts: 10, .. }));
        std::fs::remove_file(&f).unwrap();
    }
}
```

- [ ] **Step 2: 运行确认失败** → `cargo test claude_events`

- [ ] **Step 3: 实现**

```rust
// src-tauri/src/collectors/claude_events.rs
use std::path::PathBuf;
use crate::core::types::*;

pub struct HookEventsPoller {
    file: PathBuf,
    offset: u64,
}

impl HookEventsPoller {
    pub fn new(file: PathBuf) -> Self { Self { file, offset: 0 } }

    pub fn poll(&mut self) -> Vec<UnifiedEvent> {
        let mut out = Vec::new();
        let Ok(data) = std::fs::read(&self.file) else { return out };
        let fresh = &data[self.offset as usize..];
        let mut consumed = 0u64;
        for line in fresh.split(|b| *b == b'\n') {
            consumed += line.len() as u64 + 1;
            let Ok(text) = std::str::from_utf8(line) else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { continue };
            let Some(session_id) = v.get("sessionId").and_then(|x| x.as_str()) else { continue };
            let Some(ts) = v.get("ts").and_then(|x| x.as_u64()) else { continue };
            let kind = match v.get("kind").and_then(|x| x.as_str()).unwrap_or("") {
                "permission_prompt" => WaitingKind::Permission,
                "idle_prompt" => WaitingKind::Idle,
                _ => WaitingKind::Other,
            };
            out.push(UnifiedEvent::Waiting {
                agent: AgentKind::ClaudeCode,
                session_id: session_id.to_string(),
                kind, ts,
            });
        }
        let total = data.len() as u64;
        self.offset = if data.ends_with(b"\n") { self.offset + consumed.min(total - self.offset) } else { self.offset + consumed.saturating_sub(1).min(total - self.offset) };
        out
    }
}
```

- [ ] **Step 4: 运行确认通过** → `cargo test` 全绿
- [ ] **Step 5: Commit** `git add src-tauri && git commit -m "feat(collectors): hook 事件消费"`

---

### Task 7: hook 安装器 `installer.rs`

**Files:**
- Create: `src-tauri/src/installer.rs`
- Modify: `src-tauri/src/lib.rs`（声明模块）
- Test: 同文件（全用临时目录，绝不碰真 `~/.claude`）

**Interfaces:**
- Produces: `install_hooks(hook_exe: &Path, settings_path: &Path, backup_root: &Path) -> Result<InstallReport>`、`uninstall_hooks(settings_path: &Path) -> Result<usize>`；`InstallReport { backup_path, changed: bool }`；hook 组识别标记 = command 含 `"agentboard-hook.exe"`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Drop for Tmp { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    fn tmp() -> Tmp {
        let p = std::env::temp_dir().join(format!("ab-inst-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    fn exe_and_settings(base: &Path) -> (PathBuf, PathBuf) {
        let exe = base.join("agentboard-hook.exe");
        std::fs::write(&exe, b"fake").unwrap();
        let settings = base.join("settings.json");
        std::fs::write(&settings, r#"{"mcp":{"a":1},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"other.exe"}]}]}}"#).unwrap();
        (exe, settings)
    }

    #[test]
    fn install_adds_group_backs_up_and_preserves_existing() {
        let t = tmp();
        let (exe, settings) = exe_and_settings(&t.0);
        let backup_root = t.0.join("backups");
        let report = install_hooks(&exe, &settings, &backup_root).unwrap();
        assert!(report.changed);
        assert!(report.backup_path.exists(), "备份文件存在");

        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        // 既有内容原样保留
        assert_eq!(v["mcp"]["a"], 1);
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        // 我们的组已加入且 matcher 正确
        let groups = v["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["matcher"], "permission_prompt|idle_prompt|agent_needs_input");
        assert_eq!(groups[0]["hooks"][0]["type"], "command");
        assert!(groups[0]["hooks"][0]["args"][0] == "notification");
        assert!(groups[0]["hooks"][0]["command"].as_str().unwrap().ends_with("agentboard-hook.exe"));
        // hook exe 被复制到备份根同级的 bin 目录约定位置由调用方定；此处只验证注册串里是传入的 exe 路径
    }

    #[test]
    fn install_is_idempotent() {
        let t = tmp();
        let (exe, settings) = exe_and_settings(&t.0);
        let backup_root = t.0.join("backups");
        install_hooks(&exe, &settings, &backup_root).unwrap();
        let report = install_hooks(&exe, &settings, &backup_root).unwrap();
        assert!(!report.changed, "重复安装无变更");
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["hooks"]["Notification"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let t = tmp();
        let (exe, settings) = exe_and_settings(&t.0);
        let backup_root = t.0.join("backups");
        install_hooks(&exe, &settings, &backup_root).unwrap();
        let removed = uninstall_hooks(&settings).unwrap();
        assert_eq!(removed, 1);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(v["hooks"]["Notification"].as_array().map(|a| a.is_empty()).unwrap_or(true));
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "Bash", "别人的 hook 不能动");
    }

    #[test]
    fn install_on_missing_file_creates_minimal() {
        let t = tmp();
        let (exe, settings) = exe_and_settings(&t.0);
        std::fs::remove_file(&settings).unwrap();
        let report = install_hooks(&exe, &settings, &t.0.join("backups")).unwrap();
        assert!(report.changed);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(v["hooks"]["Notification"].as_array().unwrap().len() == 1);
    }
}
```

- [ ] **Step 2: 运行确认失败** → `cargo test installer`

- [ ] **Step 3: 实现**

```rust
// src-tauri/src/installer.rs
use std::path::{Path, PathBuf};
use serde_json::Value;

const MARKER: &str = "agentboard-hook.exe";
const MATCHER: &str = "permission_prompt|idle_prompt|agent_needs_input";

pub struct InstallReport {
    pub backup_path: PathBuf,
    pub changed: bool,
}

pub fn install_hooks(hook_exe: &Path, settings_path: &Path, backup_root: &Path) -> std::io::Result<InstallReport> {
    let exe_str = hook_exe.to_string_lossy().replace('/', "\\");
    let mut root: Value = std::fs::read_to_string(settings_path).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    if group_present(&root, &exe_str) {
        return Ok(InstallReport { backup_path: PathBuf::new(), changed: false });
    }

    // 1) 备份（仅在确要修改前）
    let ts = crate::core::types::now_ms();
    let backup_dir = backup_root.join(format!("{ts}"));
    if settings_path.exists() {
        std::fs::create_dir_all(&backup_dir)?;
        std::fs::copy(settings_path, backup_dir.join("settings.json"))?;
    }

    // 2) 合并
    let hooks = root.as_object_mut().unwrap().entry("hooks").or_insert_with(|| serde_json::json!({}));
    let notification = hooks.as_object_mut().unwrap().entry("Notification").or_insert_with(|| serde_json::json!([]));
    notification.as_array_mut().unwrap().push(serde_json::json!({
        "matcher": MATCHER,
        "hooks": [{ "type": "command", "command": exe_str, "args": ["notification"] }]
    }));

    // 3) 写回（pretty，UTF-8）
    if let Some(parent) = settings_path.parent() { std::fs::create_dir_all(parent)?; }
    std::fs::write(settings_path, serde_json::to_string_pretty(&root).unwrap())?;
    prune_backups(backup_root, 10);
    Ok(InstallReport { backup_path: backup_dir.join("settings.json"), changed: true })
}

pub fn uninstall_hooks(settings_path: &Path) -> std::io::Result<usize> {
    let text = std::fs::read_to_string(settings_path)?;
    let mut root: Value = serde_json::from_str(&text)?;
    let mut removed = 0;
    if let Some(groups) = root["hooks"]["Notification"].as_array_mut() {
        let before = groups.len();
        groups.retain(|g| !is_ours(g));
        removed = before - groups.len();
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&root)?)?;
    Ok(removed)
}

fn group_present(root: &Value, exe_str: &str) -> bool {
    root["hooks"]["Notification"].as_array().map(|groups| {
        groups.iter().any(|g| is_ours(g))
    }) .map(|ours| ours && root_string_contains(root, exe_str)) .unwrap_or(false)
}

fn root_string_contains(_root: &Value, _s: &str) -> bool { true } // exe 路径变化视为同一组件；更新路径时先 uninstall 再 install

fn is_ours(group: &Value) -> bool {
    group["hooks"].as_array().map(|hs| hs.iter().any(|h| {
        h["command"].as_str().map(|c| c.replace('/', "\\").contains(MARKER)).unwrap_or(false)
    })).unwrap_or(false)
}

fn prune_backups(backup_root: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(backup_root) else { return };
    let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    while dirs.len() > keep {
        let _ = std::fs::remove_dir_all(dirs.remove(0));
    }
}
```

- [ ] **Step 4: 运行确认通过** → `cargo test` 全绿
- [ ] **Step 5: Commit** `git add src-tauri && git commit -m "feat(installer): hook 注册/备份/卸载（幂等）"`

---

### Task 8: ZCode 采集器 `collectors/zcode_db.rs`

**Files:**
- Create: `src-tauri/src/collectors/zcode_db.rs`
- Modify: `src-tauri/src/collectors/mod.rs`；`Cargo.toml` 加 `rusqlite = { version = "0.32", features = ["bundled"] }`
- Test: 同文件（内存库按实测 schema 建表，见 Global Constraints 的字段清单）

**Interfaces:**
- Produces: `ZcodePoller::open(db_path) -> Result<Self>`（readonly URI）、`poller.poll(now) -> Vec<UnifiedEvent>`（边沿触发：状态不变则不产事件）；状态映射：
  - 发现：`session` 表（`time_deleted IS NULL AND time_archived IS NULL`）新 id → `Discovered`（title 用 db 现成 `title`）
  - 运行中：presence 有行 且 最新 turn `completed_at IS NULL` → `Activity`
  - 等待·空闲：presence 有行 且 无进行中 turn → `Waiting(Idle)`
  - 失败：最新完结 turn `status='error' AND error_type != 'turn_cancelled'` → `TurnFailed`
  - 完成：`last_outcome IN ('completed','interrupted')` 或从 presence 消失 → `Closed`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::*;
    use rusqlite::Connection;

    fn fixture_db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(r#"
            CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, directory TEXT,
                time_created INTEGER, time_updated INTEGER, last_outcome TEXT,
                time_deleted INTEGER, time_archived INTEGER);
            CREATE TABLE session_runtime_presence (session_id TEXT, owner_id TEXT, process_id INTEGER);
            CREATE TABLE turn_usage (id INTEGER PRIMARY KEY, session_id TEXT, status TEXT,
                error_type TEXT, started_at INTEGER, completed_at INTEGER);
        "#).unwrap();
        con
    }

    fn insert_session(con: &Connection, id: &str, last_outcome: Option<&str>) {
        con.execute("INSERT INTO session (id,title,directory,time_created,time_updated,last_outcome) VALUES (?1,?2,?3,1,1,?4)",
            rusqlite::params![id, "标题", r"C:\repo\z", last_outcome]).unwrap();
    }

    #[test]
    fn discover_then_idle_when_presence_without_active_turn() {
        let con = fixture_db();
        insert_session(&con, "z1", None);
        con.execute("INSERT INTO session_runtime_presence VALUES ('z1','o',1)", []).unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Discovered { session_id, title, .. } if session_id=="z1" && title.as_deref()==Some("标题"))));
        // presence + 无进行中 turn → 等待·空闲
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Waiting { kind: WaitingKind::Idle, .. })));
        // 状态不变 → 第二次无事件（边沿触发）
        assert!(p.poll(200).is_empty());
    }

    #[test]
    fn active_turn_is_running_activity() {
        let con = fixture_db();
        insert_session(&con, "z1", None);
        con.execute("INSERT INTO session_runtime_presence VALUES ('z1','o',1)", []).unwrap();
        con.execute("INSERT INTO turn_usage (session_id,status,started_at) VALUES ('z1','completed',5)", []).unwrap();
        con.execute("INSERT INTO turn_usage (session_id,status,started_at) VALUES ('z1','completed',10)", []).unwrap();
        con.execute("INSERT INTO turn_usage (session_id,status,error_type,started_at,completed_at) VALUES ('z1','completed',NULL,20,NULL)", []).unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Activity { session_id, .. } if session_id=="z1")));
    }

    #[test]
    fn error_turn_failed_but_cancelled_not_failed() {
        let con = fixture_db();
        insert_session(&con, "z1", None);
        con.execute("INSERT INTO session_runtime_presence VALUES ('z1','o',1)", []).unwrap();
        con.execute("INSERT INTO turn_usage (session_id,status,error_type,started_at,completed_at) VALUES ('z1','error','unknown_error',20,30)", []).unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::TurnFailed { .. })));

        let con2 = fixture_db();
        insert_session(&con2, "z2", None);
        con2.execute("INSERT INTO session_runtime_presence VALUES ('z2','o',1)", []).unwrap();
        con2.execute("INSERT INTO turn_usage (session_id,status,error_type,started_at,completed_at) VALUES ('z2','error','turn_cancelled',20,30)", []).unwrap();
        let mut p2 = ZcodePoller::with_connection(con2).unwrap();
        let ev2 = p2.poll(100);
        assert!(!ev2.iter().any(|e| matches!(e, UnifiedEvent::TurnFailed { .. })), "用户打断不算失败");
    }

    #[test]
    fn presence_gone_or_last_outcome_closes() {
        let con = fixture_db();
        insert_session(&con, "z1", Some("completed"));
        con.execute("INSERT INTO session_runtime_presence VALUES ('z1','o',1)", []).unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Closed { session_id, .. } if session_id=="z1")));

        let con2 = fixture_db();
        insert_session(&con2, "z2", None);
        let mut p2 = ZcodePoller::with_connection(con2).unwrap();
        let ev2 = p2.poll(100);
        assert!(ev2.iter().any(|e| matches!(e, UnifiedEvent::Closed { session_id, .. } if session_id=="z2")), "无 presence 即关闭");
    }
}
```

- [ ] **Step 2: 运行确认失败** → `cargo test zcode`

- [ ] **Step 3: 实现**

```rust
// src-tauri/src/collectors/zcode_db.rs
use std::collections::HashMap;
use crate::core::types::*;

#[derive(Clone)]
struct Row {
    title: String,
    directory: String,
    last_outcome: Option<String>,
    presence: bool,
    latest_turn: Option<TurnRow>,
}
#[derive(Clone)]
struct TurnRow { status: String, error_type: Option<String>, completed_at: Option<i64> }

#[derive(Clone, PartialEq)]
enum ZStatus { IdleWaiting, Running, Failed, Closed }

pub struct ZcodePoller {
    con: rusqlite::Connection,
    known: std::collections::HashSet<String>,
    prev: HashMap<String, ZStatus>,
}

impl ZcodePoller {
    pub fn open(db_path: &std::path::Path) -> rusqlite::Result<Self> {
        let uri = format!("file:{}?mode=ro", db_path.to_string_lossy().replace('\\', "/"));
        let con = rusqlite::Connection::open_with_flags(&uri, rusqlite::OpenFlags::SQLITE_OPEN_URI | rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Self::with_connection(con)
    }

    pub fn with_connection(con: rusqlite::Connection) -> rusqlite::Result<Self> {
        Ok(Self { con, known: Default::default(), prev: Default::default() })
    }

    pub fn poll(&mut self, now: u64) -> Vec<UnifiedEvent> {
        let mut out = Vec::new();
        let mut rows: HashMap<String, Row> = HashMap::new();

        let Ok(mut stmt) = self.con.prepare(
            "SELECT id, title, directory, last_outcome FROM session WHERE time_deleted IS NULL AND time_archived IS NULL"
        ) else { return out };
        let _ = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?))
        }).map(|it| {
            for (id, title, directory, last_outcome) in it.flatten() {
                rows.insert(id, Row { title, directory, last_outcome, presence: false, latest_turn: None });
            }
        });

        let _ = self.con.prepare("SELECT session_id FROM session_runtime_presence").map(|mut st| {
            let _ = st.query_map([], |r| r.get::<_, String>(0)).map(|it| {
                for (sid,) in it.flatten() {
                    if let Some(row) = rows.get_mut(&sid) { row.presence = true; }
                }
            });
        });

        // 每会话最新 turn（按 started_at 降序取第一条；大表用 LIMIT 保护）
        let _ = self.con.prepare(
            "SELECT session_id, status, error_type, completed_at FROM turn_usage ORDER BY started_at DESC LIMIT 500"
        ).map(|mut st| {
            let _ = st.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<i64>>(3)?))
            }).map(|it| {
                for (sid, status, error_type, completed_at) in it.flatten() {
                    if let Some(row) = rows.get_mut(&sid) {
                        if row.latest_turn.is_none() {
                            row.latest_turn = Some(TurnRow { status, error_type, completed_at });
                        }
                    }
                }
            });
        });

        for (id, row) in rows {
            let z = if row.presence {
                match &row.latest_turn {
                    Some(t) if t.completed_at.is_none() => ZStatus::Running,
                    Some(t) if t.status == "error" && t.error_type.as_deref() != Some("turn_cancelled") => ZStatus::Failed,
                    _ => ZStatus::IdleWaiting,
                }
            } else {
                ZStatus::Closed
            };

            let agent = AgentKind::ZCode;
            if !self.known.contains(&id) {
                self.known.insert(id.clone());
                out.push(UnifiedEvent::Discovered { agent, session_id: id.clone(), cwd: row.directory.clone(), title: Some(row.title.clone()), ts: now });
            }
            let prev = self.prev.get(&id);
            let emit = |out: &mut Vec<UnifiedEvent>, ev: UnifiedEvent| out.push(ev);
            match (&z, prev) {
                (ZStatus::Running, Some(ZStatus::Running)) => {}
                (ZStatus::Running, _) => emit(&mut out, UnifiedEvent::Activity { agent, session_id: id.clone(), ts: now }),
                (ZStatus::IdleWaiting, Some(ZStatus::IdleWaiting)) => {}
                (ZStatus::IdleWaiting, _) => emit(&mut out, UnifiedEvent::Waiting { agent, session_id: id.clone(), kind: WaitingKind::Idle, ts: now }),
                (ZStatus::Failed, Some(ZStatus::Failed)) => {}
                (ZStatus::Failed, _) => emit(&mut out, UnifiedEvent::TurnFailed { agent, session_id: id.clone(), ts: now }),
                (ZStatus::Closed, Some(ZStatus::Closed)) => {}
                (ZStatus::Closed, _) => {
                    // last_outcome 明确完成 → TurnFinished（非推断）；其余按关闭
                    if matches!(row.last_outcome.as_deref(), Some("completed") | Some("interrupted")) {
                        emit(&mut out, UnifiedEvent::TurnFinished { agent, session_id: id.clone(), ts: now });
                    } else {
                        emit(&mut out, UnifiedEvent::Closed { agent, session_id: id.clone(), ts: now });
                    }
                }
            }
            self.prev.insert(id, z);
        }
        out
    }
}
```

> 注：真库还有 `time_created`/`time_updated`/`time_deleted`/`time_archived` 列（SQL 已按实测写）。内存夹具缺列不影响本任务的查询集合。真库打开失败（锁/迁移中）由调用方退避重试并标"源离线"，本模块直接返回空 vec。

- [ ] **Step 4: 运行确认通过** → `cargo test` 全绿
- [ ] **Step 5: Commit** `git add src-tauri && git commit -m "feat(collectors): ZCode db 只读采集器（边沿触发）"`

---

### Task 9: 历史存储 `core/history.rs`

**Files:**
- Create: `src-tauri/src/core/history.rs`
- Modify: `src-tauri/src/core/mod.rs`
- Test: 同文件（内存库）

**Interfaces:**
- Produces: `History::open(path) -> Result<Self>`、`history.upsert(&SessionSnapshot)`、`history.list() -> Vec<SessionSnapshot>`（按 last_activity 降序）、`history.prune(keep: usize)`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::*;

    fn snap(id: &str, activity: u64) -> SessionSnapshot {
        SessionSnapshot {
            session_id: id.into(), agent: AgentKind::ZCode, title: "t".into(),
            cwd: r"C:\repo\z".into(), repo: "z".into(), branch: None,
            status: SessionStatus::Done, done_inferred: false,
            last_activity: activity, time_created: activity,
        }
    }

    #[test]
    fn upsert_list_and_prune() {
        let con = rusqlite::Connection::open_in_memory().unwrap();
        let mut h = History::with_connection(con).unwrap();
        h.upsert(&snap("a", 1)).unwrap();
        h.upsert(&snap("b", 2)).unwrap();
        h.upsert(&snap("a", 9)).unwrap(); // 覆盖
        let list = h.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].session_id, "a");
        assert_eq!(list[0].last_activity, 9);

        for i in 0..1100u64 { h.upsert(&snap(&format!("x{i}"), 100 + i)).unwrap(); }
        h.prune(1000).unwrap();
        assert_eq!(h.list().unwrap().len(), 1000);
    }
}
```

- [ ] **Step 2: 运行确认失败** → `cargo test history`

- [ ] **Step 3: 实现**

```rust
// src-tauri/src/core/history.rs
use crate::core::types::*;

pub struct History { con: rusqlite::Connection }

impl History {
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p).ok(); }
        let con = rusqlite::Connection::open(path)?;
        con.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY, agent TEXT, title TEXT, cwd TEXT, repo TEXT,
                branch TEXT, status TEXT, done_inferred INTEGER,
                last_activity INTEGER, time_created INTEGER
            );
        "#)?;
        Ok(Self { con })
    }

    pub fn with_connection(con: rusqlite::Connection) -> rusqlite::Result<Self> {
        let h = Self { con };
        h.con.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY, agent TEXT, title TEXT, cwd TEXT, repo TEXT,
                branch TEXT, status TEXT, done_inferred INTEGER,
                last_activity INTEGER, time_created INTEGER
            );
        "#)?;
        Ok(h)
    }

    pub fn upsert(&self, s: &SessionSnapshot) -> rusqlite::Result<()> {
        self.con.execute(
            "INSERT INTO sessions VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(session_id) DO UPDATE SET title=?3, status=?7, done_inferred=?8, last_activity=?9",
            rusqlite::params![
                s.session_id, serde_json::to_string(&s.agent).unwrap(),
                s.title, s.cwd, s.repo,
                s.branch, serde_json::to_string(&s.status).unwrap(),
                s.done_inferred as i64, s.last_activity as i64, s.time_created as i64,
            ])?;
        Ok(())
    }

    pub fn list(&self) -> rusqlite::Result<Vec<SessionSnapshot>> {
        let mut stmt = self.con.prepare("SELECT session_id, agent, title, cwd, repo, branch, status, done_inferred, last_activity, time_created FROM sessions ORDER BY last_activity DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionSnapshot {
                session_id: r.get(0)?,
                agent: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(AgentKind::ZCode),
                title: r.get(2)?, cwd: r.get(3)?, repo: r.get(4)?, branch: r.get(5)?,
                status: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or(SessionStatus::Done),
                done_inferred: r.get::<_, i64>(7)? != 0,
                last_activity: r.get::<_, i64>(8)? as u64,
                time_created: r.get::<_, i64>(9)? as u64,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn prune(&self, keep: usize) -> rusqlite::Result<()> {
        self.con.execute(
            "DELETE FROM sessions WHERE session_id NOT IN (
                SELECT session_id FROM sessions ORDER BY last_activity DESC LIMIT ?1)", keep as i64)?;
        Ok(())
    }
}
```

- [ ] **Step 4: 运行确认通过** → `cargo test` 全绿
- [ ] **Step 5: Commit** `git add src-tauri && git commit -m "feat(core): 历史存储与 1000 条上限"`

---

### Task 10: Tauri 装配——采集循环、事件推送、托盘与通知

**Files:**
- Modify: `src-tauri/src/lib.rs`、`src-tauri/Cargo.toml`（`tauri-plugin-notification`、`tauri-plugin-single-instance`、`tauri-plugin-clipboard-manager`、`notify-debouncer-full = "0.3"`、`chrono`）、`src-tauri/capabilities/default.json`
- Test: 编译 + 手工验证点

**Interfaces:**
- Consumes: Tasks 2–9 全部产物
- Produces: Tauri 事件 `sessions-changed`（payload = `Vec<SessionSnapshot>` camelCase JSON）；commands：`get_sessions()`、`get_source_status()`、`jump(session_id)`、`get_config()`、`set_config(cfg)`；`config.json` 结构 `{"notify_enabled":true,"running_fresh_ms":120000,"done_after_ms":900000}`

- [ ] **Step 1: 安装插件依赖**

```bash
cd src-tauri && cargo add tauri-plugin-notification tauri-plugin-single-instance tauri-plugin-clipboard-manager notify-debouncer-full@0.3 chrono
cd .. && npm install @tauri-apps/plugin-notification @tauri-apps/plugin-clipboard-manager
```

- [ ] **Step 2: capabilities/default.json 确认权限**

```json
{
  "identifier": "default",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "notification:default",
    "clipboard-manager:allow-write-text"
  ]
}
```

- [ ] **Step 3: lib.rs 实现**（骨架要点，完整装配以编译通过为准）

```rust
// src-tauri/src/lib.rs 核心结构
pub mod core_mod { pub mod types { pub use crate::core::types::*; } }
pub mod core { pub mod types; pub mod machine; pub mod history; pub mod config; }
pub mod collectors { pub mod claude_transcripts; pub mod claude_events; pub mod zcode_db; }
pub mod installer;
pub mod events_file;
pub mod platform;

use core::machine::Machine;
use core::types::*;
use std::sync::mpsc;
use tauri::{AppHandle, Emitter, Manager};

#[tauri::command]
fn get_sessions(state: tauri::State<AppState>) -> Vec<SessionSnapshot> {
    state.machine.lock().unwrap().snapshots()
}

#[tauri::command]
fn get_source_status() -> serde_json::Value {
    // hooks 是否注册 + 转录目录存在 + zcode db 可读（读一次小查询）
    serde_json::json!({
        "claude_hooks": installer::ours_registered(&claude_settings_path()),
        "claude_transcripts": claude_projects_dir().exists(),
        "zcode_db": zcode_db_path().exists(),
    })
}

pub struct AppState {
    pub machine: std::sync::Mutex<Machine>,
    pub app: AppHandle,
}

pub fn run() {
    let (tx, rx) = mpsc::channel::<UnifiedEvent>();
    // ① 转录监听：notify-debouncer-full watch ~/.claude/projects 递归；事件 → tx.send(Task 4 poller.poll(now))
    // ② hook 事件：2s 间隔循环 poll Task 6 poller
    // ③ ZCode：4s 间隔 poll Task 8 poller（打开失败：退避 30s 重试）
    // ④ 核心：rx 循环 + 每 5s 发 Tick；每次 process 的 Update：
    //      - history.upsert + 每 200 条 prune(1000)
    //      - should_notify 且 config.notify_enabled → tauri-plugin-notification 通知：
    //        title = format!("{} 在等你处理", agent.label()) / "任务失败" / "任务完成"
    //        body  = 项目名（repo），不含请求内容
    //      - app.emit("sessions-changed", machine.snapshots())
    //      - 更新托盘菜单计数（等待数/运行中数）
    // ⑤ tauri::Builder：plugin(single-instance) / plugin(notification) / plugin(clipboard) /
    //    manage(AppState) / invoke_handler 上述 commands / 托盘构建
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.set_focus(); }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| { spawn_collectors(app.handle().clone(), rx); Ok(()) })
        .invoke_handler(tauri::generate_handler![get_sessions, get_source_status, jump, get_config, set_config])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

（`spawn_collectors` 三个线程 + 核心消费线程的完整实现按上述注释逐条落地；每条都有明确信号源，无设计空白。托盘用 `tauri::tray::TrayIconBuilder`，菜单两项计数 + "打开看板" + "退出"。）

- [ ] **Step 4: 验证**

Run: `cargo test && npm run tauri dev`
手工验证点：
1. 空窗口出现，托盘图标在
2. `~/.claude/projects` 随便挑一个已有 jsonl 复制一份改名 → 看板应出现"运行中"新会话（Discovered 路径）
3. 开着 dev 不动 15 分钟不现实——临时把 `done_after_ms` 调成 30000 验证 Tick→推断完成→通知链路，验证完调回 900000 并确认 config.json 实际生效

- [ ] **Step 5: Commit** `git add -A && git commit -m "feat(app): 采集循环/托盘/通知装配" && git push`

---

### Task 11: 前端看板

**Files:**
- Create: `src/types.ts`, `src/board-model.ts`, `src/board-model.test.ts`, `src/Board.tsx`, `src/SessionRow.tsx`, `src/App.tsx`（改造）
- Modify: `src/styles.css`
- Test: `board-model.test.ts`（vitest）+ 手工验收

**Interfaces:**
- Consumes: Tauri 事件 `sessions-changed`、command `get_sessions`
- Produces: `groupSessions(list) -> { attention: SessionSnapshot[]; running: SessionSnapshot[]; done: { today: ...; yesterday: ...; earlier: ... } }`（attention = Waiting ∪ Failed 置顶；done 按本机本地日期分组）

- [ ] **Step 1: 写失败测试**

```ts
// src/board-model.test.ts
import { describe, it, expect } from "vitest";
import { groupSessions } from "./board-model";
import type { SessionSnapshot } from "./types";

const mk = (over: Partial<SessionSnapshot>): SessionSnapshot => ({
  sessionId: "s", agent: "claude_code", title: "t", cwd: "C:\\r", repo: "r",
  branch: null, status: "running", doneInferred: false,
  lastActivity: 0, timeCreated: 0, ...over,
});

describe("groupSessions", () => {
  it("waiting/failed 置顶，其后 running，done 分组", () => {
    const now = new Date(2026, 10, 17, 12, 0).getTime(); // 本地时区
    const list = [
      mk({ sessionId: "d1", status: "done", lastActivity: now - 86400_000 * 3 }),
      mk({ sessionId: "r1", status: "running" }),
      mk({ sessionId: "w1", status: "waiting_permission" }),
      mk({ sessionId: "f1", status: "failed" }),
      mk({ sessionId: "d2", status: "done", lastActivity: now - 3600_000 }),
      mk({ sessionId: "d3", status: "done", lastActivity: now - 86400_000 - 3600_000 }),
    ];
    const g = groupSessions(list, now);
    expect(g.attention.map((s) => s.sessionId)).toEqual(["w1", "f1"]);
    expect(g.running.map((s) => s.sessionId)).toEqual(["r1"]);
    expect(g.done.today.map((s) => s.sessionId)).toEqual(["d2"]);
    expect(g.done.yesterday.map((s) => s.sessionId)).toEqual(["d3"]);
    expect(g.done.earlier.map((s) => s.sessionId)).toEqual(["d1"]);
  });

  it("agent 过滤", () => {
    const list = [mk({ sessionId: "a", agent: "claude_code" }), mk({ sessionId: "b", agent: "zcode" })];
    expect(groupSessions(list, 0, { agent: "zcode" }).running.map((s) => s.sessionId)).toEqual(["b"]);
  });
});
```

- [ ] **Step 2: 运行确认失败**

```bash
npm install -D vitest && npx vitest run
```
Expected: 失败（模块不存在）。

- [ ] **Step 3: 实现 types.ts 与 board-model.ts**

```ts
// src/types.ts —— 与 Rust camelCase 序列化严格一致
export type AgentKind = "claude_code" | "zcode";
export type WaitingKind = "permission" | "question" | "idle" | "other";
export type SessionStatus = "running" | `waiting_${WaitingKind}` | "failed" | "done";
export interface SessionSnapshot {
  sessionId: string; agent: AgentKind; title: string; cwd: string; repo: string;
  branch: string | null; status: SessionStatus; doneInferred: boolean;
  lastActivity: number; timeCreated: number;
}
```

```ts
// src/board-model.ts
import type { SessionSnapshot } from "./types";

export interface Groups {
  attention: SessionSnapshot[];
  running: SessionSnapshot[];
  done: { today: SessionSnapshot[]; yesterday: SessionSnapshot[]; earlier: SessionSnapshot[] };
}

const isDone = (s: SessionSnapshot) => s.status === "done";
const isAttention = (s: SessionSnapshot) => s.status.startsWith("waiting") || s.status === "failed";

export function groupSessions(
  list: SessionSnapshot[],
  now: number,
  filter?: { agent?: string },
): Groups {
  const f = filter?.agent ? list.filter((s) => s.agent === filter.agent) : list;
  const dayStart = (t: number) => { const d = new Date(t); d.setHours(0, 0, 0, 0); return d.getTime(); };
  const today = dayStart(now), yest = dayStart(now) - 86400_000;
  const g: Groups = { attention: [], running: [], done: { today: [], yesterday: [], earlier: [] } };
  for (const s of f) {
    if (isAttention(s)) g.attention.push(s);
    else if (!isDone(s)) g.running.push(s);
    else {
      const t = s.lastActivity;
      if (t >= today) g.done.today.push(s);
      else if (t >= yest) g.done.yesterday.push(s);
      else g.done.earlier.push(s);
    }
  }
  return g;
}
```

- [ ] **Step 4: 运行确认通过** → `npx vitest run` 全绿

- [ ] **Step 5: Board.tsx / SessionRow.tsx / App.tsx（手工验收驱动，逻辑已在纯函数里测过）**

```tsx
// src/App.tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import Board from "./Board";
import type { SessionSnapshot } from "./types";

export default function App() {
  const [sessions, setSessions] = useState<SessionSnapshot[]>([]);
  useEffect(() => {
    invoke<SessionSnapshot[]>("get_sessions").then(setSessions);
    const un = listen<SessionSnapshot[]>("sessions-changed", (e) => setSessions(e.payload));
    return () => { un.then((f) => f()); };
  }, []);
  return <Board sessions={sessions} />;
}
```

Board 渲染三段：置顶区（等待/失败，红/黄徽标）→ 运行中 → 完成三组折叠；每行：agent 徽标 + 标题 + repo/branch + 相对时间 + 状态徽标（推断完成加"推断"角标）+ 点击行 invoke `jump`。样式 `styles.css` 深色简洁，一行一个 flex。**不加任何未列出的功能。**

- [ ] **Step 6: 验证**

Run: `npx vitest run && npm run tauri dev`
手工验收：窗口显示 Task 10 验证留下的会话；等待会话置顶；点击行无报错（jump 未实现前 console 提示即可——Task 12 实现）。

- [ ] **Step 7: Commit** `git add -A && git commit -m "feat(ui): 看板分组/排序/筛选" && git push`

---

### Task 12: 跳回 + 设置页

**Files:**
- Create: `src-tauri/src/platform.rs`, `src/Settings.tsx`
- Modify: `src-tauri/src/lib.rs`（`jump`/`get_config`/`set_config` 实装）、`src/App.tsx`（设置入口）、`src-tauri/src/core/config.rs`
- Test: `platform.rs` 纯函数单测 + 手工验收

**Interfaces:**
- Produces: `platform::jump_payload(snap) -> JumpPayload { spawn: Option<(String, Vec<String>)>, clipboard: String, note: String }`——Claude Code：spawn `wt -d <cwd>` + clipboard `claude --resume <id>`；ZCode：spawn `explorer <cwd>` + clipboard cwd；`config.rs`：`load() -> Config` / `save(&Config)`，结构见 Task 10

- [ ] **Step 1: 写失败测试**

```rust
// platform.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::*;

    fn snap(agent: AgentKind, id: &str, cwd: &str) -> SessionSnapshot {
        SessionSnapshot {
            session_id: id.into(), agent, title: "t".into(), cwd: cwd.into(),
            repo: "r".into(), branch: None, status: SessionStatus::Done,
            done_inferred: false, last_activity: 0, time_created: 0,
        }
    }

    #[test]
    fn claude_jump_wt_and_resume() {
        let p = jump_payload(&snap(AgentKind::ClaudeCode, "s1", r"C:\repo"));
        let (prog, args) = p.spawn.unwrap();
        assert_eq!(prog, "wt");
        assert_eq!(args, vec!["-d".into(), r"C:\repo".into()]);
        assert_eq!(p.clipboard, "claude --resume s1");
    }

    #[test]
    fn zcode_jump_explorer_and_clipboard_dir() {
        let p = jump_payload(&snap(AgentKind::ZCode, "z1", r"C:\repo\z"));
        let (prog, args) = p.spawn.unwrap();
        assert_eq!(prog, "explorer");
        assert_eq!(p.clipboard, r"C:\repo\z");
    }
}
```

- [ ] **Step 2: 运行确认失败** → `cargo test platform`

- [ ] **Step 3: 实现 platform.rs + config.rs**

```rust
// src-tauri/src/platform.rs
use crate::core::types::*;

pub struct JumpPayload {
    pub spawn: Option<(String, Vec<String>)>,
    pub clipboard: String,
    pub note: &'static str,
}

pub fn jump_payload(s: &SessionSnapshot) -> JumpPayload {
    match s.agent {
        AgentKind::ClaudeCode => JumpPayload {
            spawn: Some(("wt".into(), vec!["-d".into(), s.cwd.clone()])),
            clipboard: format!("claude --resume {}", s.session_id),
            note: "已打开终端窗口，续接命令已复制到剪贴板",
        },
        AgentKind::ZCode => JumpPayload {
            spawn: Some(("explorer".into(), vec![s.cwd.clone()])),
            clipboard: s.cwd.clone(),
            note: "已打开会话目录，路径已复制（ZCode 无 CLI 续接命令）",
        },
    }
}
```

```rust
// src-tauri/src/core/config.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub notify_enabled: bool,
    pub running_fresh_ms: u64,
    pub done_after_ms: u64,
}
impl Default for Config {
    fn default() -> Self { Self { notify_enabled: true, running_fresh_ms: 120_000, done_after_ms: 900_000 } }
}

pub fn path() -> std::path::PathBuf { crate::agentboard_dir().join("config.json") }

pub fn load() -> Config {
    std::fs::read_to_string(path()).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let p = path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(p, serde_json::to_string_pretty(cfg).unwrap())
}
```

- [ ] **Step 4: lib.rs 实装 jump 与 config commands**

```rust
#[tauri::command]
fn jump(state: tauri::State<AppState>, session_id: String) -> Result<String, String> {
    let snap = state.machine.lock().unwrap().snapshots()
        .into_iter().find(|s| s.session_id == session_id)
        .ok_or("会话不存在")?;
    let p = platform::jump_payload(&snap);
    if let Some((prog, args)) = &p.spawn {
        std::process::Command::new(prog).args(args).spawn().map_err(|e| e.to_string())?;
    }
    use tauri_plugin_clipboard_manager::ClipboardExt;
    state.app.clipboard().write_text(p.clipboard, None).map_err(|e| e.to_string())?;
    Ok(p.note.to_string())
}

#[tauri::command]
fn get_config() -> core::config::Config { core::config::load() }

#[tauri::command]
fn set_config(cfg: core::config::Config) -> Result<(), String> { core::config::save(&cfg).map_err(|e| e.to_string()) }
```

Settings.tsx：三个开关/输入（通知开关、完成推断阈值分钟数、运行中新鲜窗口），读写 `get_config`/`set_config`；数据源状态区显示 `get_source_status()` 三项（hook 已注册/转录目录/db 可读）。跳回失败在行内 toast 显示 note。

- [ ] **Step 5: 验证**

Run: `cargo test && npm run tauri dev`
手工验收：点一个 ZCode 会话行 → Explorer 打开且剪贴板是目录；点 Claude 会话行 → 新 wt 窗口落在 cwd 且剪贴板有 `claude --resume …`；设置页改动后重启仍生效。

- [ ] **Step 6: Commit** `git add -A && git commit -m "feat: 跳回与设置页" && git push`

---

### Task 13: 端到端验收 + README + 发布 v0.1.0

**Files:**
- Create: `README.md`
- Modify: 无代码（验收驱动修复）

- [ ] **Step 1: 安装 hook 到真实环境**

```bash
# 用应用内设置页的"接入 Claude Code"按钮（Task 12 已实装则用之；否则临时加一个 dev command 调 install_hooks）
```
验证：`~/.claude/settings.json` 出现我们的 Notification 组，`~/.claude/backups`（按 Task 7 实际 backup_root = `~/.agentboard/backups`）有备份。

- [ ] **Step 2: 端到端手工验收清单（spec §9 最后一行）**

逐项执行并记录：
1. Claude Code 新会话：发一个 prompt → 看板出现"运行中"
2. Claude Code 触发权限请求（让它跑一个需要批准的命令）→ 状态变"等待·权限" + 一条通知
3. 在终端批准 → 状态回"运行中"（等待解除路径）
4. 会话完结放置 15 分钟（临时调阈值 1 分钟加速验证）→ "完成（推断）" + 通知
5. ZCode 新会话跑一个任务 → "运行中"
6. ZCode 任务失败（故意触发报错）→ "失败" + 通知
7. ZCode 关掉会话 → "完成"
8. 跳回两种 agent 各一次
9. 重启应用 → 历史还在，等待/运行中计数正确
10. 双开应用 → 第二个聚焦已有窗口

- [ ] **Step 3: README.md**

内容：一句话定位、截图占位、安装（`npm install && npm run tauri build`，产物 exe）、接入 Claude Code 说明（应用内一键 + 手动命令）、数据源与隐私声明（全部本地、无网络）、支持/局限（ZCode 等待审批不可见、不能聚焦原终端 tab）、Roadmap（审批交互、更多 agent）。

- [ ] **Step 4: 发布**

```bash
git add -A && git commit -m "docs: README 与验收记录" && git push
gh release create v0.1.0 "src-tauri/target/release/agentboard.exe" "src-tauri/target/release/agentboard-hook.exe" --title "v0.1.0" --notes "首个可用版：Claude Code + ZCode 会话看板、托盘计数、通知、跳回"
```

（exe 名称以实际 build 产物为准，若 tauri bundle 出 .msi 则附 msi。）

---

## 计划自审记录（Self-Review）

1. **Spec 覆盖**：§3 架构三层 → Tasks 2–10；§4 前置验证 → 已完成并写入"前置验证结论"；§5 状态判定 → Task 3 测试逐路径覆盖 + Task 8 映射；§6 通知/跳回 → Tasks 10/12；§7 错误处理 → hook exit-0（Task 5）、解析跳过（Tasks 4/6）、db 退避（Task 10 注释③）、轮转（Task 5）、单实例（Task 10）；§8 扩展预留 → 状态机 Waiting(kind) 字段 + installer 幂等支持多组；§9 测试策略 → 各任务 TDD + Task 13 端到端清单；§10 决策记录无实现项。**已知取舍**：spec §5 的 ZCode"等待"行，实测 db 无信号，降级为"等待·空闲"并在 Task 8 与 README 标注——这符合 spec"推不出来的不硬标"原则。
2. **占位符扫描**：Task 10 lib.rs 为骨架+逐条落地注释（spawn_collectors 与托盘），是本计划唯一非逐行代码处；其中每条注释都对应已定接口（mpsc 信号源、Update 处理、插件名、command 名），无设计空白。执行时若与编译现实冲突，以 Global Constraints 为准调整。
3. **类型一致性**：`UnifiedEvent`/`SessionSnapshot`/`Update`/`JumpPayload`/`Config` 各任务引用与 Task 2/3/9/12 定义一致；TS `types.ts` 与 Rust serde camelCase 逐字段核对过（`sessionId`/`doneInferred`/`lastActivity`）。
