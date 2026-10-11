# 更新记录

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

## [0.1.0] - 2026-10-11

第一个可用的预览版：Windows 便携 zip。

### 新增

- 托盘程序 `aec-tray`：
  - 圆点颜色表示当前模式（灰 = 日常，绿 = 检测通过，黄 = 未通过或进行中，红 = 恢复未完成）；
  - 菜单可以进入/退出 Agent 模式、打开 Edge / Chrome / Claude Code / Codex、重新检测、查看检测结果、复制排查提示词；
  - 单实例运行；注销或关机前自动恢复。
- 本机设置页（只监听 127.0.0.1，带令牌）：
  - 每个用户填自己的出口地址、端口、协议和账号；
  - 密码存进 Windows 凭据管理器；
  - 可选 13 个目标时区和 7 种浏览器语言。
- 环境检测，硬门槛全部通过才算就绪：
  - G1 出口与直连不同；
  - G2 目标站点可达且出口一致；
  - G3 DNS 由内核代办；
  - G4 系统和浏览器时区；
  - G5 浏览器语言。

  另有只读信号：ASN、延迟、WebRTC、Canvas/WebGL、Cookie、非目标流量出口、时区由谁设置。
- 出口 relay（Rust）：多个端口作为同一出口的候选池，自动切换、冷却、并发上限和拨号预算，失败如实报告。
- 系统时区真实写入：只有在设置里允许、并且没有其他程序在改同一项时才写；退出时恢复并读回确认。
- 排查提示词：每个失败或未知项生成一段固定格式、已去掉密码的说明，可以粘贴给任意 AI 助手。

### 已知限制

- 只支持 Windows 10/11；可执行文件未签名，首次运行会出现 SmartScreen 提示。
- 不修改系统代理：只有从托盘启动的浏览器和 CLI 走出口。
- 时区真实写入和真实出口端到端还没有在维护者以外的机器上验证过。
- 没有开机自启，没有安装包；通知来源显示为 `aec-tray.exe`。

[未发布]: https://github.com/zxs4KHf/agent-egress-controller/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/zxs4KHf/agent-egress-controller/releases/tag/v0.1.0
