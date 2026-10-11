# ADR-0002：复用 mihomo，使用 Tauri 桌面壳

- 状态：Accepted
- 日期：2026-10-09

## 背景

现有项目已经验证 mihomo、relay 和部分模式切换脚本。重新实现代理、DNS 或 TUN 会扩大风险；同时 PowerShell 托盘不足以承载配置档案、验收报告和恢复状态机。

## 决策

复用 mihomo 作为网络内核，逐步把 relay 和脚本收敛为受控服务命令；使用 Tauri 做 Windows 窗口、托盘和安装包。前端只能调用白名单命令，不能直接执行任意 PowerShell。

## 后果

需要 Rust/Tauri 构建环境、WebView2、签名和安装测试；网络行为继续由成熟内核负责，产品层专注模式、验收、恢复和引导。
