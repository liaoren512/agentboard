# Pitwall collector 参考件（MIT）

来源：Pitwall.app (v0.x) Contents/Resources/collector，作者 JasonHZS（github.com/JasonHZS/pitwall-collector）。
原仓库以 MIT 许可发布 collector 部分；本目录仅作 agentboard 实现参考，保留原文件内容未改写。
Pitwall 应用本体（MacOS/Pitwall 二进制、图标、SwiftUI 界面）为专有软件，未复制、不模仿其资产。

关键参考点：
- pitwall-hook：统一事件 schema（v:1）+ 各 agent payload 归一化 + 审批撤回协议（指纹去重）
- pitwall-hook-zcode：ZCode 桌面版 hook 桥（父进程链校验 dev.zcode.app、PermissionRequest 查 db 校验根交互会话、原生弹窗竞速语义）
- pitwall-approval：审批应答协议（文件队列 60s 窗口）
- pitwall-events：事件文件 CLI（tail/status/summary）
