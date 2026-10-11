# 成熟方案调研

> 文中的 **legacy** 指本项目的前身：维护者此前在 Windows 上自用的一套出口链路（mihomo + Python relay + PowerShell 脚本 + 托盘）。它不随本仓库发布，只作为行为参照和「外部写入者」探测的样本出现。

调研目标是判断哪些能力值得复用、哪些应该只借鉴，以及本项目是否有必要继续做。

## 结论

本项目仍有必要做，但产品价值在“把本机 Agent 工作环境的切换、验收、恢复和提示词引导整合成低维护控制面”，不在重新发明代理内核。最稳妥的路线是复用 mihomo，使用 Tauri 做 Windows 桌面壳，把现有脚本改成受控、可测试的服务层；第一版保持系统代理和显式启动入口，TUN 以后再做。

## 对比

| 方案 | 成熟能力 | 可借鉴内容 | 对本项目的结论 |
| --- | --- | --- | --- |
| Clash Verge Rev | Tauri 桌面客户端、mihomo 内核、系统代理、TUN、托盘、profile、IP 信息卡、更新和隐私说明 | profile/节点/状态组织、托盘交互、隐私披露、失败状态 | 最接近的产品参照；不直接把完整客户端塞进本项目，因为本项目还需要模式快照、恢复和 Agent 引导 |
| mihomo | 代理端口、规则路由、DNS、TUN、监听器、多协议出站 | 作为网络内核、DNS 和未来 TUN 的能力边界 | 继续复用；不要自研代理协议、DNS 或 TUN 内核 |
| Proxifier | Windows 透明代理、按进程和主机规则、代理链、DNS 通过代理、profile、实时连接和错误报告 | per-app 规则、profile、连接可观测性、恢复和错误报告 | 用作应用作用域和诊断的能力参照；其透明拦截是商业产品能力，第一版不复制 |
| Tauri | Windows WebView2 桌面应用、系统托盘、NSIS/MSI 打包、代码签名流程 | 托盘窗口、安装包、自动更新和签名边界 | 作为新项目 UI/壳；Rust 控制层只暴露受控命令，不让前端直接改系统设置 |

## 关键证据

- Clash Verge Rev 的官方仓库说明它是基于 Tauri 的跨平台 GUI 客户端，支持系统代理和 TUN；其隐私说明还明确记录 mihomo 负责代理和 DNS，IP 卡会调用可配置的外部服务，并提供自动更新/日志控制。[官方仓库](https://github.com/clash-verge-rev/clash-verge-rev) · [隐私说明](https://github.com/clash-verge-rev/clash-verge-rev/blob/dev/PRIVACY.md)
- mihomo 官方文档把普通代理端口、TUN 和监听器分开；TUN 用于系统流量捕获、自动路由、DNS 劫持和按应用路由，适合高级场景。[入站配置](https://wiki.metacubex.one/en/config/inbound/) · [TUN](https://wiki.metacubex.one/en/config/inbound/tun/) · [DNS](https://wiki.metacubex.one/en/config/dns/)
- Proxifier 官方文档支持按应用/主机规则、代理链、通过代理解析 DNS、profile 和错误报告，说明“应用级代理控制”已经有成熟产品范式。[功能说明](https://www.proxifier.com/docs/win-v3/) · [profile](https://proxifier.com/docs/win-v4/profiles.html)
- Tauri 官方文档支持 Windows WebView2、托盘、NSIS `.exe` 安装器和 WiX `.msi` 安装器，并单独说明代码签名与 SmartScreen 声誉问题。[Tauri 入门](https://tauri.app/start/) · [托盘 API](https://tauri.app/reference/javascript/api/namespacetray/) · [Windows 安装器](https://tauri.app/distribute/windows-installer/) · [Windows 签名](https://tauri.app/distribute/sign/windows/)

## 检测服务调研（2026-10-10）

对象是 `https://ip.net.coffee/`。方法：下载首页、`/claude/`、`/dns/`、`/webrtc/`、`/ip/` 的 HTML 和 JS，提取请求地址，再用公网测试 IP `1.1.1.1` 实测接口。

| 接口 | 实测 | 是否公开说明 | 结论 |
| --- | --- | --- | --- |
| `ip.net.coffee/api/geoip/{ip}`、`/api/iprisk/{ip}` | 200，JSON，CORS `*`；iprisk 含 ASN、是否机房/代理等字段 | 否；`robots.txt` 写有 `Disallow: /api/` | 不自动调用，站点页面作为手动链接 |
| `ip.net.coffee/api/dns/result/{token}` + `{token}-{i}.d.ip.net.coffee` | 依赖站点自建权威 DNS | 否 | 不自动调用；DNS 路径用本地证据判定 |
| `https://1.1.1.1/cdn-cgi/trace` | 200，`ip=`、`loc=`、`colo=` | Cloudflare 公开的诊断端点 | 自动调用，用于出口 IP 和地区 |
| `https://claude.ai/cdn-cgi/trace`、`https://www.anthropic.com/cdn-cgi/trace` | 站点前端在用 | 同上；属于目标站自身 | 自动调用：一次请求同时验证目标可达和目标看到的出口 IP |
| `https://api.ip.sb/geoip/{ip}` | 200，含 `asn`、`asn_organization`、`country_code` | 公开免费 API | 自动调用，用于 ASN 和地区复核 |
| `https://ipwho.is/{ip}` | 200，含 `connection.asn`、`country_code` | 公开免费 API（有频率限制） | 自动调用，作为第二来源 |

未验证：`ipwho.is` 和 `api.ip.sb` 的具体频率限制和服务条款，需要在实现检测服务契约时查阅并写入配置注释。

## 是否值得做

值得做的部分：

1. 把已有的固定出口链路变成独立、可恢复、可验收的模式档案。
2. 解决“代理实际生效了吗、DNS 是否走对、Agent 是否处于目标语言/时区”的可见性问题。
3. 提供低维护的托盘入口、退出恢复和配置提示词。
4. 将现有实验内容从核心构建中移除，降低维护和误操作风险。

不值得在第一版做的部分：

1. 自研代理内核、TUN、DNS 或透明拦截。
2. 机器指纹伪造、账号风控、模型质量和 API 渠道切换。
3. 复制一个完整的 Clash 客户端功能面。
4. 用一个无法解释的信任分数替代可验证的分项证据。

## 主要风险

- 系统代理、环境变量、时区和浏览器进程都可能被其他软件改变，因此必须保存快照并做恢复验收。
- Windows TUN、DNS 劫持和防火墙规则具有更高权限和副作用，第一版后置是为了缩小故障半径；mihomo 文档也把 TUN 作为高级配置并说明其对路由、防火墙和 DNS 的影响。
- 外部 IP/ASN 服务可能变更或不可达，检测契约需要多服务、超时、来源和未知态。
- Tauri 安装器可生成 `.exe`/`.msi`，但 Windows 发布还涉及 WebView2、签名和 SmartScreen 声誉，不能把“打包成功”当作“用户可顺利安装”。
