# 0.5.9：系统代理外部改写的红色冲突提示

日期：2026-10-17。对应 PR #15，公开发布状态、合并提交与 CI 证据以 [v0.5.9 Release](https://github.com/zxs4KHf/agent-egress-controller/releases/tag/v0.5.9) 为准。

## 发布资产与老用户升级

- Windows x64 安装包：`AgentEgressController-0.5.9-setup.exe`，校验文件 `SHA256SETUP-0.5.9.txt`。
- 完整便携包：`AgentEgressController-0.5.9-portable.zip`，校验文件 `SHA256SUMS-0.5.9.txt`。
- 独立更新器：`aec-update-0.5.9.ps1`，校验文件 `SHA256UPDATER-0.5.9.txt`；字节与完整包内 `aec-update.ps1` 一致。
- 组件清单：`aec-incremental-0.5.9.json`、`SHA256INCREMENTAL-0.5.9.txt`，以及九个 `aec-0.5.9-*.zip` 组件，供托盘更新器按实际文件哈希选择下载。不是二进制字节补丁，不承诺固定节省比例。

0.5.3–0.5.8 从托盘「设置与维护 → 检查完整更新」升级；下载和核心停服分别确认。0.5.1/0.5.2 原更新器可全量升级一次，需当次增量则先校验并替换独立更新脚本；原 0.5.0 必须先引导，更旧版本手动升级。本次仅更新界面不足以安装核心修复。规则、原 ISP、普通上游、密钥和自启设置保留。详见[升级指南](UPDATING.md)。

## 问题与决策

旧实现已观察外部改写并撤销 G6，但托盘和页面只按 AgentUnverified 显示黄色，通知使用黄色警告图标。恢复同一个端口后可自动回绿，无法说明期间的连接是否绕过固定出口。

本次为 AEC 本地接管增加持久 `systemProxyConflict`，旧状态缺字段默认 false。已确认的入口冲突为红色错误，未知读取仍黄色；两种状态不混淆。红色弹窗通过托盘 Effect 在 UI 借用结束后派发，每次冲突会话只提示一次，关闭弹窗不清证据。控制中心状态、看板徽标和恢复步骤同步，固定入口内部验证不能掩盖外部入口失效。

不抢回系统代理、不自动关闭服务，不更改 Clash、隧道、relay、路由表、ISP 身份或指纹浏览器。继续保留客户端协作和 AEC 本地接管两个选项；客户端协作的 G7 仍独立核对。放弃靠自动写回或仅重新检测恢复绿色：会造成争抢或掩盖已发生的绕行。

## 用户恢复步骤

1. 在托盘或看板正常退出 Agent 模式，等待恢复完成。
2. 关闭其他客户端的系统代理开关；保留需要的内核和普通上游端口运行，VPN/TUN 不能额外接管。
3. 重新进入 Agent 模式，重新核对 G6 与原固定 ISP。无需重启 Windows。

如需保留 Clash 系统代理，退出后选择站点协作并安装、激活规则；不要在运行中混用两种所有者。

## 验证与边界

- [已验证] FakePlatform 公共控制器回归先失败：`cargo test -p aec-core an_external_proxy_change_is_reported_without_competing_writes --lib -- --nocapture`，旧实现 `systemProxyConflict` 为 Null，而预期 true；修复后通过。
- [已验证] 冲突不写回、同一会话不重复通知；恢复原端口与 recheck 仍失败；退出再进入重新就绪。主动 recheck 在定时巡检前也识别改写，路由证据清空和状态重新打开均不能丢失冲突；无证据读取失败不误报冲突。
- [已验证] 原生隔离托盘菜单测试：冲突优先于 busy 显示红图标与明确文字，首次 refresh 返回错误弹窗 Effect，重复 refresh 不再弹窗，结束会话后新冲突可以再次提醒。
- [已验证] `node packaging/test-ui.cjs` 使用真实已安装 Edge、隔离后端与重放状态，检查红色状态点、红色徽标、恢复步骤、退出入口及恢复后的隐藏。没有改写生产 Windows 系统代理。
- [推断 / 不作保证] 此提示不是系统级阻断，不保证外部改写后流量仍在 ISP，也无法发现两次轮询之间已恢复的短暂改写；不要把红色警告当成网络保护已生效。真实供应商与浏览器全流程需要各自的路径证据。

- [已验证] `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --quiet` 通过；`cargo test --quiet -- --include-ignored` 共 293 项通过、0 失败，包括 pinned mihomo 隔离链路。
- [已验证] `powershell -NoProfile -ExecutionPolicy Bypass -File packaging/portable/aec-update.ps1 -SelfTest` 通过；`node packaging/test-cooperation.cjs` 与 `node packaging/test-browser-environment.cjs` 通过，没有生产配置写入。

GitHub CI 结果附在 PR。构建与隔离证据不代替生产升级后的验证。

## 升级和回滚

包含核心变更，单独 UI 热更不足。用户授权后正常停止本项目托盘及其拥有的子进程，核对日常状态和空恢复日志后使用更新器事务安装；不强杀、不停止 Clash 或第三方程序。保留 `.aec-update-backup` 与原 profiles/settings，验证安装文件哈希和配置未变化，再恢复运行。

回滚需正常退出本版本，再恢复已验证的安装备份；用户数据和凭据不随程序覆盖。回滚到旧版本会失去红色锁存提醒，不能将旧版自动变绿等同于会话一直未绕行。
