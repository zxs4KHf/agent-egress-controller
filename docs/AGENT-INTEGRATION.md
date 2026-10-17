# AI Agent 接入

AEC 0.4.0 提供两种只面向本机的 Agent 接入方式：MCP stdio（推荐）与 HTTP/JSON。托盘必须运行；接口默认关闭，用户在看板的「Agent 接入与 MCP」页明确开启。关闭后，HTTP 新请求立即拒绝；关闭托盘会关闭 listener、移除本次发现信息并删除本次令牌。

## MCP（推荐）

在托盘菜单打开看板，进入「Agent 接入与 MCP」，开启本机 Agent 控制，复制该页给出的 MCP server 配置并按目标客户端文档注册。配置示例：

~~~json
{
  "mcpServers": {
    "agent-egress-controller": {
      "command": "C:\\Path\\To\\AgentEgressController\\aec-agent.exe",
      "args": [
        "--data-dir",
        "C:\\Path\\To\\AgentEgressController\\data",
        "mcp"
      ]
    }
  }
}
~~~

各桌面 Agent 的配置文件位置与外层格式不同；应按客户端自己的文档注册 mcpServers 条目。纯云端模型不能直接访问本机 MCP。托盘关闭时 stdio bridge 仍可启动，但调用会明确返回本机控制器不可用，启动托盘后重试。

MCP 使用官方 2025-11-25 JSON-RPC 生命周期与 newline-delimited stdio transport。工具列表为：

| Tool | 用途 | 改变状态 |
| --- | --- | --- |
| aec_options | 能力、选项、约束和接口信息 | 否 |
| aec_state | 当前模式、忙闲、固定出口保护、路径、规则与摘要 | 否 |
| aec_settings | 读取表单设置；不返回密码 | 否 |
| aec_configure | 在日常模式 patch 设置；密码只能在本机设置页输入 | 是 |
| aec_rules | 替换受保护网站和有序分流规则 | 是 |
| aec_path | 选路径，断开连接并核对相同的 ISP 出口后才放行 | 是 |
| aec_route | 选择既有组成员；仅按用户明确意图解除或恢复该组域名的固定保护 | 是 |
| aec_update | 打开界面/完整更新器，下载及核心停服需用户本机确认 | 是 |
| aec_update_status | 最近 20 次更新的阶段、失败码、HTTP 状态、下载量及下一步建议 | 否 |
| aec_enter / aec_exit / aec_recheck | 进入、恢复或重新检查 | 是 |
| aec_startup / aec_shortcuts | 管理登录自启和快捷方式 | 是 |

所有字段在本机按严格 schema 校验，未声明字段和任意命令都拒绝。成功表示操作已执行，不代表 Agent 模式通过检查。之后读取 aec_state；只有实际证据确认时才报告就绪。连接超时的结果可能未知，先读取状态，不要盲目重试有副作用的操作。

## 本机代理上游管理

0.5.4 新增下列 MCP 工具，同名 HTTP 调用使用 `/api/v1/call`：

| 工具 | 参数 | 语义 |
| --- | --- | --- |
| `aec_upstreams` | `{}` | 列出名称、地址、端口、协议、当前选择和 bound 状态，不检测或修改 |
| `aec_upstream_save` | `{"name":"Clash","host":"127.0.0.1","port":7890,"protocol":"http"}` | 新增或按稳定名称修改；不替换路由预设，不自动选择新成员 |
| `aec_upstream_select` | `{"name":"Clash"}` | 选择已保存普通上游，绝不取消保护站点 |
| `aec_upstream_delete` | `{"name":"Clash"}` | 删除非当前、未被其他规则直接引用的成员 |
| `aec_upstream_probe` | `{"name":"Clash"}` | 仅检测，不修改；返回 pass/fail/unknown、错误码、检查时间与 HTTP 状态 |

地址枚举为 `127.0.0.1`、`localhost`、`::1`，协议为 `http`、`socks5`；示例端口不代表用户的实际配置。不要使用 AEC 自己的端口、控制口或 DNS 端口。最多 16 个免认证本机端点，名称是不可变标识；不得传密码、远程地址、URL、任意命令或 force。首次接入会保留原唯一被规则引用的普通上游，例如 XxYun。多个旧上游分别被引用时拒绝自动合并，不能擅自抹掉用途差异。

