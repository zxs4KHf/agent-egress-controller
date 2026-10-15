# ADR-0015：控制中心使用独立 Windows Shell 身份

- 状态：Accepted
- 日期：2026-10-15

## 证据与问题

用户截图显示控制中心与 ChatGPT 共用任务栏分组。旧控制中心虽设置了窗口/EXE 图标，但没有声明独立 AppUserModelID，桌面看板快捷方式也未写该字段。真实窗口属性检查 `packaging/test-shell-identity.ps1 -ExistingWindow -ControlExe <旧程序>` 在修复前失败：没有 `AgentEgress.Controller` 标识。此证据不能单独区分 Windows 的隐式分组启发式和打包宿主身份继承。

## 决策

控制中心在展示 UI 前设置进程标识，再设置窗口级 `AgentEgress.Controller`，确保窗口覆盖宿主身份。标识不含版本号，升级保留分组。窗口配置自己的图标、显示名和无令牌重开命令；只有全部成功才显示。看板 .lnk 使用同一 ID；只启动托盘的 .lnk 使用 `AgentEgress.Tray`，避免任务栏误选一个只启动托盘而不打开看板的入口。

快捷方式通过 Windows 原生 IShellLinkW/IPropertyStore 写入，与安装包共用实现。先核对整批入口的目标与参数，再写入；不同安装、自定义参数或文件符号链接拒绝覆盖。维护命令 `aec-control.exe --create-shortcuts [all|desktop|start-menu]` 只创建/修复当前安装入口，不启动网络 Worker、不读出口密码。

任务栏重开固定为同目录 `aec-tray.exe --dashboard`。不把启动时的 loopback URL/token 保存到 Shell 属性或固定项中。只有控制中心文件变化时，维护者可备份后正常关闭并替换该窗口程序；不停止托盘、relay 或 mihomo。通用更新器仍按原核心更新规则要求停服确认，本次不新增自动二进制热更新能力。

## 否决的方案

- 只换 ICO：图标位图不决定任务栏分组。
- 只设置进程 ID：明确的窗口级覆盖更稳妥，不能依赖启动宿主的身份行为。
- 只修改当前快捷方式：从 Agent、托盘或其他启动器进入时仍可能合组。
- 保存控制中心原始命令行：其中包含短期认证令牌，过期后无法重开，也不应该持久化到快捷方式。
- 重启网络服务、改分流或清理整个任务栏缓存：与 Shell 身份无关，影响范围过大。

## 验证与边界

原生临时 .lnk 测试覆盖中文路径、ID/图标/参数读回、重复修复、拒绝覆盖另一安装。真实控制中心测试读取窗口属性、图标来源和无令牌重开命令，CI 运行窗口后正常关闭自己的进程。安装包编译继续走 Windows CI。网络状态不从图标推断，实机维护前后独立核对原网络 PID、档案和路由。

官方依据：[AppUserModelIDs](https://learn.microsoft.com/en-us/windows/win32/shell/appids)、[System.AppUserModel.ID](https://learn.microsoft.com/en-us/windows/win32/properties/props-system-appusermodel-id)。
