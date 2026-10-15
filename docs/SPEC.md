# 第一版规格：Agent 网络与环境控制器

> 文中的 **legacy** 指本项目的前身：维护者此前在 Windows 上自用的一套出口链路（mihomo + Python relay + PowerShell 脚本 + 托盘）。它不随本仓库发布，只作为行为参照和「外部写入者」探测的样本出现。

- 状态：草案，待用户审阅
- 日期：2026-10-10
- 术语以 [CONTEXT.md](../CONTEXT.md) 为准；约束以 ADR 0001–0007 为准。和 ADR 冲突时以 ADR 为准，并在这里标注。

## 1. 问题陈述

用户在 Windows 本机让 Claude、Codex 等 Agent 工作时，需要一个固定、可验证的网络出口和一致的时区/语言环境；平时又要回到自己的日常环境。现在这套能力由一堆 PowerShell 脚本、托盘和 shim 拼成，问题有：

- 链路「活着」不等于「被用上」：系统代理入口指错、浏览器沿用旧实例、启动参数被静默丢弃，都曾让流量走错出口，却没有任何提示。
- 进入和退出没有快照，退出时只能执行固定脚本，不知道应该回到什么状态，也不知道恢复是否成功。
- 检测结果分散在日志和外部网页里，没有统一的「是否可以开工」判断。
- 实验性功能（渠道切换、Prism、打票等）和核心链路混在同一个托盘和启动流程里，维护面大、误操作多。

## 2. 解决方案

一个常驻托盘的 Windows 桌面应用（Tauri 壳 + Rust 控制层 + mihomo 内核），提供两种模式：

- **进入 Agent 模式**：先拍环境快照，再起内核和 relay，按档案设置允许的环境，启动选定的入口，然后跑环境验收。只有全部适用的硬门槛都通过，才显示「Agent 模式已就绪」。
- **退出到日常模式**：关闭本应用启动的入口，按快照和变更日志逆序恢复，并逐项验证。恢复失败的项保留展示，直到用户重试或确认。

每个失败或未知的检查项都附证据和一段可复制的排查提示词，用户自己决定是否交给 Agent。系统代理、时区等全局状态的写入受接管门保护：开发期和 legacy 链路仍在运行时，只做 dry-run。

## 3. 用户故事

1. 作为用户，我想一键进入 Agent 模式，这样不用手动起内核、改时区、开专用浏览器。
2. 作为用户，我想在进入前自动保存当前环境，这样退出时能回到进入前的真实状态，而不是某个写死的默认值。
3. 作为用户，我想只有在出口、目标域名、DNS、时区、语言都核实通过后才看到「已就绪」，这样不会在错误出口上登录账号。
4. 作为用户，我想看到每项检查的证据、来源和时间戳，这样能自己判断结果可不可信。
5. 作为用户，我想让未知结果明确显示为「未知」，这样检测服务挂掉时不会被误判为通过。
6. 作为用户，我想知道经代理看到的出口 IP、ASN、地区和延迟，这样能确认用的是预期的固定出口。
7. 作为用户，我想确认目标站（claude.ai、chatgpt.com 等）看到的出口 IP 和我的固定出口一致，这样能发现分流规则漏网。
8. 作为用户，我想确认目标域名由内核按域名解析和转发、没走 DIRECT，这样能发现 DNS 走错路径。
9. 作为用户，我想确认 Agent 浏览器实际的 `navigator.language`、Accept-Language 和时区，这样能发现启动参数被丢弃。
10. 作为用户，我想看到 WebRTC、Canvas、WebGL、Cookie 的只读信息，必要时有警告，这样能了解风险，而不是被一个总分糊弄。
11. 作为用户，我想要一个不调用第三方检测服务的仅本地模式，这样在隐私敏感或外部服务不可用时仍能做基础检查。
12. 作为用户，我想看到并能编辑使用了哪些外部检测服务，这样服务失效时可以替换。
13. 作为用户，我想有手动测试链接（如 ip.net.coffee），在 Agent 浏览器里打开，这样能用自动检测之外的工具复核。
14. 作为用户，我想每个失败或未知项都有一段可复制的排查提示词，这样可以直接交给 Claude/Codex 处理。
15. 作为用户，我想要「复制全部问题」，这样一次就能把完整上下文交给 Agent。
16. 作为用户，我想确认提示词里不含密码、secret、Cookie 和长期标识，这样复制出去也不会泄密。
17. 作为用户，我想退出 Agent 模式时逐项看到恢复结果，这样知道哪些已经回到日常状态。
18. 作为用户，我想在恢复失败时看到失败项、证据和排查提示词，并能重试，这样不会在半恢复状态下浑然不觉。
19. 作为用户，我想在 Agent 模式期间别的程序改了系统代理时，退出不会盲目覆盖，而是提示冲突，这样不会和其他程序互相打架。
20. 作为用户，我想关闭窗口时应用缩到托盘，这样不会误退出。
21. 作为用户，我想从托盘选择「退出」时先恢复日常模式再退出，这样不会留下 Agent 环境。
22. 作为用户，我想在应用崩溃或断电后再启动时被告知「上次 Agent 会话未正常退出」，并能一键恢复，这样不会长期停留在错误状态。
23. 作为用户，我想看托盘图标颜色就知道当前状态（日常、就绪、未验证、恢复未完成），这样不用打开窗口。
24. 作为用户，我想从托盘直接启动 Edge、Chrome、Claude CLI、Codex CLI 等入口，这样入口统一、参数正确。
25. 作为用户，我想在启动专用浏览器时，如果同一 profile 的实例已经以不同参数在运行，能收到明确提示，而不是参数被静默丢弃。
26. 作为用户，我想只用本机已安装的 Edge/Chrome，需要下载任何东西时先征求我同意。
27. 作为用户，我想用设置页填写出口候选、目标域名、时区、语言和入口，这样不用手改配置文件。
28. 作为用户，我想把上游凭据存在 Windows 凭据管理器里，这样配置文件和日志里不出现明文。
29. 作为用户，我想在检测到 legacy 链路等外部写入者时，接管全局状态的开关不可用并说明原因，这样新旧两套不会同时改系统代理。
30. 作为用户，我想在接管门关闭时也能看到「如果打开会改什么」（dry-run 计划），这样能预先核对。
31. 作为用户，我想新产品的开发实例不占用 legacy 链路的端口，也不影响它，这样开发期间日常工作不受打扰。
32. 作为用户，我想要 NSIS 安装包和便携 zip，这样能正常安装，也能免安装试用和回滚。
33. 作为用户，我想卸载前应用先恢复日常模式，这样卸载不会留下代理或时区改动。
34. 作为用户，我想所有操作都有结构化日志和错误码，这样出问题时 Agent 能根据日志排查。
35. 作为用户，我想前端不能执行任意命令，这样即使界面出问题也不会改动系统其他部分。
36. 作为用户，我想看快照历史（最近 10 份），这样能对照「上次进入前是什么状态」。
37. 作为开发者，我想状态机能用假适配器做单元测试，这样不用真改系统就能验证进入、退出和崩溃恢复逻辑。
38. 作为开发者，我想 relay 的重写版本和 legacy 的行为可以对照测试，这样不会丢掉候选池、重试、限流和拨号预算这些已验证的语义。