`bound:false` 表示没有规则引用选择组；保存成功不等于流量已使用它。需要用户明确决定哪些普通规则指向「本机代理上游」，受保护站点仍优先固定 ISP。原有 DIRECT 兜底保持，不新增直连回退。运行中保存/选择可能短暂断连，重载失败尝试回存旧配置，固定证据失败继续阻断。操作不关闭或修改 XxYun/Clash。

检测经显式指定的端口访问 `https://www.cloudflare.com/cdn-cgi/trace`，约 8 秒超时，不跟随重定向，不绕过 TLS，不自动选择或回退直连。`UPSTREAM_HTTPS_OK` 只证明测试 URL 可用，不能报告 GPT 对话、Claude ISP 或整个网关已就绪。核心 0.5.4 是必需条件，只有新版界面或 stdio bridge 不足以提供新工具。

## 站点协作与浏览器环境

只有包含本功能的新核心才提供以下工具；UI 热更不能给旧核心增加能力：

0.5.8 的 `aec_configure` 支持 `{"clientCooperation":true}`，仅日常模式可保存；同时关闭 AEC 系统接管选项，不改当前 Windows 系统代理。旧配置缺字段默认 false，不能通过 UI 热更升级能力。`aec_state` 返回 `clientCooperation`、配置字段 `systemProxy` 和 G7 `scopeEvidence`；协作状态也返回 `systemProxyTakeover`。配置字段不等于当前系统入口或保护证据。协作启动不要求先打开 Clash 开关，但日常浏览器要经过 Clash 入口才受规则覆盖。关闭开关是保护失效，重新开启必须重新核对。G7 的 unknown 不能用内部 ISP pass 或历史规则报告覆盖。

设置页向用户并列提供两项：①保留客户端系统代理并安装激活站点规则；②关闭客户端系统代理，由 AEC 本地接管，配置普通上游，无需修改 Clash 规则。第二项需要用户在本机选择接管范围，Agent 不可绕过现有全局接管确认边界。`clientCooperation:false` 只关闭协作，不代表自动启用 AEC 系统代理接管。两项都保留原固定 ISP 和隧道优化；不要通过普通上游选择或反复改系统代理开关来代替所有权选择。

| 工具 | 参数 | 边界 |
| --- | --- | --- |
| `aec_cooperation` | `{}` | 生成只含受保护站点的局部脚本；`status:export_only`，不是安装/激活/验证 |
| `aec_cooperation_state` | `{}` | 持久块是否仍匹配当前站点、canRestore、带时间的最近错误及历史运行/DNS 报告 |
| `aec_cooperation_preview` | `{}` | 五分钟预览，只读核实标准 CVR 2.5.8、唯一注册的默认脚本、DNS 接管、文件摘要 |
| `aec_cooperation_install` | `{"planId":"预览返回值"}` | 确认范围后追加受管尾块，不改注册表/订阅/节点，不自动激活或停服 |
| `aec_cooperation_restore` | `{}` | 精确撤销自己的未变化尾块；外部编辑拒绝覆盖，不自动激活 |
| `aec_cooperation_verify` | `{}` | 只读当前规则前缀、Rule 模式和 HTTP 类型；端点、ISP 身份和 DNS policy 无证据仍 unknown |
| `aec_cooperation_dns_probe` | `{"consent":true}` | 需用户明确同意的外网权威 TXT 诊断，分别检查当前解析器及显式固定入口 Google DoH |
| `aec_browser_environment` | `{}` | 读取设置、`canRestore`、待确认计划、历史 `recentError`；泄露证据缺失仍为 unknown |
| `aec_browser_environment_preview` | `{"scope":"ai_only","language":"en-US","preventWebrtc":true,"disableQuic":true,"remoteDnsOnly":true}` | 生成五分钟确认计划，不应用、不重启；`scope` 还可选 `global`，当前只支持普通 Edge |

不存在 Agent **浏览器环境应用/强制确认**工具。用户在看板查看影响浏览器、排除项、原值与目标后确认，或取消。修改选项会使当前 UI 计划失效；过期、旧 planId、外部策略变化与机器策略冲突均拒绝。全局范围不写 Chrome / Chrome for Testing；如指纹浏览器使用 Edge，不能应用全局范围。不要依据独立 user-data-dir 推断注册表策略隔离。

