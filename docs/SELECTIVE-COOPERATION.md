# 选定站点协作与浏览器环境：开发验收记录

日期：2026-10-17。0.5.6 候选的验收记录；公开发布状态以 GitHub Release/PR 为准。本任务不升级或重载维护者生产网关与 Clash，用户最终自行托盘更新。

## 目标与实际状态

保留用户现有系统代理的规则和节点，只将用户选择的站点交给 AEC 原固定 ISP。普通网站、GPT 与 Arena 不自动改道。固定路径不可用或身份不匹配时宁可断连，不换出口。

已实现：局部脚本导出、CVR 2.5.8 默认订阅脚本持久安装/撤销、运行规则只读核对、可选 DNS/ECS 权威探针、固定身份门后的专用入口、选定域名 UDP 拒绝、局部 DoH 与标准 ECS 零长度覆盖、看板与 Agent 入口、浏览器两范围预览/确认/恢复、中性紫色静态图标、托盘分层。

0.5.8 候选补齐独立站点协作运行方式与 G7 入口巡检，支持 CVR 默认 named pipe；完整验收及生产切换见 [0.5.8 记录](RELEASE-0.5.8.md)。协作方式不改 Windows 系统代理，启动顺序不限。普通浏览器仍需 Clash 系统代理开启；关闭后显示未受保护，重新开启后重新核对规则。旧接管配置不静默迁移；站点范围只取明确列表，不自动增删 GPT/Arena/Claude。

明确不支持/仍未知：任意自定义 JS 的自动安全组合（仅手动导出）、自动激活、全部日常浏览器无泄露的现场保证、管理员防绕过组件。运行 API 不回显 HTTP 节点地址或 DNS policy，总 runtimeVerified 不会因规则存在变成 true。当前已实现的是窄范围可恢复安装与分项取证，不是整机绝对锁定。

## 持久安装与运行核对

自动适配限定可核实的标准 CVR 2.5.8、已有 Windows named pipe 或已认证 IPv4 loopback TCP 控制器和当前 remote/local 独占默认脚本。通过 GUI EXE 文件版本、控制通道服务端 PID/创建身份与生成运行配置交叉核对；控制器未开、版本未知则拒绝，不开启或猜 secret。只追加受管 footer，保留默认脚本原字节，不写 profiles.yaml、订阅原文、全局脚本、节点选择。五分钟预览绑定文件摘要与注册关系，写入前重查；外部编辑冲突保留原状。文件上限 256 KiB，超限拒绝，不截断后写回。

恢复记录只保存已证明无凭据的默认模板与 AEC 块，不复制用户完整配置/任意私有脚本。记录先落盘，安装/恢复再原子替换；失败保留记录可查。更改站点时先撤销旧块再预览。并发外部编辑仍需用户避免；摘要前置检查不是第三方进程配合的文件事务锁。

安装不自动激活。用户在 Clash 官方界面重新激活当前配置，可能短暂影响连接，然后点只读运行核对。AEC 不周期 PUT、不改系统代理、不重置节点。规则必须按顺序包含 UDP REJECT 和域名固定转发；Global/Direct、禁用/缺失规则、切换未安装订阅或控制口错误显示未知。

CVR 会在脚本异常/超时后回退原配置，不能把 throw 当阻断。受支持 Rule 输入的冲突返回选定域名 REJECT，保留普通规则；引擎失败/不支持模式仍不能承诺绝对 fail-closed。当前 API 不提供实际端点/DNS 策略回读，规则通过不等于固定身份通过。

## DNS / ECS 分项证据

用户同意后发两次随机 TXT：一次经当前客户端 resolver，一次显式经固定入口 Google DoH。公开服务返回权威观察的 FROM/ID 和可选 ECS；解析器校验实际 mihomo 的 Name/Qtype/Qclass、Google 的 name/type，以及随机名、TTL=1、Status、TC、时间、TXT 转义。没有权威回证、超时、错误或旧名都未知。返回不包含递归来源 IP 原文。

clientResolver 不是选定 Claude DNS policy 的证明；fixedDoh 仅证明这一显式解析器传输。selectedSiteDns/browserDns 保持 unknown，不永久增加检测域名。ECS 非零警告，零前缀或未观察到 ECS 只对此查询通过。固定规则使用标准 ECS 零长度覆盖；恶意重复 ECS 的严格剥除不在承诺内。权威连接 IPv6 不等于浏览器/代理使用 IPv6。

失败码、操作与时间由 aec_cooperation_state 读取，运行与 DNS 历史报告带时间。关闭弹窗不丢证据。详见 [官方资料研究](research/2026-10-17-persistence-evidence.md)。

## 浏览器范围

“仅 AI 浏览器”只影响 AEC 自己的专用目录。确认后新启动参数限制非代理 WebRTC UDP、关闭 QUIC、阻断本地目标解析并仅排除 loopback 代理主机；语言使用用户选择，不改变系统语言或伪造指纹。已运行实例参数不一致时要求用户自己关闭，不静默忽略参数。

“全局浏览器”当前只支持普通 Edge 的全部 profile。Chrome / Chrome for Testing 不写全局策略，不修改独立指纹项目的文件和 profile。独立 profile 不能阻挡同产品全局策略，因此指纹浏览器如使用 Edge，不能使用此范围。预览列明原值、目标、影响与排除项，用户确认后才写入。QUIC、语言等需要用户自行重开 Edge；保存成功不代表运行生效。

