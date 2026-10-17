# 0.5.8：Clash 默认控制通道与协作入口修复

日期：2026-10-17。发布状态以 GitHub Release/PR 为准。本记录不表示维护者生产网关已升级或 Clash 已激活规则。

## 问题与修复

维护者现场只读调查确认：运行程序为 0.5.7，profile 仍开启 AEC 系统接管，Windows 当前却指向 Clash 的 `127.0.0.1:7897`；当前 Clash Rule 配置没有 `AEC-MANAGED-FIXED`。CVR 2.5.8 默认使用 named pipe、TCP controller 为空，旧适配器无法读取。这能解释普通网站恢复后 Claude 仍走普通节点，不能仅归因于 ISP 隧道断开。

0.5.8 新增明确的 `clientCooperation` 工作方式，阻止 AEC 在协作模式下写系统代理；与旧接管选项互斥。支持已有 named pipe，不开启 TCP、不猜 secret、不自动重载。G7 只读观察真实入口和当前有序规则，变化后撤销就绪，重开开关后重新读取。导出/安装/验证只取明确选定站点，普通 GPT、Arena 和独立指纹出口不自动接管。看板/托盘隐藏协作模式无关的 AEC 普通上游切换。

## 生产切换步骤与影响

1. 备份当前安装和 data，明确停服授权后通过完整更新安装核心；UI-only 不足以增加此功能。
2. 日常状态在出口设置选择“站点协作”，保留原 ISP 精确 IP，不勾选 AEC 系统接管。核对站点列表；旧 GPT 项不自动删除，由用户明确选择所需范围。
3. 看板预览并确认安装受管 footer。仅标准 CVR 2.5.8、独占默认脚本可自动安装；自定义/共享脚本或 DNS 覆盖等拒绝，不强行改写。
4. 由用户在 Clash 官方 UI 激活配置，可能短暂中断连接。Clash 保持系统代理开启、规则模式，普通规则及节点选择保留。
5. AEC 进入 Agent 模式与打开 Clash 系统开关的先后不限；运行核对需看到当前受管规则。G7、内部 ISP 身份与 DNS 证据分别查看，不把程序已更新等同于已保护。

关闭 Clash 系统代理会让日常浏览器绕过其规则；Google 等需要代理的站点可能不通，Claude 也不再受此入口保护，AEC 不会抢回。重新开启后需要新规则证据。普通 IP 检测网页遵循原规则，可能显示普通节点 IP，不代表 Claude 的实际出口。关闭入口、Global/Direct、脚本引擎失败、裸 IP、旧连接或独立代理不属于绝对阻断保证；未实现管理员级整机锁定。

## [已验证] 本地隔离验收

- `cargo fmt --all --check`：通过。
- `cargo clippy --all-targets -- -D warnings`：通过。
- `cargo test --quiet -- --include-ignored`：292 项通过，0 失败，0 忽略；包括 pinned mihomo 隔离真实链路，无生产系统写入。
- `cargo test -p aec-core --test local_upstreams cooperation_ -- --nocapture`：先捕获域名范围被自动扩大，两项失败；修复后两项通过。
- `cooperation_rechecks_rules_after_system_proxy_is_reenabled`：先关后开、先运行后打开、关期间规则失效后重开均取得新证据；规则匹配仍不伪造端点/DNS pass。
- `default_windows_pipe_controller_reads_rules_with_tcp_controller_disabled`：真实隔离 named pipe 能读 `/version` 和规则，Claude 命中固定 mock、Google 命中普通 mock；错误入口/bypass/配置变化拒绝旧证据。
- `node packaging/test-ui.cjs`：实际已安装 Edge 与隔离后端通过；工作方式互斥、保存、协作状态同步、上游隐藏、导航/CRUD/确认/取消/热更等回归。修正了等待文案子串造成的测试误匹配。
- `node packaging/test-cooperation.cjs`：保留用户规则/组/节点、幂等、冲突/环路/非 Rule 处理通过。
- `node packaging/test-browser-environment.cjs`：实际 Edge 的远程域名代理、中文语言、正向 UDP STUN 对照及限制后无非代理 STUN 通过；不改真实浏览器策略或用户 profile。
- `cargo run --quiet -p aec-core --example cooperation_probe`：现场只读核对，返回 `COOP_RUNTIME_NODE_MISSING` / G7 fail；未安装/激活/重载当前 Clash。
- 安装版 `aec-agent.exe diagnose`：现场只读返回 version 0.5.7、mode agent_unverified、networkReadiness unknown；不是生产升级后的验证。

## [推断 / 待现场验收]

维护者升级后的真实订阅安装/激活、Claude 新连接经原 ISP、其他网站保持原节点、日常浏览器 DNS/WebRTC/ECS 全范围证据及供应商稳定性尚未验收。隔离 mock 不代表真实 ISP。当前内核 API 不回显 HTTP 节点地址和 DNS policy，整体 `runtimeVerified` 保持 false；不能凭规则存在显示整机已保护。

## 回滚

正常停止本版本后，保留/恢复更新器备份和原 data。Clash 持久规则需要单独撤销自己的未变化 footer，再由用户激活；撤销会取消该块保护，先说明影响。外部已改脚本不覆盖。不要为了回滚停止或修改第三方客户端、指纹浏览器或节点；不要把 AEC 系统接管与 Clash 系统开关同时当作入口所有者。