协作规则安装的授权与浏览器确认门不同：已获用户明确授权选定站点范围后，Agent 可预览/安装精确 planId。自定义脚本、共享/全局脚本、路径逃逸、未知版本、GUI DNS 接管、文件变化均拒绝。只操作标准 CVR 数据目录的已注册脚本，不接受 Agent 提供文件路径、端口、secret 或任意 JS。安装后由用户在 Clash 官方界面手动重新激活；AEC 不提供自动 PUT/重载接口。修改站点后先撤销旧块再预览；取消确认不写文件。

CVR 默认 named pipe 控制通道也可核对，不要为了 AEC 开启 TCP controller。只读取白名单路径，进程或生成配置变化会拒绝旧通道。固定范围只取用户的站点列表；旧列表如含 GPT，不能未经确认删除或声称 GPT 已不受管。升级程序、选择工作方式、安装持久规则和 Clash 激活是四个独立步骤，不能只完成第一步就报告现场已修复。

看板使用 `POST /cooperation?t=<页面令牌>` 与精确 Origin，action 为 state/preview/install（planId）/restore/verify/dns_probe（consent）。Agent 使用同名 MCP/鉴权 HTTP 工具；两者共用串行 Worker。未知字段，包括 state/restore 中的 force，均拒绝。`cooperation-error.json` 保存动作、静态错误码和时间；`cooperation-runtime.json`、`cooperation-dns.json` 保存有作用域/时间的报告。关闭提示仍可查；API 不可用时 Agent 可离线只读这些 JSON，不要上传配置或调用更新来代替排查。

DNS 探针向 `dnscheck.tools` 和 Google 发送随机查询；它不是只读本机操作，未经用户同意不能调用。报告中的 clientResolver 是当前客户端解析器诊断，fixedDoh 是显式固定入口传输，selectedSiteDns/browserDns 仍 unknown。权威递归来源不是 ISP HTTP 出口，ECS 缺失仅该查询通过，不推广为所有站点/浏览器；结果没有凭据或来源 IP 原文。不根据旧历史 pass 宣称当前网络安全。

本机页面使用 `POST /browser-environment?t=<页面令牌>`，要求精确 Origin；接受严格的 `state`、`preview`、`apply`（精确 planId）、`restore`。这不是 Agent Bearer 接口；Agent 只能通过同名工具读取或预览，不能借此声称用户已确认。失败码保留在 `browser-environment-last-error.json`，恢复快照在 `browser-environment-recovery.json`。它们不含代理凭据；仍只用于本机排查，不自动上传。`recentError.historical:true` 是历史事件，不等于当前操作仍失败。

`remoteDnsOnly` 对 AI 专用浏览器设置解析阻断及 loopback 代理例外；全局 Edge 只关闭预测/预解析，不改系统 DNS，更不保证 EDNS/ECS 安全。`preventWebrtc` 约束非代理 UDP，不能把没有 ICE 候选当作无泄露。全局语言策略是浏览器界面语言，不承诺所有网页的 Accept-Language；系统显示语言不改变。

## HTTP/JSON 协议

用于自建本机 Agent host。数据目录下的 agent-api.json 是发现信息：

~~~json
{
  "schemaVersion": 1,
  "port": 45678,
  "credentialRef": "agent-api-<random>",
  "pid": 1234
}
~~~

发现文件不含认证令牌、用户代理凭据或出口密码。认证令牌以 AgentEgressController:<credentialRef> 为名称存放于当前 Windows 用户的 Credential Manager，UTF-16 secret 是该通用凭据的 password 字段。推荐 MCP bridge 代为读取凭据；自建 host 可用 Windows Credential Manager API 取得令牌。

每次调用使用精确的 loopback Host 和 Bearer token，不要发 Origin：

~~~http
POST /api/v1/call HTTP/1.1
Host: 127.0.0.1:45678
Authorization: Bearer <Credential Manager token>
Content-Type: application/json
Content-Length: <UTF-8 body bytes>

{"name":"aec_state","arguments":{}}
~~~

响应形状为 {"ok":true,"result":...} 或 {"ok":false,"error":"..."}。共享工具名、字段 schema、枚举、工具说明及安全约束以 aec_options 为准。接口只绑定 127.0.0.1，不启用 CORS，不接受浏览器 Origin、传输编码或未知字段。

## 安全与固定出口

