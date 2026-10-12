# 第三方组件

发布包的 bin/aec-mihomo.exe 为 MetaCubeX mihomo v1.19.31 官方 Windows amd64 二进制，未修改，按 GPL-3.0 分发。

- [固定版本源码](https://github.com/MetaCubeX/mihomo/tree/v1.19.31)
- [官方发布](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.31)
- exe SHA256：`532ed88b3a37d72f6b5ee5d24a45ce5d8799f3ba34931d6a167d4345f7ebca50`
- GPL-3.0 全文附于 LICENSE。

控制器源码与依赖版本见本仓库 release tag 和 Cargo.lock。Rust 依赖许可：Serde、serde_json、windows-sys、indexmap、hashbrown、equivalent 为 MIT 或 Apache-2.0；ryu 为 Apache-2.0 或 BSL-1.0；serde_yaml_ng、unsafe-libyaml 为 MIT。打包附上本机编译依赖的原始许可证/版权文件。上述第三方声明不改变本项目 GPL-3.0-only 分发许可。

IPRoyal 公开中转参数来自 [官方 Shadowrocket 指南](https://iproyal.com/integrations/how-to-use-iproyal-proxies-with-shadowrocket/)，用于连接 IPRoyal 代理，不提供用户私人账号或节点可用性保证；文档核对日期 2026-10-12。

便携包不含浏览器、用户凭据、维护者配置或 GeoSite/GeoIP 文件。
