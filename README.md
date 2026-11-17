# agentboard

Windows 托盘看板：实时聚合本机 Claude Code 与 ZCode 会话状态，谁在等你处理、谁跑完了，一眼看清。

## 功能

- **看板四状态**：每个会话按 运行中 / 等待 / 失败 / 完成 分组展示，等待细分为权限、提问、空闲等
- **托盘计数**：系统托盘菜单常驻显示「等待 N · 运行 N」，点击打开看板
- **桌面通知**：会话转入等待 / 失败 / 完成时弹系统通知（可在设置页开关）
- **历史**：本地留存最近 1000 条会话记录
- **跳回**：点击会话行，复制工作目录/命令到剪贴板并拉起新终端窗口
- **设置**：通知开关、运行中判定窗口、静默推断完成阈值，以及各数据源接入状态

## 安装与构建

前置：[Rust](https://rustup.rs/) 与 Node.js（含 npm）。

```bash
npm install
npm run tauri build
```

产物：`src-tauri/target/release/agentboard.exe` 与 `agentboard-hook.exe`（同目录），安装包在 `src-tauri/target/release/bundle/`。开发调试用 `npm run tauri dev`。

## 接入 Claude Code

在应用设置页点击「接入 Claude Code」一键完成：

- 修改 `~/.claude/settings.json`，添加一个 Notification hook，指向同目录的 `agentboard-hook.exe`
- 写入前自动备份原 settings.json（备份存于 `~/.agentboard/backups/`）
- 只添加/替换 agentboard 自己的 hook 条目，不碰其他配置；卸载应用后从备份还原或手动删除该条目即可

## ZCode 会话

零配置：启动后只读本地 `~/.zcode/cli/db/db.sqlite`，不写入、不修改。

## 隐私

完全本地运行：不联网、无账号、无遥测。所有数据（历史、配置、hook 事件）都留在本机 `~/.agentboard/` 与既有的 Claude Code / ZCode 目录内。

## 已知局限

- ZCode 等待审批时数据库中无对应信号，看板暂不可见
- 跳回通过新终端窗口打开工作目录，无法聚焦到原有的终端标签页
- 桌面通知受 Windows 专注模式（勿扰）设置影响

## Roadmap

- 审批交互：在看板内直接批准/拒绝 agent 请求
- 支持更多 agent 接入
