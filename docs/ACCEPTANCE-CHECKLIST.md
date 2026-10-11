# 前期阶段验收清单

> 文中的 **legacy** 指本项目的前身：维护者此前在 Windows 上自用的一套出口链路（mihomo + Python relay + PowerShell 脚本 + 托盘）。它不随本仓库发布，只作为行为参照和「外部写入者」探测的样本出现。

当前阶段是「范围与架构定义完成」，不是「可运行版本完成」。本清单只验收边界、决策和隔离，不把文档存在误认为功能已经实现。

## 用户逐项确认（2026-10-10）

- [x] 产品定义为 Windows 本机 Agent 网络与环境控制器，不是软路由。
- [x] ~~旧项目和正在运行的服务保持不变。~~ 已修订：开发期间旧目录仍只读；另写可执行的迁移 Runbook，由 agent 在用户确认后把整体依赖迁入唯一工作目录，旧目录文件不改不删。见 [ADR-0006](adr/0006-legacy-migration-runbook.md)。
- [x] 新项目唯一工作目录是 本仓库。
- [x] 第一版包含系统代理、Agent/浏览器启动入口、出口/DNS/连通性验收、语言/时区、独立浏览器 profile、托盘和退出恢复。
- [x] 第一版不包含 TUN、机器指纹伪造、降智、API 渠道、模型路由、Cookie/STATE 探针或住宅池实验。
- [x] 关键验收失败或未知时，不显示「Agent 模式已就绪」。
- [x] AgentProfile/DailyProfile 与进入前环境快照是恢复依据；退出时逐项恢复和验证。
- [x] Canvas/WebGL/Cookie/WebRTC 只读展示或警告，不生成检测网站式 0–100 总分。补充：能正式引用的检测接口就自动调用，不能的给手动测试链接，见 [ADR-0007](adr/0007-detection-service-sources.md)。
- [x] ~~缺失环境使用可视化 Grill 问答和可复制提示词。~~ 已修订：不做可视化问答；每个失败或未知项附可复制的 agent 排查提示词，生成与发送分离。见 [ADR-0005](adr/0005-troubleshooting-prompts-replace-visual-grill.md)。
- [x] 技术路线是复用 mihomo/relay，使用 Tauri 做窗口与托盘，先做安装包和便携包，TUN 后置。

## 同日追加的决策

- [x] 系统代理和时区写入走适配器，开发期默认 dry-run，真实写入受接管门控制（[ADR-0004](adr/0004-global-state-adapter-takeover-gate.md)）。
- [x] 首次配置用设置页表单，不用问答。
- [x] relay 在新产品中用 Rust 重写；legacy 迁移阶段原样使用 Python 版。
- [x] 上游凭据存 Windows 凭据管理器/DPAPI；legacy 阶段沿用 config.yaml，放在 gitignore 范围内。
- [x] mihomo：legacy 复制旧二进制并记录哈希；新产品固定官方 release 版本并校验，下载前先征得用户同意。
- [x] 第一版只有 1 个 AgentProfile 和 1 个 DailyProfile，schema 预留多个。
- [x] 快照：当前 1 份，加只读历史 10 份。
- [x] 浏览器：Edge 和 Chrome，优先用本机已安装的版本，不自行下载；需要下载时先征得用户授权。
- [x] 安装包：NSIS `.exe` 加便携 zip；MSI 以后再说。

## 之后

这份清单记录的是 2026-10-10 前期阶段的范围决策，作为历史保留。之后的进展见 [CHANGELOG](../CHANGELOG.md) 和 [路线图](ROADMAP.md)。
