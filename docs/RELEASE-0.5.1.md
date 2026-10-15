# 0.5.1 预览版验证记录

日期：2026-10-15。内容包括 Fluent 控制中心、Windows 图标、资源热更新与核心停服确认、托盘/面板/Agent 统一快捷路由、认证优先 relay、无窗口诊断。

## 本机独立验证

- **[已验证]** `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings` 通过。
- **[已验证]** `cargo test -- --include-ignored`：227 项通过，0 失败、0 忽略。包括 pinned mihomo 真实 loopback 链、空闲 null 连接、保护阻断/重载、子域例外保护、普通出口独立工作与切回固定保护；测试未接管维护者系统代理或停止生产服务。
- **[已验证]** `powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging/portable/aec-update.ps1 -SelfTest` 通过。覆盖停服确认/取消/顺序、停止失败恢复、UI-only 不停服、SHA 清单、路径/data 保护、原子指针与回滚、协议不兼容拒绝。
- **[已验证]** `cargo build -p aec-core --example ui_preview --locked` 后使用本机 Node/Playwright 执行 `node packaging/test-ui.cjs` 通过。真实页面服务、隔离 fake backend、已安装 Edge：导航、规则筛选分页/全量保存、草稿保护、站点搜索折叠、Agent 双向开关和徽标、快捷出口往返、运行中资源切换不重启进程、旧页资源保留。
- **[已验证]** 新 `aec-agent diagnose` 对现有运行服务只读调用约 282ms 返回；未输出密码/地址。实时接口有响应，但 `networkReadiness` 仍为 unknown，未把它当作联网恢复证明。

## 独立 CI 与发布门槛

GitHub Windows 工作流执行 fmt/clippy/test、更新器 SelfTest、Edge 页面测试、NSIS 编译冒烟，以及校验 pinned mihomo 后的全部真实内核测试，再构建便携 ZIP、安装 EXE 与 SHA256 sidecar。具体运行结果以 PR/Actions 记录为准，未通过不得合并 main 或发布候选。

首轮独立 CI 发现真实重载在 Windows TEMP 短路径下返回 HTTP 400。本机将同一 fixture 改用 8.3 路径后稳定复现；原因是只 canonicalize 重载文件名，启动 `-d` 仍为短路径，触发内核的字符串路径安全校验。改用已核实归属的内核启动配置路径，并按 pinned API 把 force 放入 query，不禁用安全检查。参考 [mihomo v1.19.31 配置接口](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/hub/route/configs.go)与[路径校验](https://github.com/MetaCubeX/mihomo/blob/v1.19.31/constant/path.go)。保留真实短路径重载回归，不通过放宽测试或扩大超时绕过。

## 限制与未完成证明

- Issue #4 外部窗口工具挂起根因未确认；本版增加无窗口有界诊断，不宣称修复 VPN 或窗口自动化组件。
- 安装/卸载和登录自启的人工实机验收、WebView2 不同运行时与 Windows 图标缓存仍需用户验证；编译和页面测试不代替这些结果。
- 未证明所有供应商路径、所有用户网络或完整 GPT/Claude 登录会话都可用。精确 ISP 验证失败时仍阻断受保护流量。
- 从 0.5.0 升级需要先完整更新；核心替换须用户单独确认。没有得到确认前，本机仍运行旧服务，新图标及托盘逻辑不会自动加载。
- 增量是按改变文件替换，下载仍是完整 ZIP；不是二进制差分下载。

## 回滚

代码使用 revert 变更提交回滚，不重写 main 历史。更新器保留 `.aec-update-backup` 和 rollback.json；先正常退出并确认恢复，再恢复程序文件。保留原 `data`、用户 Geo 文件和 Credential Manager；档案不引入旧版无法读取的新字段。仅界面回滚恢复旧活动指针，不停止网络。

切换到非固定出口本身是用户配置变更，不会因二进制回滚自动撤销。需要恢复原路由时使用明确备份并核对范围；不得为了回滚换掉用户预期 ISP IP。临时诊断文件与维护者运行数据不随包分发。