两个范围都保存 AEC 自己的专用 Edge / Chrome 启动参数；全局预览也明确列出这部分影响。排除的是日常 Chrome、Chrome for Testing 与第三方指纹实例，不是 AEC 自己管理的专用浏览器目录。

恢复使用精确原值，包括原来不存在的策略。发现外部改写时不覆盖，保留恢复记录。应用中途失败尝试回滚；快照不完整时拒绝静默启动未受保护的专用浏览器。最近错误的动作、错误码和时间保留，Agent 可读取，不依赖弹窗。

全局 Edge 的网络预测关闭不能代替 DNS 隧道。系统时区是独立整机设置，只经既有接管门修改；Windows 显示语言不改。浏览器语言策略不保证网站 Accept-Language。DNS / UDP / WebRTC / IPv6 / ECS 分项缺证据仍为未知。

## 自测命令与证据

[已验证，当前完整版本] `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings` 与 `cargo test --quiet -- --include-ignored` 全部通过：284 项，0 失败，0 忽略。新增 7 项持久安装公共接口测试、2 项权威 DNS 解析测试及 3 项实际内核运行/wire/JSON 测试；以下 272 项为此前基础版本记录。

[已验证] `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --quiet -- --include-ignored` 全部通过：272 项，0 失败，0 忽略，包含本机 pinned mihomo 的真实隔离链路。

[已验证] `cargo test -p aec-core --test browser_environment`：8 项通过，包括确认门、HTTP Origin/令牌、Agent 不可确认、中文原值恢复、过期/旧计划与外部冲突、部分写入回滚、崩溃恢复、错误持久读取和实际启动参数接入。系统写入使用 FakeBrowserHost，没有更改真实注册表策略。

[已验证] `cargo test -p aec-core --test cooperation -- --include-ignored`：实际 pinned mihomo 的选定站点路由、前置 UDP 拒绝、双内核固定入口阻断及 DoH 失败不走普通解析。合成 ISP 只证明路由行为，不是维护者当前供应商连接证据。

[已验证] `node packaging/test-browser-environment.cjs`：实际已安装 Edge，临时专用 profile、本地 HTTP 代理与 UDP STUN 捕获。未限制的正向对照捕获 UDP；启用限制后捕获为零。`.invalid` 主机名经代理请求成功，中文语言与预期一致。无外网访问或真实策略写入。测试先捕获 resolver 规则误伤 loopback 与普通 Chromium 不采用 force 参数两项问题，修复后通过。

该真实 Edge 测试连续通过三次；没有残留测试浏览器。`packaging/test-cooperation.cjs` 验证导出脚本保留原规则、组和选择，重复应用一致，冲突和环路拒绝。原生 `test-shell-identity.ps1` 验证独立窗口标识、图标及无令牌重开；更新器 SelfTest 和 `test-updater-shell.ps1` 验证取消不授权停服，均通过。本任务没有运行生产升级或重载。

[已验证] `node packaging/test-ui.cjs`：真实页面/隔离后端验证两范围预览、改选项使计划失效、取消不应用、确认保存和恢复，以及规则筛选分页、站点折叠搜索、导航、Agent 双向开关、上游 CRUD 和 UI 热更。

[已验证，新增定向测试] client_cooperation 公共接口覆盖默认模板/CRLF、独立挂载、预览不写、安装恢复、共享/全局脚本、GUI DNS 覆盖、路径逃逸、自定义 JS、错误持久、旧计划、外部编辑、HTTP Origin/token、Agent 同操作及额外字段拒绝。真实 pinned 内核核对规则、真实 DNS wire 非零 ECS→零长度与 OPT 保留、实际 /dns/query JSON 契约均通过。Node VM 模拟 CVR 异常回退，确认 Rule 冲突仍返回选定域名 REJECT。

[推断 / 待现场验收] 用户真实 CVR 订阅刷新/激活、全局 Edge 策略实际重启生效、日常浏览器全部无泄露和真实供应商稳定性。本任务不做生产重载或外网 DNS 查询，不据隔离测试宣称这些现场项通过。GitHub 最终 CI 链接与状态见对应 PR/Release。

## 官方依据

- [Chromium 代理与远程解析](https://chromium.googlesource.com/chromium/src/+/HEAD/net/docs/proxy.md)
- [普通 Chromium 参数到偏好的映射](https://chromium.googlesource.com/chromium/src/+/HEAD/chrome/browser/prefs/chrome_command_line_pref_store.cc)：WebRTC 使用 `webrtc-ip-handling-policy`。
- [Edge QUIC](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/QuicAllowed)、[网络预测](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/NetworkPredictionOptions)、[WebRTC 全局限制](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/WebRtcLocalhostIpHandling)、[WebRTC origin 规则](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/WebRtcIPHandlingUrl)、[界面语言](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/applicationlocalevalue)。
- [mihomo DNS](https://wiki.metacubex.one/config/dns/)、[固定版本 UDP 路由实现](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/tunnel/tunnel.go#L615)。HTTP 不支持 UDP 不等于自动失败关闭。
- [RFC 7871 ECS](https://www.rfc-editor.org/rfc/rfc7871.html)。DoH 加密传输不等于没有 ECS 信息。