## 4. 实现决策

### 4.1 架构与模块

```
Tauri 前端（WebView2）
   │ 只能调用白名单命令（4.12）
Tauri 宿主 —— 托盘、窗口、单实例、会话结束钩子
   │
Controller（Rust，唯一对外的业务入口）
   ├─ 状态机与变更日志（state store）
   ├─ Profile 存储 / 快照存储 / 凭据存储（Credential Manager）
   ├─ 全局状态适配器：SystemProxy、Timezone（real | dry-run）
   ├─ 入口启动器：BrowserLauncher（Edge/Chrome）、CliLauncher
   ├─ 内核管理：CoreProcess（mihomo 子进程 + 运行配置生成）
   ├─ Relay（Rust 重写，进程内任务）
   ├─ 检测：CheckRunner + Provider 契约 + 本地检查页服务
   └─ 报告：CheckReport、RestoreReport、排查提示词生成
```

- **Controller** 是测试的最高接缝（见第 5 节）。Tauri 宿主只负责把命令和事件转发给它。
- 每个适配器和启动器都是 trait，生产实现和测试替身实现同一个接口。
- 第一版不新增独立 Windows 服务，不需要管理员权限。

### 4.2 运行目录、端口和进程隔离

| 项 | 安装版 | 便携版 | 开发 |
| --- | --- | --- | --- |
| 数据目录 | `%LOCALAPPDATA%\AgentEgressController\` | `<exe 所在目录>\data\`（同目录有 `portable.flag` 时启用） | 仓库 `runtime\dev-data\`（gitignore） |

数据目录下的结构：`profiles\`、`snapshots\`、`state.json`、`providers.json`、`core\`（运行配置，退出时删除）、`browser-profiles\edge|chrome\`、`logs\`。

默认端口都只绑定 `127.0.0.1`，mixed-port 可在设置里改，其余内部端口目前固定：

| 用途 | 默认端口 |
| --- | --- |
| mihomo mixed-port | 17893 |
| mihomo external-controller | 17909（随机 secret，每次启动重新生成） |
| mihomo DNS | 17553 |
| relay SOCKS5 入站 | 17325 |
| 本地检查页服务 | 17980 |

2026-10-10 核查时，这些端口都没有被占用；legacy 链路使用 7893/9099/10553/12325。

mixed-port 可覆盖：设置页「本机代理入口端口」保存到 `settings.json` 的 `coreMixedPort`，字段缺失或为 0 时用默认值。表单留空或 0 重置默认，旧表单缺字段保留原值；其他值须为 1–65535，不得与控制、DNS、relay、隧道、固定出口检测端口重复。只在日常模式保存，下次进入生效，托盘和开发命令共用已保存端口。
接管已有链路时复用客户端入口，迁移准备在隔离检测结束后记录旧入口端口，不改用户级代理环境变量。内部端口目前仍只读默认值。

系统代理由单一写入者管理；其他代理客户端关闭系统代理开关，内核可继续作为普通流量上游。本程序不合并外部全局绕过列表。发现外部改写时撤销 G6/就绪并通知，不自动写回。用户关闭冲突开关并重新进入后，再取得新的 G6 证据。

非 trace 的 `oaiusercontent.com` 目标探测 `https://files.oaiusercontent.com/`，G3 核对该主机的路由记录，G2 保留根域检查 ID 并显示实际 URL。路由保护仍匹配根域及所有子域名；实际服务不可达仍报告失败，不跳过检测，不将可达性冒充出口 IP 证据。

隔离规则：

- 启动前做端口预检。端口空闲才继续；如果已被占用，只有当占用进程的可执行路径在本应用数据目录下、并且 PID 和启动时间与 `state.json` 中记录的一致时，才视为「自己的进程」并接管，否则报 `PORT_IN_USE`，不结束任何外部进程。
- mihomo 二进制以 `aec-mihomo.exe` 的名字放在应用目录，进程名与 legacy 的 `mihomo.exe` 区分开。
- 单实例 mutex 为 `Local\AgentEgressController.App`。外部写入者探测使用 legacy 的 `Local\AIEgressGuard`、`Local\AIEgressModeTray`。
- 只结束本应用记录在案的进程（PID + 可执行路径 + 启动时间三者都匹配）。

### 4.3 数据模型（schemaVersion = 1）

以下 JSON 形状是决策本身，字段名在实现中保持一致。

**AgentProfile**（第一版只有一个，id 为 `agent-default`；存储结构是数组，为多个 profile 预留）

```jsonc
{
  "schemaVersion": 1, "id": "agent-default", "kind": "agent", "name": "Agent",
  "egress": {
    "candidates": [ { "id": "c1", "host": "…", "port": 12324, "proto": "socks5|http", "credentialRef": "c1|null" } ],
    "relay": { "connectTimeoutMs": 1200, "dialBudgetMs": 4000, "cooldownMs": 3000, "maxInflight": 12, "maxRounds": 3 },
    "expected": { "ip": "…|null", "countryCode": "US|null", "asn": 0 }
  },
  "proxyScope": { "mode": "explicit_only | system_and_explicit", "bypass": ["<-loopback>", "127.0.0.1", "localhost"] },
  "routing": {
    "outbounds": [],
    "groups": [{"name":"agent-egress", "type":"select", "members":["relay"], "check":null, "switchable":false, "selected":null}],
    "rules": [{"kind":"domain_suffix", "value":"claude.ai", "target":"agent-egress", "note":null, "noResolve":false}],
    "final":"agent-egress"
  },
  "locale": {
    "timezone": { "windowsId": "Eastern Standard Time", "iana": "America/New_York" },
    "browserLang": "en-US", "acceptLanguage": "en-US,en"
  },
  "targets": [ { "domain": "claude.ai", "trace": true }, { "domain": "www.anthropic.com", "trace": true }, { "domain": "chatgpt.com", "trace": true } ],
  "entries": [
    { "id": "edge", "type": "browser", "browser": "edge", "openCheckPage": true, "extraArgs": [] },
    { "id": "chrome", "type": "browser", "browser": "chrome", "openCheckPage": true, "extraArgs": [] },
    { "id": "claude-cli", "type": "cli", "command": "claude", "env": {} },
    { "id": "codex-cli", "type": "cli", "command": "codex", "env": {} }
  ],
  "detection": { "mode": "standard | local_only" }
}
```

