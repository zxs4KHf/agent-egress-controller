# ADR-0009：复用内核的加密第一跳与官方公开隧道预设

- 状态：Accepted，0.2.0 已实现
- 日期：2026-10-12

HTTP/SOCKS5 出口在某些网络中无法直接连接，需要先通过加密中转访问出口。复用固定版本 mihomo 的 Shadowsocks 实现，不在 Rust relay 中再实现加密协议，也不引入 TUN；用户自己的出口账号继续由 relay 使用，中转不会替换最终出口。

`egress.tunnel` 保存启用状态、服务器列表、AEAD cipher、`credentialRef`、直连回退开关和单步超时。内核在 loopback 独立端口提供中转入口；relay 先经入口访问同一个出口候选池，再按用户选择尝试直接访问相同候选。启用直连回退时，第一半拨号预算给隧道，后半留给直连，避免无响应入口吞掉所有预算。全部失败时如实失败，不把目标站偷偷放成 DIRECT。

通用表的导入另外允许具名 `kind: ss` 传输出站，以及 HTTP/SOCKS5/SS 的 `dialerProxy` 引用。它们参与同一组命名空间和环检测，由内核拨号；SS 必须有支持的 AEAD cipher 与凭据引用，UDP 关闭。这是对 ADR-0008 原先 HTTP/SOCKS5 出站模型的明确扩展，使源表的节点名称与分组保持完整。

IPRoyal 官方 Shadowrocket 指南公开了 tt01–tt25.virtual-liberator.com、46472/46473、chacha20-ietf-poly1305 与共享密码。按用户要求提供可选预设（50 个端口组合），该共享值属于供应商公开参数，不是用户私人凭据；源码可以包含预设，设置保存后仍使用 Windows 凭据管理器引用，页面和排查输出不显示密码。官方说明限定配合 IPRoyal 出口使用，并要求关闭 UDP。用户出口账号、密码绝不随预设发布。

来源：[IPRoyal 官方指南](https://iproyal.com/integrations/how-to-use-iproyal-proxies-with-shadowrocket/)，文档核对日期 2026-10-12。核对官方文档不等于验证每个节点实时可用；实际中转与最终出口由本机检测验收，用户可改用自定义隧道。

否掉的备选：把供应商的完整私人路由表硬编码成模板，会发布私人配置并让每次改域名都要求发版；删除公开隧道预设则无法满足用户指定的官方中转路径；在 Rust 自行实现 Shadowsocks 会重复已有实现并增加安全维护成本；用 TUN 会扩大系统影响范围并违反 ADR-0003。
