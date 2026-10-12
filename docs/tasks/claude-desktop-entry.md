# Issue #1：Claude 桌面版遵循 ISP 出口与分流

## 目标和范围

2026-10-12 维护者明确确认：**不需要「打开 Claude 桌面版」入口，只需正常使用的 Claude 桌面版遵循 ISP 出口和现有分流规则。** 本文件保留原路径供 [Issue #1](https://github.com/zxs4KHf/agent-egress-controller/issues/1) 的链接使用；标题和范围以本版本为准。

0.2.0 已实现可选 Windows 系统代理接管、按项接管门、G6、退出恢复和外部改写告警。需要桌面应用接入时开启接管，让遵循系统代理的请求进入同一个 mihomo mixed-port、同一张数据路由表。配置用户自己的 ISP 出口，必要时通过 IPRoyal 官方中转连接这个出口，不能把中转 IP 当作最终出口。

不新增 Entry::App、默认入口迁移、Claude 启动/退出管理或代理启动参数；不要求关闭现有 Claude，不修改 Claude 的文件、配置和全局环境变量。已有 Edge/Chrome/CLI 入口行为保持原有约束。不能结束非本应用启动的进程，也不能绕过接管门改系统代理。

## 实现

- 系统接入和分流复用现有 0.2.0 能力，不另建代理链。
- 内置分流的附加域名建议包含 `anthropic.com`、`claude.ai`、`claude.com`、`claudeusercontent.com`，覆盖子域名。AI 规则先于国内规则和 MATCH。导入表保持原顺序/选择，不自动插入规则。
- 增加 R11 只读桌面流量核验。Windows TCP 表读取 → 已验证自有内核 `GET /connections` → 再读 TCP 表；稳定的客户端源 IP/端口、入站端口、PID/创建时间/可执行路径共同归属桌面连接。
- 只读核验检查实际出站链符合路由数据和显式 select 成员；Claude 服务域名走 DIRECT/REJECT 告警。空闲、无归属、已关闭连接、IP-only、无法判定的 Geo/IP 规则、未知安装形态或读取失败均未知。
- R11 显示在托盘完整报告和排查提示词；`aec-dev desktop check [--json] [--data-dir <目录>]` 可独立运行，只读且不更新状态/日志。通过退出码 0，警告/未知为 1，参数错误为 2。
- 至少观测到一个 Claude 服务域名且所有稳定样本可核验才通过。R11 不参与绿灯硬门槛，也不证明所有请求均走代理。实际 ISP 身份和出口 IP 仍结合 G1/G2，精确预期 IP 按档案严格比较。

对应代码：`crates/aec-core/src/desktop.rs`、`windows.rs`、`core/process.rs`、`live.rs`，以及 `aec-dev/src/main.rs`、托盘和排查提示词文字。完整用户步骤见 [USER-GUIDE.md](../USER-GUIDE.md#claude-桌面版接入与核验)。发布的 0.2.0 zip 没有新增 R11/命令；源码和下一版本包含。

## 已验证与待验收

**[已验证]** 本机安装 Claude 2.31226.1，采用 `AnthropicClaude/app-2.31226.1/claude.exe` 的安装形态。仅检查过版本、进程与软件网络代码，没有修改应用。该事实不等于真实消息经本程序 ISP 出口的验收。

**[已验证]** 单元测试覆盖 API 走固定出口与非 AI DIRECT、内核探测/Claude Code 不冒充桌面证据、源端口/入站/源 IP 不符、进程身份复用、已关闭连接、选择切换后的旧连接、IP-only、Geo/IP 不确定和读取失败。接管/恢复/外部冲突继续由 FakePlatform/FakeHost 测试覆盖。

**[已验证]** `desktop_connection_snapshot_correlates_with_native_tcp_identity` 使用 pinned mihomo 和本地假目标，真实关联 `GET /connections` 与 Windows TCP 进程创建身份；测试进程不会被识别为 Claude 桌面版。测试不改全局状态、不访问 Anthropic、不发送真实消息。

**[待验收]** 普通 Windows 10/11 用户机器上，接管门允许系统代理 → G6 通过 → 用户正常操作 Claude → R11 有真实桌面连接且符合规则 → G1/G2 确认配置的 ISP 出口 → 退出读回恢复。不能把本地假目标测试写成真实 Claude 对话实测。遇到旧连接池，用户可以自行重连或退出重开，本程序不代操作。

## 交付检查

```powershell
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test -- --include-ignored
```

真实内核测试需已有 `runtime/bin/aec-mihomo.exe` v1.19.31，SHA256 与 pin 一致；不自动下载。PR 区分 [已验证]、[推断]/[待验收]，CI 通过后交付。回滚只需撤回新增只读核验/文档，档案格式与接管/恢复逻辑没有改变。

**[已验证，2026-10-12]** `cargo fmt --all --check` 和 `cargo clippy --all-targets -- -D warnings` 退出码 0；`cargo test` 共 170 项通过、5 项忽略；`cargo test -- --include-ignored` 共 175 项通过、0 失败。`cargo run -p aec-dev --bin aec-dev -- desktop check --json` 在没有自有内核的当前数据目录返回 `R11: unknown / owned_core_unavailable`、退出码 1，运行前后数据文件 SHA256 和系统代理注册表五项对比均未变化。这是只读命令验收，不是真实 Claude 消息出口验收。

## 被废弃的旧方案及原因

- 专用菜单与 `--proxy-server` 启动：维护者明确不需要启动入口，普通启动应用接入系统代理已经是本次目标。
- Entry::App、默认入口迁移、owned 进程和 WM_CLOSE：本次不管理 Claude 生命周期，不能取得现有进程的关闭权。
- 以健康内核探测/G6 配置就判桌面通过：没有应用来源证据，违反「不伪造通过」。
- 为保证所有子组件而改 Claude 配置、全局代理变量或使用 TUN：超出项目边界；独立 Node/MCP/Cowork 路径必须另行验收。

## 一手来源与边界

- [Electron ProxyConfig](https://www.electronjs.org/docs/latest/api/structures/proxy-config)：system 模式读取操作系统配置；应用的代理配置和命令行参数可改变行为，不能据此断言特定 Claude 版本的每个子组件都遵循系统代理。
- [Electron setProxy](https://www.electronjs.org/docs/latest/api/session#sessetproxyconfig)：代理切换后连接池可能沿用旧代理。
- [mihomo v1.19.31 connections API](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/route/connections.go)、[Metadata](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/constant/metadata.go)、[TrackerInfo](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/tunnel/statistic/tracker.go)：只读当前连接快照、源/入站端口与出站链。无需启用内核的全局进程扫描。

来源核对日期：2026-10-12。Squirrel 形态之外的 Windows Store/MSIX 版本暂不做 R11 归属；不代表它不能走系统代理，只代表本程序没有该形态的身份验收证据。