- `expected.ip` 按精确 IP 匹配（2026-10-10 用户决定），不接受网段。留空时，第一次取得 `ipProxy` 后提示用户「固定为该 IP」，用户确认后才写入，不会自动固定。供应商更换 IP 后，需要用户在设置里重新确认。
- 浏览器入口默认不加 WebRTC 限制参数（2026-10-10 用户决定）。WebRTC 只由 R3 只读检测并告警；`extraArgs` 允许用户自行添加 `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`，R3 的排查提示词会提到这个选项。
- 浏览器可执行文件只在标准安装位置和注册表 App Paths 中查找，找不到就报 `ENTRY_NOT_INSTALLED`，不下载。
- CLI 入口按 PATH 解析，但跳过 legacy `shim` 目录，避免套两层代理；子进程注入 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY`、`NODE_USE_ENV_PROXY=1`，沿用 legacy 已验证的变量集合。

路由完整定义见 ADR-0008/0009：具名 HTTP/SOCKS5/AEAD SS 出站，select/fallback/url-test 组，有序 domain/domain_suffix/domain_keyword/ip_cidr/geosite/geoip 规则与最后兜底。凭据只能用 `credentialRef`；可选 `cipher` 与 `dialerProxy` 用于加密传输链。所有引用校验、共享命名空间、空组与环在启动前检查。预设只创建初始路由数据，旧模板字段只在档案加载迁移处识别。

`egress.tunnel` 可选结构：`{enabled, preset, servers:[{host,port}], cipher, credentialRef, directFallback, stepTimeoutMs}`。这是 relay 候选池的统一加密第一跳，与路由表中的具名传输出站独立。GEOSITE/GEOIP 需要用户提供可信的数据文件；程序不自动下载。直接与隧道路径仍连接用户配置的同一个出口，是否符合预期 IP 以 G1/G2 的真实证据为准。

`aec-dev profile import <yaml>` 只导入路由并迁移凭据，不采用源文件的 DNS、TUN 或监听端口。支持常规块列表规则注释；拒绝未知出站/组字段、provider、UDP 和不支持规则，避免静默改变语义。导入前保存原档案副本，凭据与档案保存失败会清理此次新增凭据。`profile verify` 只读比对出站字段、组及完整规则序列，不存凭据。

托盘列出 `switchable: true` 的 select 组，与看板和 `aec_route` 共用串行切换操作。切换前撤销就绪，保存 `selected` 并重载内核、断开现有连接，重新检测；启动恢复选择。固定模式下非固定成员只解除直接指向该组的显式 DOMAIN-SUFFIX 范围的保护，固定成员恢复保护后验证才放行。其他域名、预期 ISP IP 与第一跳不变；前置规则覆盖或更大父域保护拒绝快捷更改。不猜测 Geo/IP/关键词或精确域名规则的保护范围。重新加入的站点默认 trace=false，独立 ISP 探测保持精确验证；至少保留一个检测站点，档案保持旧版可读。

**DailyProfile**

```jsonc
{
  "schemaVersion": 1, "id": "daily-default", "kind": "daily", "name": "日常",
  "restore": { "source": "snapshot", "closeLaunchedEntries": true },
  "fallback": { "timezoneWindowsId": "China Standard Time", "systemProxy": null }
}
```

`fallback` 只在快照缺失或损坏、并且用户在恢复界面上明确选择「使用日常档案默认值」时才使用。

**EnvironmentSnapshot**（`snapshots\<ulid>.json`；保留当前活动的 1 份，加只读历史 10 份，超出的按时间删除最旧的）

```jsonc
{
  "schemaVersion": 1, "id": "01J…", "takenAt": "2026-10-10T21:00:00-04:00", "appVersion": "0.1.0",
  "profileId": "agent-default",
  "systemProxy": { "flags": 3, "proxyServer": "127.0.0.1:7893", "bypass": "…", "autoConfigUrl": null, "autoDetect": false },
  "timezone": { "windowsId": "China Standard Time", "dynamicDaylightDisabled": false },
  "foreignWriters": [ { "kind": "legacy_guard_mutex", "evidence": "Local\\AIEgressGuard held" } ],
  "takeoverGate": "open | closed"
}
```

系统代理按 WinINet 的「每连接选项」读写（标志位、代理服务器、绕过列表、PAC 地址、自动检测）。快照覆盖这五项，而不只是注册表里的 `ProxyServer` 和 `ProxyEnable`。待实测：用 WinINet API 读写和直接写注册表的差异，作为票据 03 的验证项。

**state.json**（每次状态迁移都先写入临时文件，再原子替换）

```jsonc
{
  "schemaVersion": 1, "mode": "daily|entering|agent_ready|agent_unverified|exiting|restore_incomplete",
  "phase": "snapshot|core|global|entries|checks|null",
  "activeSnapshotId": "01J…|null",
  "journal": [ { "item": "systemProxy|timezone", "before": {}, "after": {}, "appliedAt": "…", "dryRun": true } ],
  "ownedProcesses": [ { "role": "core|browser:edge|cli:claude", "pid": 0, "exe": "…", "startedAt": "…" } ],
  "lastReportId": "…", "unacknowledgedRestoreFailures": []
}
```

### 4.4 状态机

| 当前状态 | 事件 | 动作 | 下一个状态 |
| --- | --- | --- | --- |
| daily | 用户「进入」 | E1–E6 | agent_ready / agent_unverified；E2–E5 失败 → 自动回滚 → daily 或 restore_incomplete |
| agent_ready / agent_unverified | 用户「重新检查」或定时检查（5 分钟） | 重新执行 E6 | agent_ready / agent_unverified |
| agent_ready | 检查发现硬门槛退化 | 托盘变黄并通知 | agent_unverified |
| agent_ready / agent_unverified | 用户「退出到日常」或「退出应用」 | X1–X4 | daily / restore_incomplete |
| restore_incomplete | 用户「重试」 | 只对失败项重新执行 X2–X4 | daily / restore_incomplete |
| restore_incomplete | 用户「确认并保留现状」 | 把失败项写入历史，标记为已确认 | daily（报告中保留失败记录） |
| 任意非 daily 状态（应用启动时从 state.json 读到） | 启动 | 只读比对当前环境与变更日志，展示「上次会话未正常退出」 | 停在原状态，等用户选择「恢复」（执行 X1–X4）；不自动写入 |

**进入步骤**（每步都有超时，失败就回滚已完成的步骤）：

- E1 预检：profile 校验；端口预检；入口可执行文件是否存在；评估接管门；凭据是否齐全。失败直接回到 daily，不做任何改动。
- E2 快照：通过全部适配器读取并写盘（落盘后才继续），然后设置 `activeSnapshotId`。
- E3 内核：生成运行配置（凭据从凭据管理器注入，文件放在数据目录 `core\`，退出时删除），启动 relay 任务和 `aec-mihomo.exe`，15 秒内端口进入监听，controller 的 `/version` 响应正常。
- E4 全局状态：对每个适配器执行 `plan → apply → verify`。接管门关闭时只记录 dry-run 计划，变更日志中标 `dryRun: true`。`proxyScope.mode = explicit_only` 时不规划系统代理变更。
- E5 入口：只启动用户这次选中的入口（默认是档案里第一个浏览器入口）。浏览器如果已有同一 user-data-dir 的实例：启动参数一致就只开新窗口；不一致就弹窗请用户关闭该实例，绝不强制结束进程，用户取消则这一项记为失败。
- Claude 桌面版由用户正常启动，通过可选系统代理接管进入同一套路由；它不是本应用入口，不加入 owned 进程、不改安装/配置/语言、不要求启动前退出，也不产生浏览器 G5/R3–R5。其 Node/MCP/Cowork 等独立路径是否遵循代理必须分别验证。
- E6 检查：执行第 4.5 节的检查，生成 CheckReport，然后按硬门槛判定是 ready 还是 unverified。

**退出步骤**：

- X1 关闭本应用启动的入口：对浏览器调用 `CloseMainWindow`，最多等 20 秒；CLI 不主动结束，只提示用户。失败记为恢复项「入口仍在运行」，不阻塞后续步骤。
- X2 按变更日志逆序恢复全局状态：先读当前值；如果仍等于 `after`，说明还归本应用所有，就写回 `before` 并验证；如果不等于 `after`，说明已被外部改写，记为「冲突」，不覆盖，附排查提示词。dry-run 条目只记录，不写。
- X3 停止本应用拥有的内核和 relay，确认端口释放，删除运行配置。
- X4 生成 RestoreReport。全部通过 → daily；否则 → restore_incomplete。

**会话结束**（注销或关机）：收到会话结束通知后，在 5 秒预算内执行 X2 中的系统代理和时区恢复；没完成的，下次启动时按崩溃恢复流程处理。

### 4.5 环境验收

四态的含义：通过 = 证据满足目标；警告 = 不影响就绪但需要留意；失败 = 证据不满足；未知 = 证据不足。每项检查有 `applies(profile, gate)`，不适用的项不显示在就绪判定里。

**就绪判定**：全部适用的硬门槛都是「通过」→ `agent_ready`；否则 → `agent_unverified`。警告不影响就绪。

**硬门槛**：

| ID | 检查 | 通过 | 失败 | 未知 |
| --- | --- | --- | --- | --- |
| G1 `egress.effective` | 经 mixed-port 请求**目标域名自己的** trace，取得 `ipProxy`；标准模式下同时直连 `www.cloudflare.com/cdn-cgi/trace` 取得 `ipDirect` | 取得 `ipProxy`；与 `expected.ip`/`countryCode` 一致（已设置时）；与 `ipDirect` 不同（已取得时） | 与 expected 不一致，或等于 `ipDirect` | 所有目标 trace 都超时或失败 |
| G2 `target.reachable`（每个目标域名一行） | 经代理请求 `https://<domain>/cdn-cgi/trace`；`trace: false` 的目标改用 `HEAD /` | 收到 HTTP 响应（< 500），并且 trace 的 ip 等于 `ipProxy` | 连接失败或超时（重试 1 次后）、5xx，或 trace ip 与 `ipProxy` 不同（分流漏网） | 内核 controller 不可用，无法判断请求是否经过内核 |
| G3 `dns.path` | 内核 DNS 在监听；本轮检查期间内核连接日志里的目标连接 | 每个目标带域名，链路命中有序路由数据决定的目标 | 命中 DIRECT、REJECT、错误目标或只带 IP | 拿不到连接记录，或前置 Geo/IP 规则无法从域名证据判断 |
| G4 `locale.timezone` | 系统时区 Windows ID；有检查页结果时，加上浏览器的 `Intl` 时区 | 都与档案一致 | 不一致 | 读取失败；或接管门关闭、当前时区又不等于目标（原因写「全局状态未接管」） |
| G5 `locale.language`（每个浏览器入口一行，只在本次启动了浏览器入口时适用） | 检查页上报的 `navigator.language`/`languages`，以及服务端收到的 `Accept-Language` | 与 `browserLang`/`acceptLanguage` 一致 | 不一致 | 15 秒内没有收到检查页上报 |

