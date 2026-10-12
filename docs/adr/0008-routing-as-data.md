# ADR-0008：路由表改为数据，模板退为预设

- 状态：Proposed
- 日期：2026-10-12
- 相关：SPEC 4.3、4.13；ADR-0003、ADR-0006

## 背景

`routing` 现在只有三个字段（`crates/aec-core/src/profile.rs` 的 `Routing`）：`targetGroup`、`rulesTemplate`、`fallback`。生成内核配置的 `rules()`（`crates/aec-core/src/core/config.rs`）只认两个模板，能产出的形状是固定的：一个 `select` 组，成员只有 `relay`；可选一个单节点兜底出站；若干条 `DOMAIN-SUFFIX`；一条 `MATCH`。

一张实际在用的分流表（例如前身链路的那张，见 ADR-0006）通常需要的形状：

| 项 | 典型需求 |
| --- | --- |
| proxy-group | 多个，类型含 `fallback` 与 `select` |
| 组成员 | 1–7 个，成员可以是出站名，也可以是 `DIRECT` |
| 出站 | 多个：带凭据的 http、指向本地转发层的 socks5、加密隧道出站、用户自己的日常代理 |
| 匹配类型 | `DOMAIN-SUFFIX`、`DOMAIN`、`MATCH` |
| 规则顺序 | 首匹配即语义，注释写明每条为什么归这个口 |
| 组健康检查 | `url`、`interval`、`tolerance` 逐组不同 |
| 可切换组 | 某些站点的出口由托盘在两个成员之间切换 |

前身链路的表有约 70 条规则、4 个组。两个模板装不下这些。若按现有写法加一个专用模板，等于把一张表连同注释里的实测结论硬编码进 Rust 源码：以后动一个域名就要改代码、重新发版。

## 决策

1. **`routing` 从模板名改为数据。** 字段变为 `outbounds`、`groups`、`rules`、`final`。
2. **模板退为预设。** `default-v1`、`split-v1` 保留，只用于生成初始数据（首次运行、设置页「套用预设」）。运行路径、校验、配置生成器都不再出现模板名；`rulesTemplate` 字段及其枚举校验删除。
3. **出站模型扩展为具名列表。** `kind ∈ { http, socks5 }`，凭据一律走 `credentialRef`。保留名三个：`relay`（由 `egress.candidates` 生成）、`DIRECT`、`REJECT`。加密隧道不在这里表达，见 ADR-0009。
4. **组模型**：`{ name, type: select | fallback | url-test, check: { url, intervalSec, toleranceMs }, members: [], switchable: bool, selected: Option<String> }`。
5. **规则模型**：`{ kind, value, target, note }`，`kind ∈ { domain, domain_suffix, domain_keyword, ip_cidr }`。`note` 渲染成生成配置里的注释行——一张分流表一半的价值在注释记录的实测结论，丢掉就只剩一张域名清单。
6. **兜底不再强制非 DIRECT。** 删除 `FallbackUpstream` 文档里「拒绝直接连接」的约束，改由规则数据自己决定：很多人的日常组就是「日常代理，不通时 DIRECT」。
7. **导入而不是手抄。** 新增 `aec-dev profile import <config.yaml>`：用 YAML 库解析 mihomo/Clash 配置，出站、组、规则逐条搬运，凭据写进 Windows 凭据管理器，档案里只留 `credentialRef`。
8. **可切换组走内核控制口。** aec-core 增加最小的控制口客户端（目前只有 `GET /version`）；托盘按 `switchable` 生成菜单项，切换用 `PUT /proxies/<group>`，结果写回档案的 `selected`。

## 后果

- 删除 `FALLBACK_PROXY` 常量，以及 `ConfigError::FallbackMissing`、`ConfigError::NameCollision` 两个分支。
- 新增校验：出站与组共用一套命名空间，名称唯一；`final` 必须存在；每条规则的 `target` 必须引用存在的出站、组或保留名；被引用的组成员非空。
- 兜底出站需要凭据时，凭据会进内核配置（内核自己拨号），靠 `redact()` 在展示和日志里遮盖。这与候选池不同：候选池仍然只出现在 relay 的配置里，不进内核配置（SPEC 4.13）。
- SPEC 4.3 的 `routing` 形状与本 ADR 冲突，以本 ADR 为准，实现时同步修订 SPEC。
- 与 ADR-0003 一致：不引入 TUN。

### 验收：与源表格的等价性

导入完成后逐项比对，由脚本产出结论：

1. 出站名称集合一致；
2. 组名、类型、成员顺序、健康检查参数一致；
3. 规则条数、顺序与每条 `(kind, value, target)` 三元组一致；
4. `aec-mihomo -t -d <生成目录>` 退出码 0；
5. 经 aec 的内核请求目标站 `/cdn-cgi/trace` 的出口 IP，与源配置取得的出口 IP 相同。

### 落地顺序

1. `profile.rs`：数据模型、校验、单元测试；
2. `core/config.rs`：渲染，以及逐条比对的 fixture 测试；
3. 导入器与凭据迁移；
4. `live.rs`：预检改为探测兜底组的成员；
5. 控制口客户端与托盘可切换组。

### 未决

- 日常组除 `DIRECT` 之外是否预留一个由用户填写的成员。
- DNS 模式：保持 aec 的 `redir-host`。不开 TUN 时 `fake-ip` 没有意义。
- 改系统代理属于全局状态，归 ADR-0004 的接管门管，不在本 ADR 范围内。
