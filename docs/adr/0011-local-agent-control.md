# ADR-0011：托盘管理器通过本机接口向 Agent 暴露控制能力

- 状态：接受
- 日期：2026-10-13

## 背景

用户需要让桌面 Agent 帮忙配置本机 AI 流量分流，同时保持非技术用户的主要入口是 AEC 看板。单独 HTTP API 难集成；单独 MCP 又限制自定义 Agent host。HTTP 浏览器设置页已有一个串行 Backend，但其页面 query token 与 Origin 保护面向浏览器，不能重用为 Agent 凭据。

## 决策

1. 托盘提供 loopback-only、可从看板显式启用/关闭的本机 HTTP/JSON 控制接口。
2. 发布 aec-agent.exe 作为 MCP stdio adapter 和通用 JSON host bridge。MCP 和 HTTP 共用工具名、严格 JSON schema、枚举及安全语义；所有更改最终进入现有托盘串行 Worker。
3. MCP 初始化按官方 JSON-RPC stdio 生命周期运行。HTTP 使用临时 Bearer token；仅随机 credential reference 写入发现文件，token 写当前用户 Windows Credential Manager。每次托盘启动轮换 token，退出时清发现文件与 token。
4. 只暴露状态、表单配置、规则、路径、模式、自启和快捷方式等有限操作。不提供通用 shell、文件、URL、注册表、任意进程或裸 profile 写接口。密码始终从用户本机设置页输入。
5. 认证成功表示本机同用户进程获准访问，不能被解释为 MCP 客户端本身获得系统级隔离。接口拒绝浏览器 Origin、CORS 与非 loopback Host；不接受未知字段。Agent 输出可能含本机出口、站点和路由信息，因此不应自动上传。
6. 固定 ISP 身份和全局写入接管门仍由原状态机强制执行。Agent 不能把未知证据报告为通过，也不能因切换而隐式改变出口身份；安全边界修改须由用户确认。

## 后果

- 桌面 MCP 客户端可直接使用发布包，不需要用户安装语言运行时或 SDK；支持不同 MCP 配置格式的客户端仍需用户按其文档注册。
- 纯云端 Agent 无法直接访问本机 stdio/loopback，仍需用户提供可信的本机执行 host。
- 用户要保持托盘运行；托盘停止会结束 HTTP 接口，关闭 Agent 开关会立即拒绝新请求。
- Credential Manager 与发现文件需进行安装/卸载清理；本实现使用进程生命周期令牌，退出清理并对发现信息作身份匹配。
- 新增测试必须覆盖协议 framing、认证拒绝、关闭开关、未知字段、无密码输出和固定出口路径不变。

## 被否决的方案

- 只支持 MCP：拒绝，无法支持用户自建本机 Agent host 和非 MCP 集成。
- 只支持 HTTP：拒绝，每个 Agent 都需要额外 client-side HTTP 适配，不是可直接注册的通用工具接口。
- 将 browser query token 或仅 Origin 检查重用作 Agent 鉴权：拒绝，浏览器凭据会进入 URL/历史记录，并受 CSRF/DNS rebinding 面影响。
- 直接让 Agent 修改 JSON/YAML 档案或运行任意 shell：拒绝，会绕过 Credential Manager、全局接管门、固定出口状态机、验证和 serial worker。
- 远程/局域网监听或云端中继：拒绝，超出本机产品边界并增加凭据/出口资料的网络暴露面。

## 验收

- 发布包含 aec-agent.exe、看板入口、Agent 集成指南及不含用户秘密的示例配置。
- HTTP 仅接受有效 bearer + 精确 loopback Host + 无 Origin；未授权、关闭接口、未知字段、过大请求失败关闭。
- MCP initialize、notifications/initialized、tools/list、tools/call 和 stdio framing 按选定官方协议工作；stdout 只写 JSON-RPC。
- 所有修改复用 Worker 并返回执行后状态；固定 ISP 保护、未知失败关闭、原全局状态接管门测试保持通过。
- README、用户指南、规格、变更日志和便携包清单保持同步。
