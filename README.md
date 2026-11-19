# agentboard

Windows 托盘看板：实时聚合本机 Claude Code 与 ZCode 会话状态，谁在等你处理、谁跑完了，一眼看清。

## 功能

- **看板四状态**：每个会话按 运行中 / 等待 / 失败 / 完成 分组展示，等待细分为权限、提问、空闲等
- **Claude 权限即时可见**：Claude Code 权限请求经 hook 实时上报，看板即时显示在等什么命令/文件
- **真实完成事件**：Claude Code 会话完成经 Stop hook 实时确认，附最后回复摘要
- **ZCode hooks 接入**：设置页一键安装 ZCode hooks，任务等待审批即时可见（装后新开任务生效）
- **需处理消除**：等待/失败条目可单条或全部标记已处理，从「需处理」清掉
- **来源健康**：每个数据源三态（正常/离线/无数据）与最后数据时间一目了然
- **托盘计数**：系统托盘菜单常驻显示「等待 N · 运行 N」，点击打开看板
- **桌面通知**：会话转入等待 / 失败 / 完成时弹系统通知（可在设置页开关）
- **历史**：本地留存最近 1000 条会话记录
- **跳回**：点击会话行，复制工作目录/命令到剪贴板并拉起新终端窗口
- **设置**：通知开关（已生效）、运行中判定窗口与静默推断完成阈值（均已接线，保存后实时生效），以及各数据源接入状态

## 安装与构建

前置：[Rust](https://rustup.rs/) 与 Node.js（含 npm）。

```bash
npm install
npm run tauri build
```

产物：`src-tauri/target/release/agentboard.exe` 与 `agentboard-hook.exe`（同目录），安装包在 `src-tauri/target/release/bundle/`。开发调试用 `npm run tauri dev`。

## 接入 Claude Code

在应用设置页点击「接入 Claude Code」一键完成：

- 修改 `~/.claude/settings.json`，添加 Notification / PermissionRequest / Stop 三个 hook，指向同目录的 `agentboard-hook.exe`
- 写入前自动备份原 settings.json（备份存于 `~/.agentboard/backups/`）
- 只添加/替换 agentboard 自己的 hook 条目，不碰其他配置；卸载应用后从备份还原或手动删除该条目即可

## ZCode 会话

默认零配置：启动后只读本地 `~/.zcode/cli/db/db.sqlite`，不写入、不修改。要在看板看到「等待审批」，需在设置页点击「接入 ZCode hooks」一键安装（装后新开任务生效）。

## 隐私

完全本地运行：不联网、无账号、无遥测。所有数据（历史、配置、hook 事件）都留在本机 `~/.agentboard/` 与既有的 Claude Code / ZCode 目录内。

## 已知局限

- 旧版注册的损坏 hook 组会在启动时自动修复
- ZCode 等待审批需在设置页接入 hooks 后才可见（装后新开任务生效），仅靠 db 轮询推不出等待审批
- 跳回为打开新终端窗口并复制工作目录/命令，无法聚焦到原有的终端标签页
- 桌面通知受 Windows 专注模式（勿扰）设置影响
- ZCode db 读不了时（被锁/schema 变化），该来源在设置页标记为离线，会话列表冻结在最后状态（无 rollout 兜底）
- 历史 1000 条当前仅存档（写 history.sqlite），重启应用后不回填看板
- 托盘计数显示在托盘菜单文本（「等待 N · 运行 N」），未做图标角标叠加
- 无日志文件，排查问题需开发模式（`npm run tauri dev`）看终端输出

## Roadmap

- 审批交互：在看板内直接批准/拒绝 agent 请求
- 支持更多 agent 接入
