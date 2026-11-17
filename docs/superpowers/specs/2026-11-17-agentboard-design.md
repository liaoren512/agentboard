# agentboard 设计文档（spec）

日期：2026-11-17
状态：待用户终审
参考项目：一款 macOS 同类菜单栏看板工具（闭源 App + 开源采集器）

## 1. 背景与目标

并行使用多个 coding agent 时，会话状态分散在各个终端/窗口里：等待审批的会话没人管、悄悄失败的会话没人发现。同类工具已在 macOS 上用"菜单栏看板"解决这个问题。本项目的目标是在 **Windows** 上做一个同类工具：

- 把 **Claude Code** 和 **ZCode** 的会话聚合到一块实时看板
- 托盘常驻，一眼看到「几个在等我、几个在跑」
- 状态变化发 Windows 通知，每种状态只提醒一次
- 保留最近 1000 条任务历史，可按 agent/状态筛选

完全本地运行：数据留在 `~/.agentboard/`，无账号、无网络请求（许可证检查、软件更新都不存在——本项目不发布收费版本）。

### v1 明确不做（非目标）

- **审批交互**（在看板里批准/拒绝权限请求）——后期加，路径已在 §8 预留
- **聚焦回原终端标签页**——Windows Terminal 无公开 tab API，v1 跳回 = 开新终端窗口定位到会话目录
- 更多 agent（Codex、Cursor 等）——架构上采集器是插件式的，后期按需增加
- 中文/英文国际化、多显示器布局等一切锦上添花

## 2. 方案选型（已定）

**方案 A：混合采集**——每个 agent 挑最合适的通道（这正是参考项目的实际做法：Kimi Code、DeepSeek Harness 也是纯读日志零 hook）：

| 候选 | 结论 |
|---|---|
| B：纯读文件零安装 | 否决：推不出"等待中"，看板核心价值减半 |
| C：全 hook 采集 | 否决：ZCode 永远要读文件，混合架构躲不掉，C 的统一性买不到 |
| A'：Claude Code 全套生命周期 hooks | 否决：比 A 多写一批脚本去记转录里现成的信息，v1 功能无增益 |
| **A：转录 + 1 个 hook（Claude Code）；纯读 db（ZCode）** | **采用**：只为"等待中"这个必须的信号付 hook 成本 |

后期加审批是 A 之上的加法（见 §8），不需要重写。

## 3. 总体架构

技术栈：Tauri 2 + Rust 后端 + Vite/React/TypeScript 前端。项目位于 `C:\Apps\lr\workspace_ai\agentboard\`。

```
┌─ 呈现层 ─────────────────────────────┐
│  托盘图标(计数) · 看板窗口 · Windows通知 │
└──────────────┬───────────────────────┘
               │ Tauri event / command
┌─ 状态层（Rust core）──────────────────┐
│  会话状态机 · 排序/筛选 · 历史存储(1000条)│
└──────────────┬───────────────────────┘
               │ 统一会话事件