补充规则：

- 为什么 G1 不用 `1.1.1.1` 经代理的 trace：2026-10-11 基线实测，legacy 配置按规则分流。经同一个 7893，`claude.ai` 落在固定住宅出口（美国），`1.1.1.1` 却落在另一个出口（日本网段）。所以「固定出口」只能用目标流量本身来证明。经代理请求非目标站 trace 的结果作为只读信号 R9「非目标流量出口」展示，不参与就绪判定。
- 为什么不用 IP 字面量 `1.1.1.1`：2026-10-11 实测，本机直连 `https://1.1.1.1/cdn-cgi/trace` 超时（curl exit 28），直连 `https://www.cloudflare.com/cdn-cgi/trace` 正常（200，0.9 s）。所以 trace 一律用域名形式。
- G4 通过但时区不是本应用写入的（例如 legacy 维持的），另加一条警告「时区由外部写入者维持」。
- `proxyScope.mode = system_and_explicit` 时增加硬门槛 G6 `proxy.system_scope`：系统代理五项与档案一致为通过，不一致为失败；接管门关闭时为未知。

**只读信号**（不参与就绪判定，状态只用通过、警告、未知）：

| ID | 内容 | 警告条件 |
| --- | --- | --- |
| R1 ASN/地区 | api.ip.sb 和 ipwho.is 对 `ipProxy` 的结果，并排展示 | 两个来源的 ASN 或国家不一致；与 `expected.asn` 不一致 |
| R2 延迟 | 经代理请求 trace 3 次的中位数 | 超过 1500 ms |
| R3 WebRTC | 检查页收集的 ICE 候选（标准模式使用 providers.json 里的 STUN 服务，仅本地模式只看 host 候选） | 出现与 `ipProxy` 不同的公网 srflx 地址。没有任何公网候选时判「未知」而不是通过：STUN 不可达和「没有泄露」无法区分（2026-10-11 实测：本机连不上 Google STUN） |
| R4 Canvas/WebGL | 固定绘图的哈希，以及 WebGL vendor/renderer 字符串 | 不产生警告，只展示 |
| R5 Cookie | `navigator.cookieEnabled` | 被禁用时警告 |
| R6 系统语言 | 系统 UI 语言、用户语言列表、区域格式 | 不产生警告，只展示；说明「系统显示语言要注销才生效，第一版不切换」 |
| R7 外部写入者 | 外部写入者探测结果 | 检测到外部写入者时警告 |
| R8 系统代理现状 | `explicit_only` 模式下，展示当前系统代理指向哪里 | 不产生警告，只展示 |
| R10 时区归属 | G4 通过时，时区是否由本应用在本次会话中设置 | 不是本应用设置的，并且进入时存在外部写入者（替代上文「G4 另加一条警告」的说法） |
| R9 非目标流量出口 | 经代理请求 `www.cloudflare.com/cdn-cgi/trace` 得到的 IP 和地区（默认路由模板下它和目标流量同一出口；分流模板下才会不同） | 不产生警告，只展示；用于说明分流规则的效果 |
| R11 Claude 桌面版当前连接 | 两次 Windows TCP 读取间稳定的桌面进程身份与本应用内核实时连接表关联，核对域名路由及实际出站链 | Claude 服务域名命中 DIRECT/REJECT，或当前链不符合档案（包括显式 select 成员）；无证据为未知 |

