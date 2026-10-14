# 0.4.0 本机 Agent 接入验收

日期：2026-10-13。此版本在 0.3.2 固定 ISP 恢复、严格阻断和单写入者修复基础上新增本机 Agent MCP/HTTP 双入口与 `aec-agent.exe` 发布桥接。

## 交付内容

- 看板增加 Agent 接入页面、显式访问开关、MCP server 注册配置、copy action、推荐 Agent 提示词和 HTTP/JSON 协议指引。
- 发布包包括托盘、`aec-dev.exe`、`aec-relay.exe`、`aec-agent.exe` 与 Agent 集成文档。
- MCP stdio 与 HTTP 共用一份工具 schema 和操作 contract；mutations 进入托盘单一串行 Worker。无任意 shell、文件、注册表、进程或网络地址执行入口。
- HTTP 随机 Bearer 仅存进程内存和 Windows Credential Manager，发现文件只包含端口、PID 和随机 credential reference。密码不由 Agent 设置表单管理。
- Agent schema 排除预期 ISP 地址、全局代理所有权和接管门开关。Agent 调用路径后仍要同一 ISP 的精确证据才能开放；失败继续阻断。

## 自动化验证

Windows 环境应运行：

~~~text
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test -- --include-ignored
~~~

**[已验证]** `cargo fmt --all --check` 与 `cargo clippy --all-targets -- -D warnings` 通过。`cargo test -- --include-ignored` 通过：core 198、CLI 集成 3、relay 集成 2、tray 7，共 210 项，0 失败、0 忽略。测试包含 pinned mihomo 本地配置、真实 loopback relay 链和固定出口阻断重载，不改真实系统代理或时区。

便携包由 `packaging/build-portable.ps1` 从此版本 commit 构建。zip 的 SHA256 以同目录 `SHA256SUMS-0.4.0.txt` sidecar 为准，GitHub Release 页面是本记录所在 tag `v0.4.0`。

新增测试覆盖 MCP initialize/initialized/list/call、拒绝不支持版本与初始化前调用、stdin 行 framing/大小上限、仅 JSON-RPC stdout、未知工具/字段拒绝、设置枚举与密码/接管边界、HTTP bearer/Host/Origin/关闭开关及 operation response。

## 尚未由自动测试证明

- 未安装所有第三方桌面 Agent 客户端，MCP config 对每个客户端的发现位置/导入体验仍须按其当前文档验证。纯云端 Agent 无法直连本机 MCP/loopback。
- 自动化集成测试未改真实系统代理、Windows 时区或真实 Agent 登录状态。
- 同一个 Windows 用户权限下的恶意本机进程无法由当前架构隔离；不向不可信 Agent 授权。
- 真正的网络可用性仍由目标机器、运营商、ISP 出口与隧道决定。成功提交工具操作不代表环境报告已就绪。

## 回滚

应用功能可通过新的 revert commit 回滚；不改写 `main` 历史。发布包用户从托盘正常退出，保留 `data` 与凭据管理器条目，恢复原安装目录备份中的旧版托盘、relay、mihomo 和 CLI。仅当旧版托盘启动并确认入口链路正常后，再移除新文件。不要同时启用新旧系统代理写入者。