┌─ 采集层（三个独立采集器）───────────────┐
│ ① claude-hooks   ② claude-transcripts │
│ ③ zcode-records                        │
└────────────────────────────────────────┘
```

### 3.1 采集层（三个独立模块，输出统一会话事件）

| 采集器 | 职责 | 机制 |
|---|---|---|
| ① claude-hooks | 拿"等待中"信号 | 安装器把随应用分发的 `agentboard-hook.exe` 注册进 `~/.claude/settings.json` 的 Notification hook（装前备份、可卸载、幂等）；hook 读 stdin JSON，往 `~/.agentboard/events/claude-code.jsonl` 追加一行事件，无网络 |
| ② claude-transcripts | 生命周期：新会话/运行中/完成推断/失败推断 | notify crate 监听 `~/.claude/projects/**/*.jsonl`，增量解析（记住每个文件的字节偏移）；**解析器是窄接口独立模块**——转录是非官方格式，格式变了只改这一个模块 |
| ③ zcode-records | ZCode 全部状态 | 轮询（3~5 秒）只读 `~/.zcode/cli/db/db.sqlite`（WAL 模式允许并发读）；**实现第一步先探查 schema** 并固化测试夹具；读不了退回 tail `~/.zcode/cli/rollout/*.jsonl` |

hook 的 stdin 输入含 `session_id`、`transcript_path`、`cwd`、`message` 等字段（以实现时 Claude Code 官方 hooks 文档的最新 schema 为准）。

### 3.2 状态层（Rust core）

- **归并器**：三个采集器的事件按 sessionId 归并（Claude Code：转录文件名 uuid = hook 事件的 `session_id`；ZCode：db 主键，兜底用 rollout 文件名）
- **会话状态机**：每会话独立状态机，见 §5 判定规则
- **历史存储**：状态变化落盘到 `~/.agentboard/history.sqlite`，保留 1000 条
- **派生字段**：会话标题 = 首条用户消息前 80 字符；仓库名 = cwd 目录名；分支 = 解析 `cwd/.git/HEAD`；worktree = 检测 `cwd/.git` 是文件（worktree 指针）

### 3.3 呈现层

- **托盘图标**：叠加两个数字（等待数/运行中数），点击开看板
- **看板窗口**：等待和失败置顶 → 运行中 → 完成按今天/昨天/更早折叠（按本机本地日期分组）；每行显示 agent 图标、标题、仓库/分支、时间、状态
- **通知**：Windows toast，见 §6
- **设置页**：数据源接入状态（hook 装没装、db 读没读到）、通知开关、时间窗口配置

### 3.4 本地数据布局

```
~/.agentboard/
  events/
    claude-code.jsonl        # hook 事件（超 10MB 轮转为 .1）
  history.sqlite             # 会话历史（1000 条）
  backups/                   # 安装器对 settings.json 的备份（时间戳目录，留最近 10 组）
  logs/agentboard.log
  config.json                # 通知开关、运行窗口、完成停滞阈值
```

## 4. 前置验证（实现前必须完成）

1. **环境预检**：Rust 工具链 + MSVC + Node 是否就绪（Tauri 编译必需）
2. **ZCode db schema 探查**：打开 `~/.zcode/cli/db/db.sqlite` 看表结构，确认会话表的 id/标题/状态/时间字段；不满足则启用 rollout 兜底方案
3. **Claude Code hooks schema 确认**：对照官方文档确认 Notification hook 的注册格式与 stdin 字段（本机 `~/.claude/settings.json` 当前无 hooks 字段，属首次安装场景）

## 5. 状态判定规则

五状态：`运行中 / 等待(权限|提问|空闲|其他) / 失败 / 完成`。判定来源按可靠性排序，**推不出来的不硬标**（原则：信号不可靠就不声称）。

| 状态 | Claude Code | ZCode |
|---|---|---|
| 新会话 | 出现新转录文件 | db 出现新会话行 |
| 运行中 | 转录有新鲜写入（默认 2 分钟窗口，可配置） | db 状态为运行中（以实测 schema 为准） |
| 等待·权限 | hook `message` 前缀 "needs your permission" | db 等待/需关注信号 |
| 等待·提问/空闲 | hook `message` 前缀 "waiting for your input" | 同上 |
| 等待·其他 | hook 有事件但 message 不匹配已知前缀 | — |
| 失败 | 转录出现错误标记条目且其后无新写入 | db 失败信号 |
| 完成 | **推断**：最后写入后停滞 15 分钟（可配置）且无等待信号，看板标注"完成（推断）" | db 明确完成状态 |

归并与解除规则：

- 等待中一旦该会话出现新活动（转录写入 / db 状态变化）→ 立即回"运行中"（用户在终端侧已应答）
- 失败标记被该会话任何后续写入清除 → 回"运行中"
- hook 事件是一次性的：每次等待发生都重新触发判定与通知

## 6. 通知与跳回

- **通知**：进入`等待`、`失败`、`完成`各响一次；回到运行中后重新武装。内容只含 **agent 名 + 状态 + 项目名**（cwd 目录名），不含消息/请求内容
- **跳回（v1 尽力而为）**：点会话 → 开新 Windows Terminal 窗口（`wt -d <cwd>`）并填好续接命令（Claude Code：`claude --resume <sessionId>`；ZCode：给出重新打开该会话的命令）。如实标注局限：**做不到聚焦原标签页**

## 7. 错误处理

铁律：**hook 脚本绝不能弄坏 agent 本身**——无网络、单行追加、任何异常静默降级并 `exit 0`，最坏丢一条事件，绝不阻塞 Claude Code。

| 故障场景 | 处理 |
|---|---|
| hook 没装 / Claude Code 未使用 | 采集器报"未接入"，设置页如实显示，不报错 |
| 转录格式变化 | 单条解析失败跳过并计数，连续失败标"源降级"提示；不崩 |
| ZCode db 锁住/schema 变化 | 只读连接 + 退避重试 → 退回 rollout JSONL → 标"源离线" |
| 事件文件无限增长 | 超 10MB 轮转；历史只留 1000 条 |
| 通知发不出（专注模式等） | 非致命，记日志 |
| 应用双开 | 单实例插件：第二次启动聚焦已有窗口 |

## 8. 后期扩展预留（v1 只做设计决策，不写代码）

1. **"等待中"带类型字段**（permission/question/idle/other）——审批功能直接消费，不重构状态机
2. **hook 安装器按"可注册多个 hook"实现**——后期加 `PermissionRequest` hook 只是配置多一项
3. 审批实现路径（照同类工具验证过的模式）：`PermissionRequest` hook 触发 → 请求文件挂 `~/.agentboard/approvals/` 队列 → 应用/通知横幅点批准或拒绝 → hook 写回应答退出；60 秒未应答则退出不置可否，终端原生弹窗照常；先到先答
4. 转录格式真变了 → 生命周期升级为 hooks（安装器现成），升级路径而非重写

## 9. 测试策略

| 层 | 测什么 | 怎么测 |
|---|---|---|
| 状态机 | 状态转换、通知一次性、失败/等待解除 | Rust 单测：fixture 事件序列 → 断言状态路径（运行→等待→运行→完成推断等） |
| 转录解析器 | 增量解析、边界 | 单测：本机真实转录脱敏样本 + 合成边界（空文件、半行写入、未知条目类型） |
| hook 脚本 | stdin→事件行、绝不阻塞 | 单测：喂 fixture JSON 断言输出；喂垃圾也 exit 0 |
| 安装器 | 备份、幂等、卸载干净 | 单测：临时目录装/卸 settings.json，断言备份在、其他字段不动、重复安装结果一致 |
| ZCode 适配器 | schema 探查、只读、容错 | 探查后固化快照夹具再写单测 |
| 端到端 | 真实环境验收 | 手工清单：真实跑 Claude Code + ZCode 各一会话，核对「状态×通知×跳回」 |

## 10. 决策记录

- 2026-11-17 选定方案 A（混合采集），放弃 B（零安装，等待状态推不出）、C/A'（全 hook，ZCode 侧注定读文件，统一性买不到）
- 2026-11-17 v1 范围定为只读监控；审批交互为第一优先级后续功能
- 2026-11-17 应用形态定为 Tauri 托盘应用（放弃 .NET/WinUI、本地 Web 看板）

## v0.1.0 实现偏差注记

以下为本版实现与设计文档的已知偏差，如实记录：

1. **ZCode db 读不了时会话列表静默冻结**：§7 的 rollout JSONL 兜底与「源离线」标注未实现，计划 v0.1.1
2. **历史 1000 条仅存档**：history.sqlite 正常写入并保留最近 1000 条，但重启应用后不回填到看板
3. **托盘叠加数字未做**：§3.3 设计的"托盘图标叠加两个数字"未实现，计数以托盘菜单文本呈现（「等待 N · 运行 N」）
4. **无日志文件**：§3.4 数据布局中的 `logs/agentboard.log` 未实现

## v0.1.1 实现偏差注记

1. **设置页两阈值曾未接线（本版修复）**：running_fresh_ms / done_after_ms 在 v0.1.0 仅存 config.json 不生效；v0.1.1 起 ZCode 运行中判定改用 time_updated 新鲜度（running_fresh_ms 每次 poll 前注入 ZcodePoller）、完成推断阈值 done_after_ms 每次注入 Tick 前注入 Machine，设置页保存后实时生效
