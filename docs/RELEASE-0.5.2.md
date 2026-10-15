# 0.5.2 发布说明与验收范围

日期：2026-10-15。修复控制中心归到 ChatGPT/Codex 任务栏分组，以及桌面、开始菜单、安装包的快捷方式身份。版本不更改网络分流、固定出口或用户配置格式。

## 交付内容

- 控制中心在显示前设置独立的进程和窗口 AppUserModelID，使用盾牌图标；任务栏重开调用同目录托盘的看板入口，不保存临时令牌。
- 看板快捷方式共用窗口身份；托盘快捷方式使用另一身份，避免误选只开托盘的入口。只修复同一安装的标准入口，拒绝覆盖其他安装、自定义参数或链接文件。
- 发布 `AgentEgressController-0.5.2-setup.exe`、`AgentEgressController-0.5.2-portable.zip`，对应 `SHA256SETUP-0.5.2.txt`、`SHA256SUMS-0.5.2.txt`。
- 发布独立 `aec-update-0.5.2.ps1` 和 `SHA256UPDATER-0.5.2.txt`，供原版 0.5.0 用户先更新脚本。0.5.1 可直接完整更新。详细操作见 [升级指南](UPDATING.md)。

## 历史修复证据

**[已验证]** 修复已通过 [PR #6](https://github.com/zxs4KHf/agent-egress-controller/pull/6) 合入主线。两次独立 Windows CI：[运行一](https://github.com/zxs4KHf/agent-egress-controller/actions/runs/38710353271)、[运行二](https://github.com/zxs4KHf/agent-egress-controller/actions/runs/38710358851)。包含 Rust 229 项测试、三轮真实窗口身份检查、原生临时快捷方式读回、更新器 SelfTest、Edge 页面交互、pinned mihomo、本地链路和 NSIS 编译打包。

**[已验证]** 发布准备时，使用原版 v0.5.0 的真实 `Get-PackageFiles` 读取含新版资源的包，复现“更新包包含未允许的文件：使用说明.txt”。原白名单同样不允许 `app.ico` 和 `ui/active.json`。因此不宣称原版 0.5.0 能直接自更新，采用用户先替换脚本的一次性引导；已发布旧版本资产保持不变。

## 本版发布门槛

**[已验证，本地]** 版本提升后的 fmt/clippy、229 项含真实内核的 Rust 测试、Windows PowerShell 5.1 更新器 SelfTest 全部通过。本地构建的 0.5.2 ZIP 已通过 `packaging/test-release-upgrade.ps1` 四组升级、数据保留和回滚测试。机器未安装 NSIS，安装 EXE 构建与新版本真实窗口/Edge 回归留给独立 CI，不用本地 ZIP 替代 CI 发布产物。

本版 Windows CI 须全部通过后才发布，结果链接附在 GitHub Release 中：

1. `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test -- --include-ignored`。
2. Windows PowerShell 5.1 更新器 `-SelfTest`；三轮 `packaging/test-shell-identity.ps1`；已安装 Edge 的 `packaging/test-ui.cjs`；NSIS 安装包编译。
3. `packaging/build-portable.ps1` 构建完整 ZIP、安装 EXE、独立更新器与校验文件。
4. `packaging/test-release-upgrade.ps1` 使用固定提交的 0.5.1 更新函数及发布的新引导脚本，对实际 ZIP 做四组隔离测试：0.5.1 直接 / 0.5.0 引导，各覆盖便携 / 安装模式。检查内外 SHA256、停服确认/取消顺序、完整文件复制、模式及合成数据保留、备份、重复无操作、回滚。只加载函数，真实停止/重启回调用测试替身，不操作维护者网络服务或凭据。
5. 发布后查询 GitHub `releases/latest`，核对版本和更新器依赖的资产名称，校验下载资产。

## 用户操作与限制

这是完整程序更新；只更新界面无法安装任务栏修复。核心更新需用户确认短暂断连，取消不停止服务。升级后在托盘点「创建桌面和开始菜单快捷方式」修复旧入口；旧 Agent Egress 固定项有缓存时由用户取消并重新固定，不清空整个任务栏或动 ChatGPT 的固定项。

**[未验证]** 编译及自动测试不代替所有 Windows 版本的安装/卸载向导、人工登录自启、SmartScreen 和任务栏缓存体验；程序仍未签名。不证明所有用户网络或 OpenAI/Claude 账号均可用。GitHub Latest 是更新发现渠道，不代表已完成这些人工验收。

更新保留原分流与固定 ISP。回滚遵循 [升级指南](UPDATING.md#备份回滚与边界)，程序文件恢复不应重置用户规则或更换预期出口 IP。公共发布包不包含维护者 data、日志、真实出口、密钥或浏览器会话。
