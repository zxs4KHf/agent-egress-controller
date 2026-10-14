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
| aec_enter / aec_exit / aec_recheck | 进入、恢复或重新检查 | 是 |
| aec_startup / aec_shortcuts | 管理登录自启和快捷方式 | 是 |

所有字段在本机按严格 schema 校验，未声明字段和任意命令都拒绝。成功表示操作已执行，不代表 Agent 模式通过检查。之后读取 aec_state；只有实际证据确认时才报告就绪。连接超时的结果可能未知，先读取状态，不要盲目重试有副作用的操作。

## HTTP/JSON

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
- 系统代理只有一个可靠写入者。Clash 等客户端的系统代理开关与全局 VPN/TUN 不受支持；可以保留其本机内核端口作为普通流量上游、关闭它的系统代理开关，然后从其客户端切换节点。冲突解释见看板「代理冲突说明」。
- 用户逐项在看板批准规则/出口/自启等变更。Agent 工具调用并不代表用户批准新的目标站点、凭据、出口身份或权限。

## 本机命令行

发布包附带 aec-agent.exe：

- aec-agent.exe config 输出当前可执行文件和数据目录的 MCP 配置，不包含 token。不同客户端可能要将 server object 嵌入不同位置。
- aec-agent.exe call 从 stdin 读取一个 JSON {"name":...,"arguments":...} 请求，输出单个 JSON 响应。不要通过命令行参数或环境变量传 token、密码。
- MCP host 启动 aec-agent.exe --data-dir <本安装数据目录> mcp，stdin/stdout 专用于 MCP JSON-RPC；错误仅写 stderr。

## 官方协议参考

- [MCP lifecycle, 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
- [MCP stdio transport, 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [MCP server tools, 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
