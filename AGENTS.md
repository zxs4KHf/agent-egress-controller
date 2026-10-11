# 给 AI Agent 的速查

改这个仓库之前先读完这一页。完整流程见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 这个项目是什么

Windows 本机的 Agent 出口控制器：只让从托盘启动的浏览器和 CLI 走用户自己配置的出口代理，把系统时区调成和出口一致，逐项检测是否生效，退出时恢复原样。术语以 [CONTEXT.md](CONTEXT.md) 为准，行为以 [docs/SPEC.md](docs/SPEC.md) 为准，取舍记录在 [docs/adr/](docs/adr/)。

## 代码在哪

| 目录 | 内容 |
| --- | --- |
| `crates/aec-core` | 领域模型、状态机（`controller.rs`）、检测（`checks.rs`）、relay、设置页、Windows 适配器 |
| `crates/aec-tray` | 原生 Win32 托盘（用户实际运行的程序） |
| `crates/aec-dev` | 开发命令行 `aec-dev` 和 relay 进程 `aec-relay` |
| `packaging/portable` | 便携包附带的说明 |

## 硬规则

1. **不碰用户的全局状态，除非经过接管门。** 系统代理和时区的真实写入只能经 `Platform::write_global`，并且只在接管门打开时调用。测试一律用 `FakePlatform` / `FakeHost`。
2. **不伪造通过。** 检测拿不到证据就是 `unknown`，不是 `pass`；不做 0–100 总分；不做指纹伪造；不用 TUN。
3. **凭据只进 Windows 凭据管理器。** 档案里只有 `credentialRef`；日志、提示词、页面、错误信息里不出现密码。新增输出时检查 `prompts::redact`。
4. **不结束不是本应用启动的进程。** 关闭只对自己启动、且身份核对一致的进程发 WM_CLOSE，不强杀。
5. **不自动发送任何东西。** 排查提示词只复制到剪贴板。
6. **不下载。** 浏览器用本机已装的 Edge/Chrome；mihomo 版本和 SHA256 固定在 `core/pin.rs`。
7. **托盘线程不在借用 UI 状态时调用会派发消息的 API**（菜单、MessageBox、ShellExecute），见 `aec-tray/src/main.rs` 的 `Effect`。

## 提交前必须通过

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

需要真实 mihomo 的测试默认忽略：把 mihomo v1.19.31 放到 `runtime/bin/aec-mihomo.exe` 后用 `cargo test -- --include-ignored`。

## 汇报方式

PR 和汇报里区分 **[已验证]**（真跑过，附命令和输出）和 **[推断]**（没跑过）。修 bug 先写能复现的测试。
