# 选定站点协作与浏览器环境：开发验收记录

日期：2026-10-17。未发布、未部署；本任务未重载维护者生产网关与 Clash。代码中的 0.5.6 仍为继承的候选版本号，不代表这组功能已发布。

## 目标与实际状态

保留用户现有系统代理的规则和节点，只将用户选择的站点交给 AEC 原固定 ISP。普通网站、GPT 与 Arena 不自动改道。固定路径不可用或身份不匹配时宁可断连，不换出口。

已实现：局部脚本导出、固定身份门后的专用入口、选定域名 UDP 拒绝、局部 DoH 固定传输、看板与 Agent 入口、浏览器两范围预览/确认/恢复、中性紫色静态图标、托盘分层。

未完成：Clash Verge Rev 持久订阅扩展挂载、已有自定义脚本安全组合、激活与运行规则读回；协作模式的完整就绪模型；所有日常浏览器泄露的现场证据；EDNS/ECS 权威查询证据；管理员防绕过组件。当前只能导出，不能把它当作端到端已交付。

## 浏览器范围

“仅 AI 浏览器”只影响 AEC 自己的专用目录。确认后新启动参数限制非代理 WebRTC UDP、关闭 QUIC、阻断本地目标解析并仅排除 loopback 代理主机；语言使用用户选择，不改变系统语言或伪造指纹。已运行实例参数不一致时要求用户自己关闭，不静默忽略参数。

“全局浏览器”当前只支持普通 Edge 的全部 profile。Chrome / Chrome for Testing 不写全局策略，不修改独立指纹项目的文件和 profile。独立 profile 不能阻挡同产品全局策略，因此指纹浏览器如使用 Edge，不能使用此范围。预览列明原值、目标、影响与排除项，用户确认后才写入。QUIC、语言等需要用户自行重开 Edge；保存成功不代表运行生效。

两个范围都保存 AEC 自己的专用 Edge / Chrome 启动参数；全局预览也明确列出这部分影响。排除的是日常 Chrome、Chrome for Testing 与第三方指纹实例，不是 AEC 自己管理的专用浏览器目录。

恢复使用精确原值，包括原来不存在的策略。发现外部改写时不覆盖，保留恢复记录。应用中途失败尝试回滚；快照不完整时拒绝静默启动未受保护的专用浏览器。最近错误的动作、错误码和时间保留，Agent 可读取，不依赖弹窗。

全局 Edge 的网络预测关闭不能代替 DNS 隧道。系统时区是独立整机设置，只经既有接管门修改；Windows 显示语言不改。浏览器语言策略不保证网站 Accept-Language。DNS / UDP / WebRTC / IPv6 / ECS 分项缺证据仍为未知。

## 自测命令与证据

[已验证] `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --quiet -- --include-ignored` 全部通过：272 项，0 失败，0 忽略，包含本机 pinned mihomo 的真实隔离链路。

[已验证] `cargo test -p aec-core --test browser_environment`：8 项通过，包括确认门、HTTP Origin/令牌、Agent 不可确认、中文原值恢复、过期/旧计划与外部冲突、部分写入回滚、崩溃恢复、错误持久读取和实际启动参数接入。系统写入使用 FakeBrowserHost，没有更改真实注册表策略。

[已验证] `cargo test -p aec-core --test cooperation -- --include-ignored`：实际 pinned mihomo 的选定站点路由、前置 UDP 拒绝、双内核固定入口阻断及 DoH 失败不走普通解析。合成 ISP 只证明路由行为，不是维护者当前供应商连接证据。

[已验证] `node packaging/test-browser-environment.cjs`：实际已安装 Edge，临时专用 profile、本地 HTTP 代理与 UDP STUN 捕获。未限制的正向对照捕获 UDP；启用限制后捕获为零。`.invalid` 主机名经代理请求成功，中文语言与预期一致。无外网访问或真实策略写入。测试先捕获 resolver 规则误伤 loopback 与普通 Chromium 不采用 force 参数两项问题，修复后通过。

该真实 Edge 测试连续通过三次；没有残留测试浏览器。`packaging/test-cooperation.cjs` 验证导出脚本保留原规则、组和选择，重复应用一致，冲突和环路拒绝。原生 `test-shell-identity.ps1` 验证独立窗口标识、图标及无令牌重开；更新器 SelfTest 和 `test-updater-shell.ps1` 验证取消不授权停服，均通过。本任务没有运行生产升级或重载。

[已验证] `node packaging/test-ui.cjs`：真实页面/隔离后端验证两范围预览、改选项使计划失效、取消不应用、确认保存和恢复，以及规则筛选分页、站点折叠搜索、导航、Agent 双向开关、上游 CRUD 和 UI 热更。

[推断 / 待验收] 全局 Edge 策略运行生效、日常浏览器无泄露、客户端持久扩展与订阅刷新后的防绕过。未跑现场验收，不能显示通过。GitHub 工作流已加入新自测，但本功能分支尚未推送，暂无本分支 GitHub CI 结果。

## 官方依据

- [Chromium 代理与远程解析](https://chromium.googlesource.com/chromium/src/+/HEAD/net/docs/proxy.md)
- [普通 Chromium 参数到偏好的映射](https://chromium.googlesource.com/chromium/src/+/HEAD/chrome/browser/prefs/chrome_command_line_pref_store.cc)：WebRTC 使用 `webrtc-ip-handling-policy`。
- [Edge QUIC](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/QuicAllowed)、[网络预测](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/NetworkPredictionOptions)、[WebRTC 全局限制](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/WebRtcLocalhostIpHandling)、[WebRTC origin 规则](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/WebRtcIPHandlingUrl)、[界面语言](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/applicationlocalevalue)。
- [mihomo DNS](https://wiki.metacubex.one/config/dns/)、[固定版本 UDP 路由实现](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/tunnel/tunnel.go#L615)。HTTP 不支持 UDP 不等于自动失败关闭。
- [RFC 7871 ECS](https://www.rfc-editor.org/rfc/rfc7871.html)。DoH 加密传输不等于没有 ECS 信息。
