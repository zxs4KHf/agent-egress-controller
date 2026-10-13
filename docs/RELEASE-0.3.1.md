# 0.3.1 验收与交付边界

日期：2026-10-13。Windows x64 便携预览版，沿用 pinned mihomo v1.19.31。公开包不包含维护者数据、账号、私人路由表或运行日志。

## 变化与原因

接替旧链路时，客户端可能继续指向旧入口；停旧服务却改用另一个监听端口会导致连接失败。设置页现在提供本机入口端口，托盘、命令行与检测共用保存值。迁移准备先用隔离端口验证，停止隔离实例后记录旧客户端入口；产品默认仍为 17893，不改用户级代理环境变量。

系统代理需要单一写入者。日常代理客户端关闭系统代理开关，保留内核作为普通流量上游；AI 域名走固定 ISP，外部改写仍告警而不抢回。不合并外部的全局绕过列表，避免绕过保护规则。

`oaiusercontent.com` 根域不提供本次测试所需的可达 HTTPS 服务；检测改用实际文件服务 `files.oaiusercontent.com`。G2 显示实际 URL，G3 核对相同主机的内核证据，保护范围仍是整个根域与所有子域。来源：[OpenAI 网络建议](https://help.openai.com/en/articles/9247338-network-recommendations-for-chatgpt-errors-on-web-and-apps)。

## [已验证]

- `cargo test -p aec-dev --test core_pipe core_commands_use_the_saved_mixed_port`：修复前失败，输出 `saved port ignored by core command`；修复后通过。测试运行实际命令行配置生成路径，使用临时档案，不写系统设置。
- `cargo test -p aec-core resource_root_probes -- --nocapture`：修复前 G2 为 Fail 而预期 Pass；修复后通过。验证实际服务主机、代理 HEAD、G3 路由证据、根域保护不缩小、服务失败仍失败。
- `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`：成功。
- `cargo test -- --include-ignored`：核心 189、core_pipe 2、relay_chain 2、托盘 1，全部通过。包含真实 pinned 内核及本地固定出口阻断、重载、断开既有连接测试。
- `cargo build --release --workspace --locked`：三个发布程序构建成功。
- 合成设置页浏览器验收：填写入口 27893 保存成功，重新加载回显相同值；后端仅内存，不写注册表或凭据。
- 维护者安装使用正常 `--quit` 退出再启动；发布程序哈希与当前安装一致。系统代理接管门开放，G6、G1、九个 G2、G3、固定出口保护均通过，状态为 `agent_ready`。实际出口精确匹配原 ISP；私人 IP 不公开。
- 三个维护者使用的 API 中转站经保留入口均返回 HTTP 200；站点身份及服务凭据不进入公开包。此验证只覆盖网页连通性。
- 旧 guard、monitor、relay、内核、托盘不在运行；两个旧登录任务禁用，启动文件夹无旧链路入口。新版 HKCU Run 与四个桌面/开始菜单入口指向同一安装；保留的本地拉起任务没有登录触发器。
- 旧 CLI 包装目录已从用户 PATH 移除并归档；四个会调用旧脚本的桌面快捷方式移入可恢复备份。正式 Claude/Codex 程序仍可解析，三个用户级代理变量原样保留。
- 清理旧 shim 后，`cli::tests::env_reaches_the_child_process` 暴露了依赖本机目录存在的断言；改用临时 shim 标记与独立 PATH，仍运行真实 cmd 子进程核对环境，不依赖维护者安装。
- 日常上游当前没有监听，普通流量按配置回落 DIRECT；example.com、百度经本程序返回 HTTP 200，并取得 DIRECT 路由记录。YouTube 无法经此直连访问，不把它报告成功。

## 未验证与限制

- 没有重启 Windows 或实际注销登录；启动命令和目标已核对，下一次登录启动尚未实测。
- R11 为 `unknown/no_desktop_traffic`，本次没有可归属的 Claude 服务连接，不冒充真实对话验收。需要用户正常使用 Claude 后重新检测；不自动发送消息。
- 文件服务的 404 表示 HTTPS 应答，不证明文件上传、权限或账号操作成功。隧道与其他用户网络仍需各自验收。
- 旧浏览器资料目录的现有 Edge 窗口保留，它不是旧出口服务；不会因目录名称强制关闭用户浏览器。
- 固定出口仅覆盖经本程序的保护域名；两次巡检之间供应商换 IP 的风险仍存在。回滚不应重新启用会争抢系统代理的旧 guard。

## 升级与回滚

升级前记录已有客户端入口，备份 data 与当前程序，正常退出并完成恢复。新版使用同一入口后再进入；移动目录时先解除旧自启，重新注册新目录入口。保持只有一个系统代理写入者。

源码回滚使用本次合并前的 main 提交或保留的版本标签，通过新 revert 提交完成，不强推、不重写历史。二进制回滚：正常退出新版，恢复备份程序与已确认的 settings/profile，再通过同一入口启动；不要覆盖仍在运行的 exe，也不要直接覆盖活跃 state/journal。恢复失败先处理，不用备份状态文件掩盖失败。

本地私有备份保留旧程序、档案、启动入口与 PATH，不随发行包上传。旧 0.3.0 发布产物保持原样。给其他用户仅交付全新 zip、对应 SHA256 与无私人凭据示例。
