# ADR-0018：启动链路不继承打包 Agent 的桌面身份

- 状态：Accepted
- 日期：2026-10-15
- 补充并修正：ADR-0015

## 问题与证据

0.5.4 的窗口 AppUserModelID、图标和快捷方式属性全部正确，用户仍看到 AEC 与 ChatGPT/Codex 合组。只读原生检查发现，实际控制中心进程的 `GetPackageFullName` 仍返回 Codex 包身份。最小子进程 fixture 未处理时继承该身份；指定 desktop-app policy 后返回 `APPMODEL_ERROR_NO_PACKAGE`。窗口属性正确不能取代包身份检查。

## 决策与理由

1. 托盘、控制中心在创建窗口、单例和 Worker 前查询包身份。已是 unpackaged 则原样运行；继承包身份则用 `CreateProcessW`、`PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY` 与 `PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE` 重开同一 EXE。子进程再次验证；未脱离则中止，不进入网络控制。该策略只调整包身份继承，不提权，不改变访问令牌、Job 或安全限制。
2. 保留原始 Windows 命令行，追加内部循环保护标记；业务参数过滤该标记。原父进程不展示 UI、不运行 Worker，只等待并传回退出码。理由：0.5.1/0.5.2 更新器把新托盘两秒内退出视为启动失败，立即退出启动器会引发错误回滚；维护命令也需要真实退出码。
3. PowerShell 更新脚本使用同一 Windows 策略，自包含实现，避免旧版本一次性引导要求下载新辅助 EXE。弹窗用真正的 WinForms 窗口、`AgentEgress.Updater` 和 AEC 图标；无外置 ICO 时内置绘制盾牌。默认按钮为取消，关闭窗口不授权停服；禁止固定瞬时更新窗口，避免记录过期 PID/参数。
4. 控制中心单例和窗口查找限定当前安装的实际 EXE，避免隔离验收或第二份安装聚焦/关闭生产窗口。网络托盘的全局单例保持不变，不允许运行第二套网关。
5. 正常更新等待已退出子进程的同安装启动器自然结束，再替换文件；不强杀。保留既有停服确认、恢复检查、增量校验和回滚契约。

## 否决的方案

- 只重写窗口 ID/ICO/快捷方式：0.5.4 的这些属性已正确，仍可复现。
- 清理整个任务栏缓存或修改 ChatGPT：影响其他应用，不能修复子进程的包身份继承。
- 让用户必须从 Explorer 手动启动：不满足 Agent 托管和一键更新入口。
- 启动后立即结束父进程：破坏旧更新器监督语义和维护命令退出码。
- 新增提权服务、TUN、修改全局代理：与身份问题无关，也超出权限边界。
- 更新器强杀占用进程：违反正常关闭与身份核对约束。

## 验证与边界

`test-packaged-launch.ps1` 从实际 Agent 启动 fixture，检查包身份、中文/空格/末尾反斜杠参数和非零退出码。`test-shell-identity.ps1` 检查真实控制中心窗口的所属进程、包身份、AppID、图标和无令牌重开命令，并正常关闭测试窗口。`test-updater-shell.ps1` 检查实际更新确认窗口身份/图标，再关闭并断言未确认；更新器 SelfTest 另验证取消不调用停止/复制/重启。

维护者本机 Codex 是打包宿主，能覆盖真实继承分支；GitHub Windows runner 通常是普通宿主，因此 CI 验证兼容分支，不能把它单独写成打包宿主复现证据。任务管理器里可能仍有一个无窗口的等待父进程；应检查实际拥有窗口的子进程。测试不改变生产配置、路由、系统代理或网络进程。旧的运行中进程不会因发布而改变身份，实际升级仍须用户确认。

官方来源：[UpdateProcThreadAttribute](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)、[GetPackageFullName](https://learn.microsoft.com/en-us/windows/win32/api/appmodel/nf-appmodel-getpackagefullname)、[Windows package identity](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/package-identity-overview)。