R11 只证明观测到的连接，至少一个 Claude 服务域名有证据且全部稳定样本可判定、符合路由才通过；不能用 G1–G3 探测请求或 G6 配置读回冒充桌面证据。源 IP/端口、入站端口、PID、创建时间和可执行路径必须对应，控制口/mixed-port 所属内核身份必须在 API 读取前后核对。当前支持 `AnthropicClaude/app-<数字版本>/claude.exe`；空闲、已关闭连接、其他安装形态、IP-only、前置 Geo/IP 无匹配证据或读取失败均未知。固定 ISP 的实际出口仍由 G1/G2 给证据。R11 不参与就绪判定，不读取对话内容、不写系统、不替用户发消息或管理 Claude 进程。

不计算任何总分。

**本地检查页**：本地检查页服务只监听 `127.0.0.1:17980`，每次启动生成一次性 token。浏览器入口启动时，第一个标签页打开 `http://127.0.0.1:17980/check?t=<token>`（loopback 在代理绕过列表中，所以直连本机）。页面采集 R3–R5 和 G5 所需字段，POST 到 `/report`。服务端校验 token 和 Origin，并记录这次请求的 `Accept-Language`。数据只存在本机，不外发；页面不读取其他站点的 Cookie。

### 4.6 检测服务契约

`providers.json` 是用户可编辑的列表，每项包含：

```jsonc
{ "id": "cf-trace", "name": "Cloudflare trace", "purpose": ["egress_ip", "geo"],
  "url": "https://www.cloudflare.com/cdn-cgi/trace", "via": "proxy|direct", "parser": "cf_trace|ipsb_geoip|ipwhois",
  "timeoutMs": { "connect": 3000, "total": 8000 }, "minIntervalMs": 10000,
  "docsUrl": "…", "sends": "请求本身；geoip 类在路径中包含出口 IP", "enabled": true, "thirdParty": true }
```

- 内置项：`cf-trace`、`ipwho.is`、`api.ip.sb`。2026-10-11 实测：`api.ip.sb/geoip` 直连返回 403，经代理查询时为 200，所以 R1 以 ipwho.is 为第一来源，api.ip.sb 作复核；任一来源 4xx 时该来源判为「未知」，不影响另一来源。目标站的 trace 由 `targets` 自动派生，`thirdParty: false`。
- 请求规则：只用 GET/HEAD；不带 Cookie；不带 Referer；UA 固定为 `AgentEgressController/<版本>`；不附加任何持久标识。
- 解析失败、非 2xx、超时都转成「未知」，并带原因：`timeout`、`http_<code>`、`parse_error`、`disabled`、`local_only`。
- 结果缓存 60 秒；同一 provider 的请求间隔不小于 `minIntervalMs`。
- 仅本地模式：跳过所有 `thirdParty: true` 的 provider。G1 本来就用目标站 trace，不受影响；`ipDirect`、R1、R9 为「未知」；R3 只看 host 候选。
- 手动测试链接（可编辑列表）：`https://ip.net.coffee/`、`/claude/`、`/dns/`、`/webrtc/`。在 UI 中用「在 Agent 浏览器打开」按钮提供，应用不解析这些页面的结果。

### 4.7 全局状态适配器与接管门

适配器接口：`read() → State`、`plan(target) → Plan`、`apply(plan, mode) → Applied`、`verify(expected) → CheckResult`。`mode` 取值为 `real` 或 `dry_run`。

- **SystemProxyAdapter**：通过 WinINet 每连接选项读写，写完后发送 settings-changed 和 refresh 通知（legacy 已验证不发通知时，已经运行的程序不会感知变更）。
- **TimezoneAdapter**：读取用 `tzutil /g` 或等价 API；写入用 `tzutil /s` 或等价 API，写完后必须读回确认（legacy 的经验：不能只看退出码）。

接管门的判定（全部满足才 `open`）：

1. 设置项「接管全局状态」已由用户开启（默认关闭）；
2. 没有外部写入者：legacy 的 mutex 未被持有；启动文件夹和 HKCU Run 中没有指向其他出口控制器的项（已知特征：`AI-Egress.vbs`、`AI-Mode-Tray.vbs`、`startup-egress.ps1`）；没有本应用之外的进程监听当前系统代理所指的端口——例外：该端口就是本应用自己的 mixed-port；
3. E2 快照已落盘。

接管门关闭时，设置页显示具体是哪一条没满足，以及对应的排查提示词。

运行期间每 30 秒读取一次全局状态。和变更日志里的 `after` 不一致时，产生事件 `EXTERNAL_CHANGE`，G6 失败并撤销就绪；重复的同一冲突不重复通知，也不自动改回或争写。用户需关闭其他代理客户端的系统代理开关，然后退出并重新进入，重新取得快照和 G6 证据。

Windows 系统代理是单一全局值，两个代理客户端不能同时拥有它。需要系统级分流时，本程序接管该值并指向自己的 mixed-port；其他代理客户端可以只运行本地内核，用户可将其监听端口填入「本机代理」作为普通流量上游，并继续在客户端内切换节点，但必须关闭该客户端的「系统代理」开关。不要同时开启全局 VPN、TUN、透明代理或其他会接管整机流量的功能；它们可能改写系统代理或截获本程序的固定 ISP 连接，导致固定出口证据失败。检测到冲突时，程序不抢写，提示用户关闭冲突开关并重新进入。

### 4.8 排查提示词

由 `(检查结果 | 恢复项 | 设置缺项, 档案, 环境摘要) → 文本` 的纯函数生成，模板固定为下列小节：

1. 背景：一句话说明本应用、当前模式、出问题的项和状态。
2. 期望：档案里的目标值。
3. 证据：实际值、来源、时间戳和原始响应摘要（截断到 2 KB）。
4. 允许你修改的范围：例如「本应用数据目录下的档案」「当前用户的浏览器快捷方式」。
5. 不允许修改：前身项目的目录、legacy 进程、接管门设置、系统级（HKLM）配置、防火墙和路由。
6. 验证方法：应用内「重新检查」，或给出等价的 PowerShell 只读命令。
7. 回滚要求：说明改了什么，以及怎么改回。

脱敏规则：密码、`credentialRef` 指向的内容、mihomo secret、检查页 token 一律不输出。上游地址只保留 host 和端口。出口 IP 默认完整保留（排查需要）；设置里可以切换为只显示到 /24。

UI 提供单项复制和「复制全部问题」。只写剪贴板，不调用任何外部程序或 API。

### 4.9 托盘与窗口

- 托盘图标：灰色 = 日常；绿色 = 已就绪；黄色 = 未验证或有冲突；红色 = 恢复未完成。提示文字显示模式和硬门槛摘要。
- 托盘菜单：状态行；进入 Agent 模式 / 退出到日常模式；重新检查；启动入口 ▸（各入口）；复制全部问题；打开窗口；退出。
- 关闭窗口时隐藏到托盘，第一次隐藏时弹一次提示。
- 托盘「退出」：如果不在日常模式，先执行退出步骤；恢复未完成时弹窗，提供「重试 / 仍然退出（保留失败记录）/ 取消」三个选项。
- 单实例：再次启动时只聚焦已有窗口。
- 窗口分页：概览（模式、硬门槛卡片、只读信号）、报告（逐项证据和提示词）、快照（当前和历史，只读）、设置（档案、检测服务、手动链接、接管门、凭据、端口）、日志（最近 500 行）。

