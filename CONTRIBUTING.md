# 一起开发

欢迎一起改 Agent 出口控制器。下面是改代码前需要知道的规矩；给 AI Agent 的速查版在 [AGENTS.md](AGENTS.md)。

## 准备环境

- Windows 10/11（目前只支持 Windows：托盘、凭据管理器、时区和进程管理都是 Windows 专用）。
- Rust stable（`rustup` 安装，MSVC 工具链）。
- 本机已安装 Edge 或 Chrome（浏览器入口用，测试不需要）。
- 可选：mihomo v1.19.31 的 Windows amd64 官方可执行文件，放到 `runtime/bin/aec-mihomo.exe`，用来跑端到端测试和开发版托盘。SHA256 固定在 `crates/aec-core/src/core/pin.rs`，`aec-dev core install <exe>` 会校验后再放进去。

```bash
cargo build
cargo run -p aec-tray        # 开发版托盘，数据放在 runtime/dev-data
cargo run -p aec-dev --bin aec-dev -- status
```

开发版和便携版使用独立的数据目录和端口（17xxx），不会动你本机其他代理工具。

路由数据与官方隧道的约束见 ADR-0008/0009。合成导入 fixture 在 `crates/aec-core/tests/fixtures/routing.yaml`，不把私人源表入库。发布前补跑 `cargo test -- --include-ignored`，包含真实 mihomo 的配置、控制口选择和本地全链路。私有表可用 `verify_mihomo_import` 示例进行只读结构/出口等价验收，命令与验证边界见 `docs/RELEASE-0.2.0.md`。

便携包用 `powershell -ExecutionPolicy Bypass -File packaging/build-portable.ps1` 构建。只复制明确列出的二进制与文档，校验 pinned mihomo，保留第三方许可证，生成 SHA256；不复制 data、logs、legacy 或私有对话。没有可信来源的 GeoSite/GeoIP 文件不会打包。已有版本产物不得覆盖。

## 改代码的流程

1. **从 `main` 开分支**：`fix/<主题>`、`feat/<主题>`、`docs/<主题>`、`chore/<主题>`。
2. **修 bug 先写能复现的测试。** 能用 `FakePlatform` / `FakeHost` 测的就不碰真机；必须碰真机的测试加 `#[ignore = "原因"]`。
3. **改完跑三件套**，CI 也跑同样的：
   ```bash
   cargo fmt --all --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
4. **行为变了就改文档**：用户可见的行为写进 `docs/SPEC.md`；值得记录的取舍新增一篇 `docs/adr/NNNN-*.md`；新术语加进 `CONTEXT.md`；用户能感知的变化写进 `CHANGELOG.md` 的「未发布」。
5. **提 PR**：说清改了什么、为什么、怎么验证、怎么回滚。验证里区分 **[已验证]**（真跑过，附命令和输出）和 **[推断]**（没跑过）。

## 不接受的改动

这些是产品边界，不是暂时没做（理由见 [docs/adr/](docs/adr/)）：

- 机器指纹伪造、Canvas/WebGL 噪声、0–100 的「纯净度」总分。
- TUN、虚拟网卡、改路由表或防火墙。
- 把凭据写进持久档案、日志或提示词；自动把排查内容发到网络上。内核/relay 启动所需的临时凭据配置按 SPEC 4.4/4.13 管理并在退出时清理，不得随发布包分发。
- 强制结束不是本应用启动的进程。
- 没有用户同意就下载东西。

## 报告问题

- 用 Issue 模板。托盘菜单「复制排查提示词」生成的文本已经去掉了密码，可以直接贴；如果不想公开出口 IP，贴之前把 IP 打码（或在开发命令行用 `aec-dev prompt --mask-ip`）。
- 安全问题（比如凭据可能泄露）不要开公开 Issue，按 [SECURITY.md](SECURITY.md) 私下报告。

## 许可证

提交代码即表示你同意以 [GPL-3.0](LICENSE) 授权你的贡献。
