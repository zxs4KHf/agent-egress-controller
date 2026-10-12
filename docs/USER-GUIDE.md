# 0.2.0 用户交付指南

本产品用于 Windows 10/11 x64 的本机 AI 出口控制，包含托盘、mihomo、Rust relay 和配置命令行。用户需要自己的 HTTP/SOCKS5 出口；远程 AI API 网关本身需要另外部署。

## 下载与首次使用

从 [GitHub Releases](https://github.com/zxs4KHf/agent-egress-controller/releases) 下载 0.2.0 zip 和 SHA256SUMS.txt。PowerShell 用 `Get-FileHash .\AgentEgressController-0.2.0-portable.zip -Algorithm SHA256` 对照校验。完整解压到可写目录，保留 portable.flag、bin 目录、三个应用 exe 和说明文件。

双击 aec-tray.exe，在首次打开的本机设置页填出口、端口、协议和账号。端口可逐项附协议，例如 `12324/socks5`、`12323/http`；同一候选池的地址必须相同，备用端口用于拨号切换，不表示多个出口。

选目标时区、浏览器语言和精确预期 IP（可留空）。系统代理与时区修改默认关闭，按需分别开启。进入后从托盘启动 Edge/Chrome，让第一个本地检查标签页回报证据。未打开浏览器、证据不足或目标不通时保持黄色；全部硬门槛通过后才开始使用需要稳定出口的账号。CLI 从托盘启动，使用子进程代理环境，现有终端不被改写。

## 直接出口与加密中转

直接连不上出口时可开启隧道。IPRoyal 预设来自 [官方 Shadowrocket 指南](https://iproyal.com/integrations/how-to-use-iproyal-proxies-with-shadowrocket/)，包括 tt01–tt25 的两个端口组合与公开共享参数，仅适合官方支持的 IPRoyal 代理。文档核对日期 2026-10-12，不代表节点当前连通。

路径是「本机 → 加密中转 → 用户出口 → 目标站」，中转入口 IP 与最终出口 IP 不同。隧道路径需要出口用户名/密码；仅用本机 IP 白名单的授权可能拒绝中转服务器。支持自定义 AEAD Shadowsocks 与直连回退，UDP 关闭。回退连接同一出口候选，全部失败时不会把 AI 目标偷偷放成 DIRECT；G1/G2 仍核对真实出口。

## 设置页分流

「全部走出口」把公共请求交给固定出口。「分流」让目标/附加域名走出口，其余直连；填本机日常代理则用「日常代理、DIRECT」fallback 组，每 15 秒健康检查。AI 目标规则在国内分流之前。

GEOSITE/GEOIP 和「国内直连」需要用户自行提供可信的 GeoSite.dat、GeoIP.dat 到 bin 目录。便携包不附带未核实来源的数据文件，程序不自动下载；没有时保持关闭，相关规则预检会报缺项。

## 导入已有表

先退出 Agent 模式。源 YAML 可带用户自己的密码，不能公开上传。便携目录 PowerShell：

```powershell
.\aec-dev.exe profile import .\my-routing.yaml --dry-run
.\aec-dev.exe profile verify .\my-routing.yaml
.\aec-dev.exe profile import .\my-routing.yaml
.\aec-dev.exe profile check
```

支持出站字段：name、type（http/socks5/ss）、server、port、username、password、cipher、dialer-proxy、udp: false。SS 必须是支持的 AEAD cipher。组支持 name、type（select/fallback/url-test）、proxies、url、interval、tolerance；fallback/url-test 必须显式配置健康检查。名称与引用必须有效，组和拨号链不能成环。

规则支持 DOMAIN、DOMAIN-SUFFIX、DOMAIN-KEYWORD、IP-CIDR/IP-CIDR6、GEOSITE、GEOIP，最后一条必须是 MATCH；IP 规则可带 no-resolve。块列表中前置注释和尾注保存到 note；flow 列表不关联逐条注释。MATCH 保存为 final，没有对应注释字段。TLS、插件、其他协议、provider、UDP、未知字段/规则拒绝整次导入，不静默删除。

导入只替换 routing，保留出口、预期 IP、时区、检测和入口；不用源 DNS、TUN、监听端口或系统设置。便携命令默认使用同目录 data，开发默认 runtime/dev-data，可用 `--data-dir <目录>` 指定。原档案备份为 `data/profiles/profiles.json.before-import-*`，原 YAML 不变。

无档案时创建默认档案，出口仍为空，需在托盘设置补齐。密码迁入当前用户 Windows 凭据管理器；保存失败清理本次新增凭据，清理失败明确提示。旧导入留下的凭据不会自动删除，用户可在凭据管理器核对后清理。

设置页对导入表默认「保留现有路由数据」，同时保留现有验收目标。选择内置预设会替换整张表。高级用户可在退出后编辑 profiles.json 的规则数据，不能加入明文密码。

## 托盘切换与桌面应用

select 组导入后可从托盘选择成员，仅 switchable: true 会显示选项。切换前撤销旧检测就绪，内核确认后保存 selected 并重检，启动时恢复选择。已有长连接保留原路径，需要应用重连后使用新成员。

桌面应用是否遵循 Windows 系统代理由应用决定。需要时开启接管；接管门要求显式开启、快照可用、该项无外部写入者，阻止时 G6 为未知。全局代理改写只告警并撤销就绪，不自动抢回；处理冲突后退出并重新进入。

### Claude 桌面版接入与核验

无需托盘启动入口，也不改变 Claude 的安装文件、配置、语言或进程归属。0.2.0 已有系统代理接管；下面的 R11 和 `desktop check` 是源码/下一版本的新增功能，0.2.0 发布包尚未包含。

1. 设置页填写自己的 ISP 出口。需要中转时启用隧道；中转服务器不是最终出口。
2. 选择「分流」，保留附加域名 `anthropic.com`、`claude.ai`、`claude.com`、`claudeusercontent.com`。`DOMAIN-SUFFIX,anthropic.com` 覆盖 `api.anthropic.com`；仅有 `www.anthropic.com` 不覆盖它。AI 域名规则应先于 GEOSITE/GEOIP 和 MATCH。导入表不会自动补规则或被覆盖，需要自己检查 Claude 域名命中的组、成员和固定 ISP 节点。
3. 开启「系统代理接管」，进入 Agent 模式，确认 G6 通过。其他代理工具不能同时争抢系统代理；接管门被阻止时按证据处理冲突，本程序不会自动抢回。
4. 正常打开和使用 Claude 桌面版。在有请求或长连接期间点「重新检测」，看 R11 的进程 PID、目标域名、预期出站和实际链。ISP 地址/网络身份用 G1/G2 核对，配置了精确预期 IP 才做严格比对。程序不会替用户发测试消息。

也可在便携目录运行以下只读命令；开发环境改用 `cargo run -p aec-dev --bin aec-dev -- desktop check --json --data-dir <目录>`。必须选择实际托盘使用的 `data`，不是另一个测试目录。

```powershell
.\aec-dev.exe desktop check --json --data-dir .\data
```

命令退出码：R11 通过为 0，警告/未知或读取失败为 1，参数错误为 2。它不会写状态、日志、系统设置，不启动、关闭或重启 Claude。托盘完整检测也显示 R11；R11 是只读抽样，不参与绿灯硬门槛。

| R11 结果/原因 | 意义与处理 |
| --- | --- |
| 通过 | 至少观测到一个 Claude 服务域名连接，当前稳定样本的域名规则和实际出站链一致；仅该样本有证据，不代表所有请求或 ISP 身份已经证明 |
| `no_desktop_traffic` 未知 | 未抓到可归属的服务连接；可能空闲、连接已关闭、走别的代理或忽略系统代理，不能据此判定通过或断网 |
| `owned_core_unavailable` 未知 | 当前目录没有可验证的本应用内核/端口，或连接表不可读取；检查数据目录和 Agent 模式 |
| `desktop_probe_failed` 未知 | Windows TCP 读取失败；不把失败当作通过 |
| `desktop_route_unproven` 未知 | 请求只带 IP、域名不可判定，或前置 Geo/IP 规则需要额外匹配证据 |
| `claude_target_not_proxied` 警告 | 观测到 Claude 服务域名走 DIRECT/REJECT；检查规则顺序和组成员，程序保留自定义表，不自动改路由 |
| `routing_mismatch` 警告 | 当前出站链不符合档案，或长连接还在使用切换前的 select 成员；核对组选择并让应用重新建立连接 |

核验只读 Windows 的 IPv4 TCP 表和本应用内核的 `GET /connections`。按客户端源 IP/端口匹配，并要求两次 TCP 读取的 PID、可执行文件路径和进程创建时间一致，控制口及 mixed-port 仍由本应用记录的内核持有。支持 `%LOCALAPPDATA%\AnthropicClaude\app-<数字版本>\claude.exe` 的 Windows 安装形态；Claude Code、启动器、其他安装形态及无法读取身份的进程均不冒充桌面版证据。不读取对话正文、请求头、Claude 配置或令牌，不使用过期日志推断 PID。

Electron 的 [系统代理模式](https://www.electronjs.org/docs/latest/api/structures/proxy-config)使用操作系统配置，但应用可自行覆盖；[代理切换说明](https://www.electronjs.org/docs/latest/api/session#sessetproxyconfig)也指出旧连接池可能继续使用旧代理。没有 R11 证据时先核对 G6 和应用代理设置，必要时由用户自行退出并重新打开 Claude。Node、MCP 子进程、Cowork VM 等路径需分别验收，不用全局环境变量、TUN 或修改 Claude 文件来强制接管。

退出 Agent 模式会恢复已接管的系统代理并停止本应用内核，Claude 仍由用户管理。原有长连接不保证随代理设置恢复立即切换；继续使用时让应用重连。

## 退出、更新与交付

退出会请求关闭本应用启动且身份一致的浏览器，按快照恢复已实际接管的项，停止本应用内核与 relay。CLI 由用户关闭。恢复冲突保留外部状态，失败变红，提供重试或明确接受现状；崩溃后先恢复未完成会话，再删数据。

升级先退出旧程序并恢复，备份 data，将新 zip 解压到新目录，再复制需要保留的数据。旧模板档案在读取时转换，保存才更新。换用户或机器需重新填写密码，credentialRef 不能替代跨机器密码。

给其他用户交付全新 zip、SHA256SUMS、无私人凭据的路由示例和自己的服务说明。不要交付维护者 data、浏览器 profile、日志、快照、真实出口或带密码 YAML。内核/relay 启动时会使用临时凭据配置，正常退出会清理；崩溃后需先恢复，不能把运行中的 core 目录当作无敏感信息的示例。仓库 `crates/aec-core/tests/fixtures/routing.yaml` 是纯合成示例，不能直接联网。

卸载前退出并完成恢复，再删目录；凭据条目以 AgentEgressController: 开头，可手动清理。排查提示词只复制、不自动发送，分享前自行决定是否遮盖 IP。
