# Pitwall 逆向分析（agentboard 集成参考）

日期：2026-11-17 · 方法：REA bundle 资源盘点 + MIT collector 源码 + 二进制字符串/类型提取（无反汇编 provider 可用，SwiftUI 视觉样式不可提取——本来也不复制）
来源：Pitwall.app（用户本机 Downloads 解压版）。collector/ 为 MIT（已拷贝至本目录 pitwall-collector/，带署名）；应用本体专有——**只学功能与信息架构，不搬资产/代码/文案原文**。

## 一、架构总览（来自 Swift 类型名）

- `Pitwall`（UI 层）+ `PitwallCore`（模型层）双模块
- 视图：CurrentSessionsView（当前会话看板）、HistoryListView/Row/InspectionView（历史）、SessionInspectionView（单会话详情检查）、ApprovalList/ApprovalRow（审批队列）、QuestionEditor/OptionRow（提问表单回答）、AgentSetupRow（接入状态行）、SettingsTabBar、WaitingRow、PanelSessionRow、AgentIcon/AgentIconCache
- **NotchSignalView / NotchHostingView**：MacBook 刘海区信号显示（Agent 完成时"刘海下滑划过一道光"）
- Core 模型：SessionKey、SessionRecord、SessionContext、SessionStateEvidence（**状态证据链**——"推不出来不硬标"的实现载体）、AgentEvent/AgentEventKind（Activity/TaskStarted/TaskEnded/WaitingForUser/SessionClosed/Unresolvable）、TaskPhase（Waiting/Failed/Unresolved/Cancelled）、TaskOutcome、TaskRecord、AgentQuestion(+Option)、AgentInstallation、CollectorRecord(Process/Context)、ClaudeDesktopSessionIndex、**PinnedSessions**、UnknownSessionDismissal、FailedTaskDismissal

## 二、功能清单（字符串实证）

**看板/交互**
- 需要关注分组（Needs Attention）+ 单条/全部消除（Dismiss）；失败/未知会话可 Dismiss（FailedTaskDismissal/UnknownSessionDismissal）
- 置顶（PinnedSessions，Pro 限量："取消几条后可再置顶，或升级 Pro"）
- 未读标记（unseen markers，仅完成结果）
- 单会话检查视图（SessionInspectionView：事件证据、来源健康"· 文件可读 / · 最后新数据 / · 来源需要检查 / · 截至最后确认"）
- 全局热键："Jump to the next agent session from any app"（任意应用间轮转跳会话）
- 历史清理（Clear History…）；复制诊断（Copy Diagnostics）
- 自检机制："Copy the check request, paste it in a new agent session. Pitwall detects it automatically"——复制一段检查请求到任意 agent 会话，自动验证采集链路通

**采集健康与竞品冲突检测**
- 事件文件损坏行跳过、格式版本不支持提示、逐 agent 的"hook 无法读取/transcript 无法读取"告警
- **CC Switch 冲突检测**："CC Switch 切换供应商时覆盖了 ~/.claude/settings.json，Pitwall 的 hooks 不在了；重新接入一次，CC Switch 会把它们存进当前供应商"
- Codex rollout 被删除不再跟踪等细粒度状态

**审批/提问（Pro）**
- 60 秒窗口，超时回退原生流程；通知横幅可直接批准
- 通知内容含命令/工具输入（跟随 Waiting 开关）
- 提问表单交接：多题、单选/多选、自定义答案；答案作为 tool-denial reason 返回给模型；带选项预览的提问留在 ZCode
- Claude/Pi/OpenCode/ZCode 原生弹窗与 Pitwall 并发竞速，先答先赢

**通知/提示**
- 中文文案实例：「N 个会话在等你」「请求批准：」「已退出，未收到任务结果」「报告本轮出错」「需要修复接入」「还没接入 Pitwall」
- 通知点击跳回原会话；聚焦辅助下回退为托盘菜单计数

**跳回**
- 终端线索采集（ctx：ppid/proc/parent_proc/term_program/iterm_session_id/tmux_pane+socket/bundle_id/ghostty_env）
- Terminal/iTerm2/Otty 按 tty 精确切标签页；tmux 单本地客户端时支持；Ghostty 只能开应用；桌面会话按链接/会话 ID 导航（DeepSeek 走本地插件）

## 三、对 agentboard 的映射（采纳 → 改造 → 不做）

**v0.1.1 采纳（只读增强）**
1. ZCode hooks 检测（大项）：Rust 重写 pitwall-hook-zcode 校验桥（进程链 dev.zcode.app → Windows 上改为校验进程名/路径；db 校验根交互会话 task_type='interactive' AND parent_id IS NULL），PermissionRequest → Waiting(permission) + tool_summary
2. 脱敏字段对齐：prompt 首行剥注入块、tool_summary（PermissionRequest 时 command/file_path/url 首行 120 字）、reply_head（Stop 时 160 字压一行）、payload_keys
3. 会话行"在等什么"（tool_summary）+ 完成行回复摘要
4. 来源健康四态显示（设置页：文件可读/最后新数据/需要检查）
5. Needs Attention 消除（单条/全部）

**v0.2 采纳（审批）**
6. 审批队列 + 60s 窗口 + 指纹撤回协议（照 pitwall-approval 设计，Rust 实现）；通知横幅带批准/拒绝按钮
7. 提问表单交接（先 Claude Code AskUserQuestion，后 ZCode）

**v0.2+ 候选**
8. 自检请求机制（复制检查 prompt → 自动验证链路）
9. 全局热键轮转跳会话
10. 置顶会话
11. 日志文件（logs/agentboard.log，终审 I5 一并解决）

**不做**：图标/样式资产复制、刘海交互（Windows 无此硬件语义）、SwiftUI 文案原文照搬（写我们自己的中文文案）。

## 四、Windows 改造要点

- Pitwall collector 依赖 jq/sqlite3/ps（POSIX）；Windows 侧全部收进 agentboard-hook.exe（Rust）：进程链校验用 Windows API（查询父进程映像名是否 ZCode.exe），db 校验复用 rusqlite
- ZCode hooks 注册位置：~/.zcode/cli/config.json（hooks.enabled），与 Pitwall 的 ZCode 集成文档一致
- CC Switch 类冲突检测：设置页每次启动校验我们的 Notification hook 组是否还在
