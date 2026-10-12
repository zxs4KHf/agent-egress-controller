# 0.2.0 交付与验收记录

日期：2026-10-12。环境：Windows，Rust stable MSVC，mihomo v1.19.31，固定 exe SHA256 `532ed88b3a37d72f6b5ee5d24a45ce5d8799f3ba34931d6a167d4345f7ebca50`。

本次接续工作以 ADR-0008 为目标，保留已有隧道、设置页和分流进度，完成数据模型、导入、控制 API、托盘选择和文档。私人 Claude/WorkBuddy 记录、源配置与真实凭据不随提交或 release 发布；公开 IPRoyal 参数按用户指定保留，来源见 ADR-0009。

## 已验证

- `cargo fmt --all --check`：退出码 0。
- `cargo clippy --all-targets -- -D warnings`：退出码 0。
- `cargo test`：默认测试通过，需要真实 mihomo 的测试默认忽略。
- `cargo test -- --include-ignored`：真实内核启动/停止、端口冲突隔离、内核 → relay → 失效候选 → 本地可用代理 → HTTP 全链路通过。
- 合成 fixture 验证出站名、组类型、成员顺序、健康检查参数、规则序列和注释；密码不出现在 profile JSON、Debug 或脱敏显示中。
- 新增真实 mihomo 测试：导入表通过 `-t`，PUT 选择生效，停止并重新启动后应用保存的选择成功。
- 回归测试先复现再修复：外部系统代理改写被巡检抢回；读取系统代理失败仍保留绿色；无响应隧道耗尽直连回退预算。
- 原模板档案读取不改文件，正常保存后无 rulesTemplate/targetGroup 字段；导入保存失败对全部新增凭据尝试清理，即使其中一次清理失败也不中断其他清理。
- 设置页浏览器验收（仅内存后端、合成数据）：多协议候选端口、IPRoyal 公开隧道、分流保存成功；重载保留设置、密码框为空，截图见 README。没有写真实凭据或全局设置。
- 便携包验收：78 个文件的内部 SHA256 全部匹配，不含私有/运行目录；独立解压目录的默认 data 初始化、合成路由导入 dry-run 和 verify 均通过。

## 真实源表等价性

私有源表只读验收命令的可复用形式：

```powershell
cargo run -p aec-dev --example verify_mihomo_import -- <source.yaml> <pinned-mihomo.exe> <geodata-directory> <source-proxy-port>
```

2026-10-12 的输出：

```text
[verified] 10 source outbounds preserved (+ built-in relay), 4 groups identical, 72 rules identical in order; pinned mihomo -t exited 0; no running services or global settings changed
[verified] claude.ai/cdn-cgi/trace returned the same exit through source and isolated imported core (IP withheld)
```

程序逐项比对所有源出站字段、全部分组、完整有序规则，验证生成配置；可选参数另在独立 31xxx 端口启动本应用拥有的临时内核，只请求 trace，随后停止并清理。源文件、原运行服务、系统代理、时区未修改，IP 与密码不输出。此结果验证源表与导入表的出口等价，不代表整个原私有部署已迁移，也不代表源表所依赖的外部本机服务可随包分发。

## 交付内容

便携 zip 含 aec-tray、aec-dev、aec-relay、固定 mihomo、portable.flag、使用说明、用户指南、合成路由示例、GPL 许可证、第三方声明/许可证、SHA256SUMS。按明确文件名单打包，不包含 runtime/data/logs/legacy/私有对话。源码由对应 GitHub tag 提供。

制作：`powershell -ExecutionPolicy Bypass -File packaging/build-portable.ps1`。脚本再次执行三项检查、校验 mihomo、release 编译，拒绝覆盖同版本已有包；无下载、无全局修改。

## 推断与待实机验收

- [推断] Windows 10/11 标准用户的时区权限、实际 WinINet 全局写入与退出恢复，需要在可打开接管门的用户机器验收。本轮测试用 FakePlatform/FakeHost，没有改维护者全局状态。
- IPRoyal 官方列出的 50 个端口组合仅核对公开文档，没有逐节点做当前网络可用性认证；实际入口和出口仍需 G1/G2 验收。
- 尚未在每一种桌面客户端验证其是否遵循系统代理。设置接管成功不等于某个客户端一定使用该代理。
- 原生托盘的所有交互尚未逐项人工点击验收；选择命令、控制 API 和恢复有自动化证据。
- 便携包不含 GeoSite/GeoIP，开启相关规则须提供可信文件；没有安装包、签名、自启或 Tauri 综合窗口。
- 远程 API 网关服务器部署不属于本机控制器交付。