### 4.10 设置与凭据

- 设置页直接编辑档案，保存前做 schema 校验，错误逐字段显示。
- 凭据保存到 Windows 凭据管理器，目标名为 `AgentEgressController:<credentialRef>`，档案中只存引用。进入时按需注入内核/relay 临时运行配置，退出时删除，不回显、不写日志或提示词。
- 登录自启在托盘与看板均可设置，默认关闭；只写 HKCU Run `AgentEgressController`，精确指向当前安装 exe 和 `--enter`。拒绝覆盖另一安装的注册；未恢复会话及接管门不被绕过。
- 托盘和看板可主动创建当前用户桌面、开始菜单的托盘和看板快捷方式；看板用 `--dashboard` 转交已有实例。拒绝覆盖不同目标的同名入口；不注册服务、URL 协议或文件关联。

### 4.10.1 看板与固定出口保护（ADR-0010，0.3.0）

本机看板复用设置服务，GET dashboard/state 与 POST rules/path/startup/shortcuts 均要求随机令牌和正确 Host；写请求还要求本机 Origin。只接收结构化表单，不接收任意命令、原始 YAML 或路径。操作串行进入 worker，限制 128 站点、512 规则，校验失败不修改档案。

保护域名与子域名优先于全部用户规则。规则类型、出站与排序由选项管理，允许自定义域名/IP，Geo 分类建议是否可用以可信用户数据为准。新增站点默认仅测可达性，已有 trace 属性保留。

`egress.fixedExit` 和 `connectionPath` 兼容旧档案，默认不启用保护；启用要求精确预期 IP。路径为 direct、tunnel-auto 或 tunnel-N，第一跳 DIRECT 只直接连接 ISP 代理。固定模式无自动直连回退。内部 relay select 默认 REJECT，只有独立 probe 强制走 ISP 并核对精确 IP 后放行；probe 监听默认 17327，第一跳监听 17326。

切换/重载前撤销就绪、阻断并关闭受保护 relay 连接，失败维持阻断。周期检测约 30 秒，未知或不匹配重新阻断；连接表无法读取时不能宣称成功阻断。已经阻断时，后续巡检取得精确匹配证据，才重选已保存的同一第一跳，重新探测精确出口并放行，再刷新完整检测报告。若当前第一跳已精确验证但硬门槛报告仍未通过，再次完整检测而不重选或关闭健康连接。重开或探测失败继续阻断；恢复不修改系统代理或保存的出口身份。完整检测耗时不计入下一次巡检间隔，未通过硬门槛仍显示未验证。保护仅涵盖经本应用路由的保护域名，并不保证整机或供应商两次巡检之间永不换 IP。0.5.1 恢复组快捷切换，明确标注固定/非固定；只有用户主动选择的域名范围可以解除保护，普通出口在固定验证失败时可独立工作。

固定出口 tunnel 候选按配置顺序尝试，可选 `directFallback` 在其后增加连接同一 ISP 出口代理的候选；它不是另一个 ISP。所有候选仍必须通过独立 trace 精确匹配后才能放行。每次尝试受自身 step timeout 和总 dial budget 限制，耗尽时后续候选可能没有机会；配置需给多个候选留出预算。不会因固定出口验证失败而将受保护域名直接送往任意 `DIRECT` 路由规则。

每个数据目录同一时刻只允许一个会话写入者。托盘持有由操作系统管理的独占文件锁；`aec-dev` 的恢复、重检、进入、退出、内核变更、档案写入和快照写入在锁被占用时失败，不覆盖托盘内存中的状态。只读检查仍可运行，进程退出或崩溃时操作系统释放锁。

### 4.11 日志与错误

- 日志为 JSONL 格式，存放在 `logs\aec-YYYYMMDD.jsonl`，保留 14 天，总量上限 50 MB。字段：`ts`（带时区偏移的 ISO 时间）、`level`、`event`、`mode`、`phase`、`correlationId`、`data`（已脱敏）。
- mihomo 的 stdout 和 stderr 写入单独文件 `logs\core-*.log`。
- 错误码按前缀分族：`CFG_`（配置）、`PORT_`（端口）、`CORE_`（内核和 relay）、`ADAPTER_`（全局状态）、`ENTRY_`（入口）、`CHECK_`（检测）、`RESTORE_`（恢复）、`IO_`（存储）、`GATE_`（接管门）。每个面向用户的错误都包含：错误码、中文说明、证据和排查提示词。

### 4.12 Tauri 命令白名单

`get_state`、`enter_agent`、`exit_agent`、`recheck`、`launch_entry(id)`、`get_report(id)`、`get_prompt(itemId | all)`、`list_snapshots`、`get_profiles`、`save_profile`、`set_credential`、`delete_credential`、`get_settings`、`save_settings`、`list_providers`、`save_providers`、`open_manual_link(id)`、`tail_logs`、`acknowledge_restore_failures`、`retry_restore`、`quit_app`。

不提供执行任意命令、任意路径读写或任意 URL 请求的接口。`open_manual_link` 只能打开列表中已有的链接。

### 4.13 Relay（Rust 重写）

2026-10-11 修订：relay 以独立进程 `aec-relay` 运行，不再是进程内任务。原因是开发命令行每条命令执行完就退出，进程内的 relay 会随之消失；独立进程可以和 mihomo 一样由 CoreManager 管理，启动顺序与 legacy 一致（先 relay，后内核；停止时反过来）。候选池在内核中表现为 `relay`（`127.0.0.1:17325`），候选池的凭据只出现在 relay 的临时配置文件里，relay 读取后立即删除。2026-10-12 的 ADR-0008/0009 另允许具名路由出站与隧道，相关凭据按需注入内核临时配置，退出时清理；它们不改变候选池的 relay 边界。

保留 legacy 已验证的语义：

- 候选池：同一出口 IP 的多个 host:port:proto。启动时校验所有候选属于同一个出口 IP，否则拒绝启动——legacy 的约束：出口 IP 中途变化是最危险的信号，宁可失败也不换。
- 单个候选失败立即切到下一个；每轮按顺序尝试，最多 `maxRounds` 轮。
- 并发上游连接不超过 `maxInflight`。
- 一条入站连接的候选总耗时受 `dialBudgetMs` 约束，必须小于内核的 5 秒拨号超时（legacy 实测这个超时不可配置）。
- 拨号和握手超时只约束连接建立阶段。上游握手成功后清除上游读写超时，入站进入转发前清除客户端读写超时；不把这些短超时当成响应或流式传输的空闲超时。转发阶段等待数据或真正的 EOF/错误，任一方向结束时关闭两端。
- 失败候选进入 `cooldownMs` 冷却。
- 全部候选失败时如实返回失败，不伪造响应；日志记录尝试过哪些候选、各自的错误和握手阶段。