- 接口只监听 IPv4 loopback，不暴露给局域网。一次托盘进程生成一个高熵令牌；令牌只短暂存在进程内存和当前用户 Credential Manager，HTTP/MCP 配置、发现文件、日志和 stdout 中不含令牌。
- 本设计抵御远程网页及其他非本机来源，不隔离同一 Windows 用户下的恶意进程。不要把本机控制权限授予不可信 Agent，不要把 MCP 工具输出或设置贴到公开对话；设置可能包含出口地址、站点和路由信息。
- Agent 不读取或写入密码；让用户在看板出口设置页填写密码。接口只允许声明的设置、规则和模式操作，不提供任意 shell、文件、URL、注册表或进程命令。
- 不要悄悄改动预期 ISP IP、关闭固定出口保护、扩大 DIRECT 范围或删除受保护网站。需要改变这些安全边界时先征得用户确认。
- 固定出口路径切换允许断连；切换失败、证据未知或 IP 不同必须继续阻断，不可选另一个公网出口冒充成功。
- 系统代理只有一个写入者。站点协作保留 Clash 系统代理，AEC 只管选定域名脚本；旧 AEC 接管兼容模式则需关闭其他客户端系统代理开关。不要混用；TUN/Global/Direct 或绕过当前入口不在域名规则保证内。冲突解释见看板「代理共存说明」。
- 用户逐项在看板批准规则/出口/自启等变更。Agent 工具调用并不代表用户批准新的目标站点、凭据、出口身份或权限。

## 本机命令行

`aec_update` 的参数严格为 `{"uiOnly":true}`（只更新兼容界面资源）或 `{"uiOnly":false}`（检查完整更新）。返回 `launched:true, completed:false, requiresUserConfirmation:true` 只表示本机更新器已打开；下载需用户确认，核心替换还需独立停服确认。接口不提供 `force`、`confirmCore` 或绕过确认的参数。取消不能被报告为成功安装。更新后读取实际服务状态，不能将更新等同于就绪。

发布包附带 aec-agent.exe：

- aec-agent.exe config 输出当前可执行文件和数据目录的 MCP 配置，不包含 token。不同客户端可能要将 server object 嵌入不同位置。
- aec-agent.exe update-status 离线读取数据目录 `update-logs/`，最长十秒；无需托盘运行或开启 Agent API，不发送请求、不重试更新、不停止网络。可先指定 `--data-dir <目录>`，该参数必须在子命令之前。0.5.3 前的 Agent 不支持此命令；可直接读新版更新器生成的 JSON 日志，不运行升级。
- aec-agent.exe call 从 stdin 读取一个 JSON {"name":...,"arguments":...} 请求，输出单个 JSON 响应。不要通过命令行参数或环境变量传 token、密码。
- aec-agent.exe --data-dir <目录> diagnose 最长十秒，只读、不打开窗口、不发外网请求；无实时证据为 unknown，不推导就绪。它不要求开启 Agent API；未开启时只能返回有限的已保存状态。见 [诊断指南](DIAGNOSTICS.md)。
- MCP host 启动 aec-agent.exe --data-dir <本安装数据目录> mcp，stdin/stdout 专用于 MCP JSON-RPC；错误仅写 stderr。

## 快捷路由调用

更新排查时使用 `{"name":"aec_update_status","arguments":{}}`；HTTP 与 MCP schema 相同，拒绝文件路径等额外参数。`sessions[0]` 为最新可读记录，包含事件数组、`plannedBytes`（组件负载预算）与 `downloadedBytes`（成功完成的下载文件字节，含元数据，不计失败请求的未完成部分/HTTP 头）。先看失败事件的 `stage/code/httpStatus/nativeCode/nextAction`，再检查本机恢复状态，不要直接再次调用 `aec_update`。`liveness:not_checked` 表示不探测更新器进程；未终结会话可能还在等确认，也可能已退出，因此 `status:unknown`。不能让 Agent 绕过停服确认。

快捷切换调用 `{"name":"aec_route","arguments":{"group":"用户既有组名","member":"该组既有成员"}}`。先从 `aec_state.switches` 读取合法选择及 `fixed` 标记；不能臆造出站或替换期望 IP。非固定选择会解除组的显式后缀域名保护，必须已获得用户对此范围的明确指示；不同于仍保持同一 IP 的 `aec_path`。保存成功后再次读取 `aec_state`，超时先读状态而非重复写。规则指向单出站时须先由用户确认改成组引用，不能用组切换成功冒充所有规则已经改道。

## 官方协议链接

- [MCP lifecycle, 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
- [MCP stdio transport, 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [MCP server tools, 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
