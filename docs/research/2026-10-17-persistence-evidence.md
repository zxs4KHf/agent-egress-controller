# Clash 持久扩展、运行核对与 DNS/ECS 证据契约

日期：2026-10-17（America/New_York）。本文件是只读资料研究，不是生产部署或外网检测通过报告。没有访问维护者的 Clash/AEC 控制器，没有修改客户端配置、系统代理或指纹浏览器，没有发起 DNS 检测查询、WebSocket 检测会话、reload、停服或升级。

## 结论

**[源码已核对 / 实现建议]** 可以实现一个范围很窄的 Clash Verge Rev **v2.5.8** 持久适配器：核实当前订阅已经引用的独立脚本文件，仅在该文件尾部追加 AEC 自有的可恢复包装块，不修改 `profiles.yaml`、订阅原文、代理组或节点选择。默认新建远程订阅已经拥有这样的脚本，但历史/损坏/共享引用不能假定成立，必须拒绝或转为官方 UI 引导。持久文件、运行规则、真实路径和 DNS 证据是不同状态，不能合并为一个“已保护”。

**[重要事实]** CVR v2.5.8 脚本抛异常或超时会保留输入配置继续处理，不会拒绝整份配置；客户端 GUI 接管的 DNS 字段又可能在脚本之后覆盖局部 DNS policy。因此脚本自身 `throw` 不是系统级 fail-closed。不能以“文件安装成功”承诺 Claude 在客户端关系统代理、切 Global/Direct、换订阅或脚本失效后仍不可绕过。[脚本失败处理](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/enhance/script.rs#L18)、[最终字段回写](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/enhance/mod.rs#L324)

**[证据边界]** mihomo v1.19.31 `GET /configs` **不回显 DNS 配置**，`GET /proxies` 的 HTTP 节点也**不回显 server/port**。运行读回可以证明规则列表和节点类型存在，但不能单靠这些端点证明 DNS policy 或最终 ISP 身份。公开的 `dnscheck.tools` 可提供带随机查询名的权威端 TXT 回证；不应把 DoH 响应没有 OPT/ECS 当成权威端没有收到 ECS。

## 1. v2.5.8 的持久层与最小 schema

**[源码事实]** `profiles.yaml` 顶层是 `current` 与 `items`。一个远程/本地订阅条目通过 `option.script` 引用另一个 `type: script` 条目的 UID，该脚本条目 `file` 指向 profiles 目录内 JS 文件。其他条目可能含订阅 URL、流量信息、节点选择或用户字段，不能输出整份文件到日志或重写成最小 schema。[IProfiles](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/config/profiles.rs#L46)、[PrfItem / PrfOption](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/config/prfitem.rs#L27)

以下只是合成的读取关系，**不是可覆盖真实文件的模板**：

```yaml
current: R-example
items:
  - uid: R-example
    type: remote
    name: Synthetic subscription
    file: R-example.yaml
    option:
      script: s-example
  - uid: s-example
    type: script
    file: s-example.js
```

**[源码事实]** `from_url()` 在缺少 script 引用时创建并注册独立脚本，再将 UID 放入订阅 option；默认内容是 `function main(config, profileName) { return config; }`。因此“已有脚本末尾包装”不必新建 profile 条目。正常远程更新保留/合并旧 option；不能据此保证用户手工删除、共享脚本或客户端升级后仍正确。[远程订阅初始化](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/config/prfitem.rs#L348)、[模板](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/utils/tmpl.rs#L19)、[update_item](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/config/profiles.rs#L260)

准确 `ITEM_SCRIPT` 内容（文件末尾有换行）：

```javascript
// Define main function (script entry)

function main(config, profileName) {
  return config;
}
```

**[本次最小兼容范围建议]** 没有可靠 JS 解析器/客户端同款解释器时，自动安装只接受可证明为上述默认 identity `main` 的脚本（可支持明确测试过的换行/BOM差异），仍保留原字节并追加 footer。已有自定义 JS 一律保留，不尝试用正则猜测其作用域或执行任意用户源码；给出“当前自定义脚本暂不支持自动组合”的可查原因和手动导出入口。支持任意已有脚本的组合属于后续能力，不是本次“安全持久安装”默认承诺。

**[实现建议]** 自动写入的最小资格条件：

- 客户端版本和实际根目录已确认；未知版本仅预览/导出。
- `current`、目标 UID、脚本 UID 均唯一；目标仅为当前 remote/local；脚本为非全局、专属该订阅的 `type: script`，不被其他订阅引用。
- 文件名为受支持的安全相对 JS 文件；拒绝绝对路径、目录穿越、重解析点/符号链接、逃出已确认 profiles 目录，以及非普通文件。
- 以字节保存原脚本摘要和恢复数据；不打印原脚本。不得用字符串搜索猜测并改写任意函数体。
- 仅支持能被可靠验证的可写 `main` 绑定。footer 捕获原函数后调用原函数，保留 `this`、`config`、`profileName` 和原返回语义，再在返回的合法配置上执行 AEC 变换。`const main`、不能确认入口、命名冲突、异步/异常/无效返回等情形先拒绝。
- 只追加/替换 AEC 精确所有权 footer；更新/卸载验证用户原字节未变和当前 footer 完整。用户编辑导致摘要冲突时不盲目恢复或覆盖。
- 写入前重新检查 current/引用/文件摘要，原子替换并保留恢复记录；不写 `profiles.yaml`、订阅 URL、订阅 YAML、全局脚本或指纹项目。

**[源码事实]** enhance 每次从条目读取脚本，再调用 `main(config, profileName)`。它先执行序列扩展、应用字段、内建/TUN/DNS 设置，再全局 merge/script、订阅 merge/script，最后回写应用拥有字段。只改磁盘不会使已经运行的配置立刻生效。[收集脚本与处理顺序](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/enhance/mod.rs#L155)、[脚本调用约定](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/enhance/script.rs#L134)

## 2. DNS GUI 接管的隐藏覆盖

**[源码事实]** `verge.yaml` 的 `profile_dns_settings[uid].enabled` 优先，无该条目才使用 `enable_dns_settings`，缺省 false。条目还可能含 `confirmation`，用于确认订阅原有 DNS 与 GUI 覆盖关系。DNS 文件名是 `dns_config.yaml`。[IVerge](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/config/verge.rs#L78)、[dns_settings_for](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/config/dns.rs#L101)、[文件常量](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/constants.rs#L50)

**[源码事实]** GUI 启用并提供非空 `nameserver-policy` 时，最终 `dns.extend(self.dns)` 按顶层 DNS 字段回写：覆盖的是整个 policy map，不是仅冲突的单个域名。AEC 在最后一个订阅脚本中加 policy 仍可能被丢弃。[DNS 设置合并与最终 enforce](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/enhance/mod.rs#L767)

**[实现建议]** 窄适配器发现该接管启用/无法确认且 GUI policy 非空时，拒绝宣称 DNS 持久安装。不要为几个站点关闭用户全部 GUI DNS 或替换整个表。需要扩大管理范围时另行预览、确认与回滚。客户端可能自动禁用未经确认的 GUI 覆盖，AEC 不应据猜测写其 confirmation。

## 3. 外部入口与激活契约

**[源码事实]** release App ID 为 `io.github.clash-verge-rev.clash-verge-rev`，数据根是 `dirs::data_dir()/APP_ID`，Windows 通常对应 `%APPDATA%/io.github.clash-verge-rev.clash-verge-rev`，profiles 子目录名为 `profiles`。应用设置 `config.yaml` 保存 controller/secret 并供 `IClashTemp::get_client_info()` 提取；最终生成运行文件常量为 `clash-verge.yaml`。`verge.yaml` 中 `enable_external_controller` 缺省 false 时，enhance 会把最终运行 `external-controller` 写空，故 `config.yaml` 里有地址不等于 HTTP 已开启。[目录来源](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/utils/dirs.rs#L10)、[controller 设置来源](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/config/clash.rs#L15)、[最终关闭控制口](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/enhance/mod.rs#L602)、[运行文件常量](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/constants.rs#L50)

**[安全建议]** 目录是候选而非身份凭证。必须交叉核对已确认客户端版本、进程归属及生成运行配置，controller 只接受现有本机 endpoint；禁重定向与系统代理转发。secret 只进请求头和内存，不落 AEC 档案/日志/页面，不尝试默认值，不启用第三方控制器或扩大 CORS。

**[源码事实]** CVR release 确有 loopback singleton HTTP 服务，认证头 `x-instance-token`，路由只有 `/commands/visible`、`/commands/scheme` 和 PAC；开发特性才额外提供 quit。scheme 用于导入订阅，不是现有扩展安装或 enhance 接口。`save_profile_file`、`enhance_profiles` 等是 Tauri 命令，不是已证明外部可调用的管理 HTTP API。[singleton 路由](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/utils/server.rs#L196)、[scheme](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/utils/resolve/scheme.rs#L12)、[profile 命令](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/cmd/profile.rs#L32)

**[源码事实]** mihomo v1.19.31 `PUT /configs` 接受 JSON `{ "payload": "完整 YAML" }`，非空 payload 优先于 path；解析失败返回 400，成功应用后 204，不保存客户端持久文件。`?force=true` 才走常规 HTTP/SOCKS/mixed 监听器强制重建，但无 force 仍会更新自定义 listener、TUN、DNS、代理与规则，暂停新流量并重置解析器连接。[HTTP 契约](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/route/configs.go#L380)、[ApplyConfig](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/executor/executor.go#L78)

**[源码事实]** 内核 `profile.store-selected` 为真时从缓存恢复选择，不能保证所有用户配置都开启它；PUT 不进入 CVR 自身的 selected-node 恢复流程。规则更新也不主动重判所有已建立 TCP 流。因此不应承诺 reload 既完全无中断、又让旧连接立即全部换路径。[内核选择恢复](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/executor/executor.go#L414)、[CVR 选择恢复](https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.8/src-tauri/src/config/profiles.rs#L438)、[内核 rule/proxy 更新](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/tunnel/tunnel.go#L193)

**[实现建议]** 激活优先由用户在官方客户端“重新激活”完成；AEC 可以安装后等待并只读核对。若实现单独的“立即应用”操作，必须预览可能中断并取得确认，使用已有生成 YAML 的完整可信快照加局部块，不能从 `/configs` 拼出完整配置。确认前后代理组 `now/all`、模式、端口、PID/启动时间、订阅 UID、生成文件摘要未被并发修改。组选择保持不可证明、资源路径不明、客户端正在刷新或凭据来源不可信时拒绝。不要周期性 PUT、切换节点、关闭全连接、启用第三方控制口或强杀客户端。

## 4. 运行态读回能证明的内容

| 只读入口 | 可核对内容 | 不能由此证明 |
| --- | --- | --- |
| `GET /version` | 实际 mihomo 版本 | CVR GUI 版本与文件归属 |
| `GET /configs` | Rule/Direct/Global 模式、一般端口、IPv6 等 General | DNS policy、全部原 YAML、HTTP 出站 endpoint |
| `GET /rules` | 有序 type/payload/proxy、index、禁用状态（若有 wrapper） | 已建立流量都重新命中、最终公网身份 |
| `GET /proxies` | 节点存在/类型/UDP 能力、组成员和选择 | HTTP 节点实际 server/port、连接稳定性 |
| `GET /connections` | 当前活跃连接 metadata/chain/rule | 已结束短请求的完整历史；采不到不等于绕行 |
| `GET /dns/query?name=...&type=TXT` | 实际内核 resolver 的 DNS 结果 | 运行中的完整 nameserver-policy 与浏览器是否使用该 resolver |

[configs 实现](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/route/configs.go#L122)、[GetGeneral](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/executor/executor.go#L122)、[rules](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/route/rules.go#L21)、[proxies](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/route/proxies.go#L57)、[HTTP Base JSON](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/adapter/outbound/base.go#L120)、[adapter JSON](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/adapter/adapter.go#L127)、[connections](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/route/connections.go)、[DNS query](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/route/dns.go#L20)

**[实现建议]** 状态分别记录 `persistent_block_present`、`runtime_rules_match`、`runtime_dns_evidence`、`fixed_identity_verified`、`coverage`、时间与原因码。Rule 模式、未禁用的 UDP REJECT 前于域名转发、完整域名集合、HTTP 节点存在是必要但不充分条件。还须将真实诊断请求与 AEC 专用入口的证据关联；没有证据就是 unknown。不要将只看节点名字当 endpoint 已验证。

## 5. ECS 配置不等于 ECS 权威检测

**[源码事实]** 固定内核的参数格式是 DNS URL fragment，以 `&` 分割；带 `=` 的部分作为参数，裸部分作为代理名。例如：

```text
https://1.1.1.1/dns-query#AEC-MANAGED-FIXED&ecs=0.0.0.0/0&ecs-override=true
```

`0.0.0.0/0` 是合法 prefix；wrapper 复制请求后把已有第一份 ECS 的 family/source-netmask/address 改成零长度，或新建 ECS。它**没有删除 ECS/OPT**，不清原 source-scope，且不遍历删除恶意重复 ECS。因此准确文案是“标准请求 ECS 零长度覆盖”，不能写“严格剥除所有 ECS”。[URL 解析](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/config/config.go#L1134)、[wrapper](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/dns/util.go#L227)、[setEdns0Subnet](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/dns/edns0_subnet.go#L7)

**[规范事实]** ECS 是 EDNS 的一个子网选项；零长度的语义不同于没有 OPT，DoH 又只是传输。Google 明确提供零前缀隐私选项，Cloudflare 声明一般不向权威发送 ECS，但 `whoami.ds.akahelp.net` 是其调试例外。[RFC 7871](https://www.rfc-editor.org/rfc/rfc7871.html)、[Google DoH JSON](https://developers.google.com/speed/public-dns/docs/doh/json#edns_client_subnet)、[Cloudflare FAQ](https://developers.cloudflare.com/1.1.1.1/faq/#does-1111-send-edns-client-subnet-header)

**[实现建议]** 隔离测试要发送含非零 ECS 的真实 DNS wire 请求，捕获真正上游收到的 wire 数据，验证零前缀、其它 OPT 保留、失败不退普通 DNS。`/dns/query` 自己构建普通单问题请求，不接受注入 ECS 参数，不能替代这个测试。向递归器主动发送零前缀、但没有权威回证时只能表述配置/本地传输证据，不表述全球权威不泄露。

## 6. 可用的公开 DNS/ECS 回证服务

### 6.1 dnscheck.tools：随机 TXT 与实时权威事件

**[服务方公开契约]** 帮助页支持 `TXT <random>.test.dnscheck.tools`，random 为最多 8 位十六进制，用于 cache-busting，并可关联 `/watch`。选用明确的 `test-alg13` 也可避免默认算法未来变化。[服务方 help](https://dnscheck.tools/help)

**[源码事实]** 服务方公开源 `addr.tools` 在研究时 commit 为 `394800c14932cbb1b0f5dbeb9126c4a43d482335`。TXT 回答直接根据权威端接收到的请求生成：`FROM: <resolver-IP>#port (proto)`、`ID: ...`、有 OPT 时 `EDNS: ...`、有 ECS 时 `ECS: address/prefix`，TTL 为 1。文本可能带注册商/地区后缀，需要严格抽取前面的协议字段，不能把全部未知文本当 IP。[固定源码 TXT 生成](https://github.com/brianshea2/addr.tools/blob/394800c14932cbb1b0f5dbeb9126c4a43d482335/internal/zones/dnscheck/handler.go#L413)、[随机参数解析](https://github.com/brianshea2/addr.tools/blob/394800c14932cbb1b0f5dbeb9126c4a43d482335/internal/zones/dnscheck/options.go#L23)

**[源码事实]** 官网前端连接 `wss://ws.dnscheck.tools/watch/<8hex>`；服务端事件包含 `time/proto/remoteIp/remotePort/qname/qtype`，可能带 `isEdns0/udpSize/clientSubnet`。服务从实际 DNS 请求提取 ECS；默认会话最多 2 分钟，重复 watcher 返回关闭码 4000，队列满可能丢事件。属于公开 best-effort 服务，不是 SLA。[实时事件与超时](https://github.com/brianshea2/addr.tools/blob/394800c14932cbb1b0f5dbeb9126c4a43d482335/internal/zones/dnscheck/websocket.go#L17)、[authority 到 watcher](https://github.com/brianshea2/addr.tools/blob/394800c14932cbb1b0f5dbeb9126c4a43d482335/internal/zones/dnscheck/handler.go#L150)、[官网实际前端](https://dnscheck.tools/main.js)

**[建议]** 用户预览并确认后，经指定被测路径查询全新 nonce 的 TXT；严格核对 qname、类型、时间、Status、截断标记、FROM 和服务已知 schema。需要浏览器证据时使用该浏览器自己的全新随机请求并关联实时事件，不要拿 Agent 发的 DoH 当浏览器 DNS 证据。源 `remoteIp` 是递归器连权威的地址，不一定等于固定 ISP HTTP 出口。权威连接是 IPv6 也不等于浏览器/代理链用了 IPv6。

本机 mihomo DNS JSON 的 `Question` 直接序列化 `miekg/dns.Question`，字段是 `Name/Qtype/Qclass`；`Answer/Authority/Additional` 则是 `{name,type,TTL,data}`。TXT data 是 DNS presentation 格式的引号字符串，可有多个 string chunk，不可按空格草率分割。随机 TXT 的 `ID` 是递归器对权威查询的 ID，不要求等于本机 stub 请求 ID。[mihomo JSON 构造](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/route/dns.go#L39)、[固定内核使用 miekg/dns v1.1.63](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/go.mod)、[Question 类型](https://github.com/miekg/dns/blob/v1.1.63/types.go#L240)、[TXT 类型](https://github.com/miekg/dns/blob/v1.1.63/types.go#L471)

### 6.2 Akamai whoami：有用但有例外

**[服务方事实]** `whoami.ds.akahelp.net`、`.ipv4.`、`.ipv6.` 的 TXT 返回 `ns`，收到 ECS 时另有 `ecs` 和代表性 `ip`；服务按 as-is/best-effort 提供。[Akamai 官方说明](https://www.akamai.com/blog/developers/introducing-new-whoami-tool-dns-resolver-information)

**[建议]** 只做明确标注“调试域名、可能有供应商特例”的可选探针。不要给此固定名字杜撰随机子域契约；不要因 Cloudflare 对这个调试名字发送 ECS，就判所有 Claude 域名都泄露。没有新鲜缓存/查询关联证据时保留 unknown。

### 6.3 RFC DoH wire 与 JSON 是传输工具

**[官方事实]** Google 与 Cloudflare 提供 DoH wire/JSON 请求接口，可以经已配置 HTTP CONNECT 代理访问；`application/dns-message` 能携带真实 DNS 消息。Google JSON 可声明 ECS，Cloudflare JSON 契约不等于标准 DNS wire 的完整 OPT 观察。[Google DoH](https://developers.google.com/speed/public-dns/docs/doh)、[Cloudflare DoH 请求](https://developers.cloudflare.com/1.1.1.1/encryption/dns-over-https/make-api-requests/)

**[限制]** 直接发给某个 DoH 的测试只覆盖该请求路径。响应不带 ECS、缓存剥掉 OPT、或者 JSON API 没有 ECS 字段，均不能证明递归器发给权威时不带 ECS。没有权威回证时结果必须 unknown。不能为取证把所有 `dnscheck.tools` 或其它未选域名永久加入固定站点；额外临时测试域名/策略也必须列入预览范围并可撤销，或者明确把直接固定入口 DoH 结果限定为“固定入口解析器传输”。

## 7. 最小安全落地与验收边界

1. 先实现仅对独立已挂载订阅脚本 footer 的预览/安装/撤销；Fake 文件系统测试覆盖重复、共享、路径逃逸、外部编辑、部分失败/崩溃恢复。实际生产文件不参与测试。
2. 合成已有用户脚本测试原字节/调用约定保持和幂等，模拟 CVR 异常回退；回退原配置不得让状态变绿。
3. 两个隔离 pinned 内核验证 Rule 模式有序规则、UDP REJECT、普通网站/Arena不变，固定门关闭或入口消失时只有选定流量失败。不要扩为指纹浏览器进程规则。
4. 将持久存在、运行读回、真实路径、DNS权威观察、ECS观察分项；未知版本、GUI policy 覆盖、Global/Direct、订阅切换、disabled规则、控制器认证错误分别给可查原因码。
5. 若提供 PUT 激活，隔离测试验证确认门、摘要并发、组选择与端口保持、失败回滚；清楚提示旧连接与新连接的不同。生产 reload/升级仍须用户再次确认。
6. 真实本地 DNS wire 捕获验证非零 ECS→零长度覆盖以及 DoH失败无 fallback；服务方 TXT/WebSocket fixture 验证匹配nonce、旧事件、截断、多chunk、错误、缺证据、非零/零/缺失ECS；外网探针单独 opt-in。
7. 用户当前要求“不动本机网关、不影响指纹浏览器”优先。隔离自测通过不等于生产已安装、已重载或现场DNS全通过；缺现场证据不阻塞诚实提供可用检测入口，但不得发布虚假 pass。

本研究只修改这一份资料文件。官方源码已阅读；涉及具体适配器可实施性的判断均标为建议，尚需上述隔离测试。