legacy `tcp-relay.py` 的语义逐条编号，Rust 实现和测试按编号对照（`crates/aec-core/src/relay.rs`、`relay/tests.rs`）：

| # | 语义 | 测试 |
| --- | --- | --- |
| S1 | 候选池非空，并且是同一个出口主机，否则拒绝启动 | `s1_pool_must_be_one_exit_host`、`s1_mixed_pool_refuses_to_start` |
| S2 | 入站 SOCKS5：优先用 0x02（凭据不校验），否则 0x00，都没有回 0xFF；只支持 CONNECT；地址类型 1/3/4 | `s2_inbound_method_selection_and_garbage` |
| S3 | 并发上限只限制拨号阶段，等待最多一个预算，等不到回失败 | `s3_gate_limits_concurrent_dials` |
| S4 | 预算内最多 `maxRounds` 轮，剩余不足 0.15 秒就停，失败切到下一个候选 | `s4_fails_over_and_records_every_attempt`、`s4_stops_at_the_budget_and_after_the_rounds` |
| S5 | 冷却只影响排序，不排除候选；全部在冷却时按最早到期排 | `s5_cooling_is_ordering_not_exclusion` |
| S6 | 每一步的超时都压在剩余预算内 | `s6_a_silent_upstream_cannot_outlast_the_budget` |
| S7 | 配置凭据时 HTTP 上游优先带 Basic，失败后保留匿名白名单回退；无凭据只匿名 | `s7_http_upstream_uses_configured_credentials_first`、`http_credentials_survive_silent_anonymous_provider`、`credentialed_upstreams_retain_anonymous_allowlist_fallback` |
| S8 | SOCKS5 每次只报一种方法；有凭据先 0x02，失败后 0x00；无凭据只 0x00 | `s8_socks5_upstream_offers_one_method_at_a_time`、`socks_credentials_survive_silent_anonymous_provider`、`credentialed_upstreams_retain_anonymous_allowlist_fallback` |
| S9 | 全部失败如实回 0x01，不伪造响应 | `s9_s11_s12_all_candidates_failing_is_reported_truthfully` |
| S10 | 双向转发，不继承握手超时；任一方向结束就关闭两边 | `s4_s10_fails_over_to_a_working_candidate_and_relays_bytes`、`s10_delayed_first_response_survives_upstream_handshake_timeout`、`s10_streaming_gap_survives_upstream_step_timeout`、`s10_client_idle_survives_inbound_handshake_timeout`、`s10_direct_dial_caller_can_wait_for_response_after_handshake`、`s10_tunnelled_transfer_survives_handshake_timeout` |
| S11 | 失败日志带上候选、错误、耗时和握手阶段；同类消息 2 秒内合并计数，第一条一定写入 | `s9_s11_s12_…`、`s2_…` |
| S12 | 日志里只有 host:port(proto)，没有凭据 | `s4_fails_over_…`、`s9_s11_s12_…` |

端到端测试 `crates/aec-dev/tests/relay_chain.rs`（需要 mihomo 二进制，`--include-ignored`）：客户端 → aec-mihomo → aec-relay → 失效候选 → 可用候选（本地假 SOCKS5）→ 本地 HTTP 服务。HTTP 服务延迟 1.8 秒才给首个响应，超过默认 1.2 秒握手超时，仍须完整返回。

### 4.14 权限

第一版全部以当前用户权限运行，只写 HKCU 和本应用数据目录。不写 HKLM；不建服务、计划任务、防火墙规则、路由或虚拟网卡；不使用 TUN。

推断，待验证：标准用户有修改时区的权限。legacy 托盘以普通权限运行时 `tzutil /s` 生效，可以作为佐证；新产品需要在 Windows 10 和 11 上各实测一次。

### 4.15 安装与打包

