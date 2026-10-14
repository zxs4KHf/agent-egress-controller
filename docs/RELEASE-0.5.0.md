# 0.5.0 预览版交付记录

本版本将出口设置、规则和站点、固定出口路径、Agent/MCP 接入、代理冲突说明合并到独立桌面控制中心。系统托盘仍常驻显示模式，可恢复、复检、退出并重新打开控制中心；关闭控制中心窗口不退出托盘。

发布形态：Windows x64 per-user NSIS 安装 EXE 与便携 ZIP。安装器不提权，提供开始菜单/桌面入口和可选登录自启。桌面控制中心使用 Microsoft Edge WebView2 Runtime；未检测到时提示用户手动安装，不下载运行时。设置仍由当前用户的既有本机服务、Credential Manager 与数据目录管理。

托盘菜单增加用户主动的「检查并安装更新」。从 GitHub 最新稳定 Release 获取完整便携 ZIP 与 SHA256 sidecar，校验后只更新允许的程序文件，备份被替换的旧文件至 `.aec-update-backup`。保留用户 `data`、用户提供的 GeoSite/GeoIP 和 Credential Manager 凭据。不是二进制差分下载。0.5.0 之前的版本需手动升级一次；旧便携版复制程序文件到原目录可继续使用原 `data`，改为安装版时数据目录不同且不会自动迁移。

## 验证

**[已验证]** `cargo fmt --all --check` 通过。

**[已验证]** `cargo clippy --all-targets -- -D warnings` 通过。

**[已验证]** `cargo test` 通过：195 项通过，0 失败，3 项因需要真实 mihomo 而忽略；包括桌面入口 URL 验证与控制中心页面鉴权/同源 framing 测试。

**[已验证]** `cargo test -- --include-ignored` 通过：211 项通过，0 失败，0 忽略；包含真实 pinned mihomo 本地配置、loopback relay 链和固定出口阻断/重载集成测试。

**[已验证]** `powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging/portable/aec-update.ps1 -SelfTest` 通过；覆盖目录穿越拒绝、data 保留和替换失败回滚。

**[待 GitHub Windows CI 验证]** NSIS 安装器编译冒烟测试。本地环境没有安装 NSIS。

**[待实机验证]** WebView2 窗口视觉和交互、缺少 WebView2 Runtime 时的提示、登录自启选项、安装卸载流程。当前自动化只验证宿主编译、本机页面服务和更新器文件操作；未声称已完成桌面端到端 UI 验收。

## 回滚

功能代码可通过 revert commit 回滚。用户升级可从 `.aec-update-backup` 恢复被替换的程序文件；不要覆盖或删除用户 `data`。安装版和便携版数据目录独立。运行 Agent 模式时先通过托盘退出并确认系统状态恢复，再操作程序文件。不要强制结束托盘或它没有启动的进程。