- NSIS 安装包，按当前用户安装（不需要管理员权限）；另提供便携 zip（含 `portable.flag`）。
- 卸载前要求用户从托盘退出 Agent 模式并确认恢复完成；卸载器请求关闭桌面窗口，若托盘进程仍锁定文件则中止卸载。卸载默认保留用户数据与 Credential Manager 条目。
- 当前用户安装至 `%LOCALAPPDATA%\Programs\`，不提权；安装 EXE 提供目录、快捷方式、登录自启选项。数据保留在 `%LOCALAPPDATA%\AgentEgressController\`，卸载默认不删除用户数据和凭据。
- WebView2：Windows 11 自带；Windows 10 缺失时显示安装说明，不自动下载。
- 第一版不签名，发布说明中写明 SmartScreen 会提示。
- mihomo 官方 release 的固定版本和 SHA256 写进构建脚本；下载前先征得用户同意。

## 5. 测试决策

- **唯一主接缝：Controller。** 测试通过 `Controller` 的公开操作（进入、退出、重新检查、启动恢复、确认失败项）驱动，注入假的 SystemProxy、Timezone、Launcher、CoreProcess、Provider、时钟和存储，断言状态迁移、快照内容、变更日志、报告和提示词。只测外部可观察的行为，不测内部函数调用顺序。
- 必须覆盖的场景：
  - 进入后各步失败时的回滚；
  - 硬门槛的每种失败和未知都不会就绪；
  - 退出时的外部改写冲突；
  - 恢复失败后的重试和确认；
  - 崩溃后重启的恢复流程；
  - 接管门关闭时只做 dry-run；
  - 端口被外部进程占用时拒绝启动且不结束该进程。
- 检查判定与提示词生成是纯函数，用表驱动测试和固定输出（golden）测试，包括脱敏用例。
- 真实适配器的测试只在 Windows 上运行，默认忽略、手动开启：只读真实系统代理和时区；dry-run 计划与真实读数一致。真实写入的测试只在接管门可打开的环境里（legacy 已被替换后，或在虚拟机中）手动执行。
- 内核集成测试：在 17xxx 端口用测试配置（出站为 DIRECT）启动 `aec-mihomo`，预期 G1 判为「失败（与直连 IP 相同）」，借此验证失败路径。
- Relay：用本地假的 SOCKS5/HTTP 上游，测试切换候选、冷却、拨号预算、并发上限，以及同一出口 IP 校验；行为对照 legacy `tcp-relay.py` 的说明逐条列出。
- 白名单测试：枚举注册的 Tauri 命令，断言与第 4.12 节的列表完全一致。
- 仓库目前没有可参照的现有测试，这是新建项目。

### 4.16 本机 Agent 控制（0.4）

- 托盘进程在本机启动 Agent HTTP adapter；监听 IPv4 loopback 随机端口。看板可显式启停访问，访问默认关闭。未启用时拒绝所有 Agent 请求；关闭托盘时终止监听并清理本次令牌和发现记录。
- MCP adapter 是发布包内 aec-agent.exe，经 stdio newline-delimited JSON-RPC 提供工具；自建本机客户端可使用同一个具名工具 contract 经 HTTP POST /api/v1/call 调用。工具名与严格 JSON schema 仅定义一份。
- HTTP 请求要求精确 loopback Host、当前进程 Bearer token、无 Origin/Transfer-Encoding 和小于 128 KiB 的 Content-Length body。无 CORS、无 LAN listener；不接受未知字段。浏览器看板仍使用独立 query token/Origin 保护。
- 发现文件 agent-api.json 只记录 schema version、随机 port、credentialRef 和 PID。Bearer secret 每次启动随机生成，写入当前用户 Windows Credential Manager，仅短暂保存在托盘内存；退出身份核对后清理。MCP config 无秘密。错误输出不得包含 body、口令或 bearer。
- 可调用的 operation 只包括状态、非密钥表单设置、网站/分流规则、固定出口第一跳、模式、登录自启和快捷方式。没有任意 shell、文件、URL、注册表、进程或 profile 命令。出口/隧道密码只由本机设置页面录入 Credential Manager。
- Agent 控制请求进入与页面相同的串行 Worker；返回执行后状态，而非表示“已排队”。超时可产生未知操作结果；调用方先读取状态，不盲目重试操作。调用成功不等于 Agent 模式就绪。
- Agent 适配器不能改期望 ISP IP、接管门或系统代理所有权开关。规则与网站变更要按用户明确意图；删除保护站点会失去该站点保护，操作前需告知。固定路径切换可断连，出口检测失败/未知/不一致保持阻断。
- 访问授权适用于当前 Windows 用户，不提供同用户进程隔离。云端 Agent 无法直接访问 localhost，必须有本机可信 MCP host。不得自动上传含出口地址、站点和路由信息的读取结果。

### 4.17 桌面控制中心（0.5）

- 控制中心使用稳定、无版本号的 `AgentEgress.Controller` 进程/窗口标识，并在显示窗口前设置，覆盖启动宿主可能带来的分组身份。看板快捷方式使用相同 ID 和控制中心图标；托盘启动快捷方式使用 `AgentEgress.Tray`。任务栏重开通过同目录 `aec-tray.exe --dashboard` 取得新会话，不把页面令牌或 localhost URL 写进 Shell 属性或快捷方式。身份更新不涉及代理、时区、路由、凭据或网络进程。
- 托盘负责网络状态、恢复与快捷操作；桌面控制中心是出口设置、分流规则/站点、固定出口、Agent/MCP 和帮助内容的统一窗口。关闭窗口不退出托盘，托盘菜单可再次打开并聚焦对应页面。
- 控制中心复用已验证的本机设置/看板页面和串行 Worker，不复制领域逻辑。WebView2 仅访问本机随机端口与启动期令牌保护的页面；服务仍只监听 IPv4 loopback，不开放 LAN。窗口状态通过既有只读 `/state` 更新。
- WebView2 Runtime 缺失时提示用户手动安装；不自动下载运行时。应用仍提供便携 ZIP 和 per-user NSIS 安装 EXE，安装 EXE 不需要管理员权限，登录自启默认关闭。

### 4.18 用户主动更新（0.5）

- 用户从托盘、看板或 Agent 限权更新工具显式启动更新器；不在后台轮询、不静默下载。稳定的日常/Agent 模式允许检查，操作中或恢复未完成时拒绝启动。
- 更新器读取项目 GitHub 最新稳定 Release，下载完整便携 ZIP 与 SHA256 sidecar 并校验。0.5 不提供二进制差分包，因此网络下载量仍是完整发布包大小；更新目标是原位替换应用文件并保留配置。
- 仅覆盖显式白名单文件。严禁覆盖 `data\` 和用户自带 GeoSite/GeoIP 数据；Credential Manager 凭据不在文件更新范围。旧文件先备份至 `.aec-update-backup\`，复制失败时恢复原文件；托盘通过正常关闭路径退出，不强杀进程。
- 新包需具有预期结构、版本目录和托盘程序，ZIP 路径越界、未知文件、超限文件或 SHA 不匹配都拒绝安装。
- 0.5.1 核验包内 SHA 清单，按文件哈希区分界面/文档和核心变更。程序、relay、内核、更新器变更在下载核验后另行显示停服确认；拒绝不能停止或写入。正常退出后必须核对日常状态、恢复日志为空且本应用内核/relay 已停止，失败即中止。
- 兼容独立界面资源采用不可变修订版及原子活动指针，可在服务持续运行时切换并回滚。仅界面更新不得替换二进制；旧 0.5.0 必须完整升级一次。刷新只重载界面，丢弃草稿前提示，不自动刷新。后端接口/占位符契约改变需提升资源协议并完整升级。
- 本机页面最大 32 个并发请求，慢修改不能阻塞导航；网络修改仍由串行 Worker 执行。Agent 访问标志即时原子更新，关闭拒绝新请求，已接受请求可继续完成。Agent 更新工具只启动固定更新器，不能绕过物理确认或声称已经安装。
- 控制中心受保护站点可折叠、搜索、限高滚动；规则支持出站类型、匹配类型、搜索和分页，保存包含所有页并保持全局顺序。来源、服务器列表、检测目标折叠，主要操作与 Agent 接入优先。原生托盘/窗口/EXE 与页面共用盾牌路径图标。
- 资源协议为 2；页面固定引用自身修订版 CSS/JS/图标，旧资源保留供已打开页面使用。停止核心失败仍提供恢复入口，不复制文件或强杀；程序文件回滚不覆盖用户档案。

### 4.19 无窗口诊断

`aec-agent diagnose` 最长等待十秒，超时返回 unknown。只读当前档案、保存状态与已启用的本机状态接口；不创建目录、不开窗口、不变更代理/时区、不访问外网或调用窗口自动化。输出仅允许名单字段，不含地址、路径、密码、令牌或原始错误。保存状态不能代替实时证据，实时状态也不等于各网站已可用；networkReadiness 一律 unknown。CLI 退出时结束诊断进程，不残留正在等待的后台诊断服务。

## 6. 不在范围内

TUN；多个 AgentProfile 的界面；机器指纹伪造或随机化；0–100 总分；可视化问答；自动发送提示词或调用外部 Agent/API；MSI 安装包；代码签名；局域网、网关和 DNS 服务；macOS 和 Linux；自动调用 ip.net.coffee 的 `/api/`；Codex 原生问答接入；切换系统显示语言；渠道切换、模型路由、Cookie/STATE、住宅池等实验区功能。

## 7. 其他说明

- legacy 迁移（ADR-0006）与本规格的实现互不阻塞。新产品替换 legacy 需要另写 Runbook，其中包括把 legacy `config.yaml` 和 `upstreams.txt` 里的凭据迁入凭据管理器、把 legacy 的浏览器 profile 迁入本应用数据目录。
- 已决定（2026-10-10）：WebRTC 限制参数默认不加；`expected.ip` 精确匹配；开机自启默认关闭（按 4.10，可在设置里开启）。
- 待实测：WinINet 每连接选项与注册表写法的差异；标准用户改时区的权限；Edge/Chrome 在同一 user-data-dir 下启动参数不一致时的行为（legacy 已有结论，新产品需复测）；ipwho.is 和 api.ip.sb 的频率限制和条款。
