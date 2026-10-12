# 任务说明：给托盘加「Claude 桌面版」入口

> 写给接手这个任务的 AI Agent（Codex、Claude Code 等）和它的使用者。照着做完、验证通过后，用 Pull Request 提交到 `zxs4KHf/agent-egress-controller`。

## 0. 背景

0.1.0 能从托盘打开 Edge、Chrome、Claude Code（终端）和 Codex（终端），让它们走用户配置的出口。**Claude 桌面版（Windows）还没有入口**：它不是浏览器，也不是命令行。0.1.0 又不改系统代理（见 `docs/adr/0004-*.md` 和 `docs/ROADMAP.md`），所以直接双击打开的桌面版不会走出口。

目标：在托盘的 Agent 模式菜单里加一项「打开 Claude 桌面版」，打开的桌面版走出口，并且时区、语言和其他入口一致；退出 Agent 模式时，按现有规则处理它。

**范围外**：系统代理接管（路线图 0.3），以及任何修改 Claude 桌面版安装文件或配置文件的做法。

## 1. 先读

按顺序读，读完再动手：

1. [AGENTS.md](../../AGENTS.md)：硬规则和提交前检查。**必须遵守。**
2. [CONTEXT.md](../../CONTEXT.md)：术语，尤其是「入口」「接管门」「外部写入者」。
3. [docs/SPEC.md](../SPEC.md)：4.4 状态机（E5 启动入口，X1 关闭入口），4.5 环境验收。
4. 代码：
   - `crates/aec-core/src/profile.rs`：`Entry` 枚举、默认档案、`validate`；
   - `crates/aec-core/src/entries.rs`：浏览器的参数、实例判定 `decide`、`LaunchRecord`；
   - `crates/aec-core/src/live.rs`：`launch_browser`、`launch_cli`、`close_entry`、`spawn_detached`；
   - `crates/aec-core/src/controller.rs`：`open_entry`、`full_report` 怎么区分浏览器入口；
   - `crates/aec-tray/src/text.rs`：`entry_label`（菜单文字）。

## 2. 第一步：先手工验证可行性（不写代码）

Claude 桌面版是 Electron 应用。**[推断]** Electron 支持 Chromium 的 `--proxy-server`、`--proxy-bypass-list`、`--lang` 命令行开关，所以可以像浏览器一样，在启动参数里指定代理。这一点必须先实测，结论决定后面怎么做。

已知安装形态（Squirrel，按当前用户安装）：

```
%LOCALAPPDATA%\AnthropicClaude\
├── claude.exe          ← 启动器：启动后马上退出，不能拿它的 pid 做跟踪
├── Update.exe
└── app-<版本号>\claude.exe   ← 真正的程序，可能有多个版本目录，取版本号最大的
```

步骤：

1. 下载 Release 里的便携版（或从源码 `cargo run -p aec-tray`），设置好自己的出口，从托盘「进入 Agent 模式」。
2. **完全退出** Claude 桌面版：包括托盘里的 Claude 图标。确认任务管理器里没有 `claude.exe`。
3. 在 PowerShell 里手动启动（`$app` 换成版本号最大的目录）：
   ```powershell
   $app = Get-ChildItem "$env:LOCALAPPDATA\AnthropicClaude" -Directory -Filter 'app-*' | Sort-Object { [version]($_.Name -replace '^app-') } | Select-Object -Last 1
   & "$($app.FullName)\claude.exe" --proxy-server=http://127.0.0.1:17893 --proxy-bypass-list="<-loopback>;127.0.0.1;localhost" --lang=en-US
   ```
4. 在桌面版里发一条消息。然后看内核日志（托盘「打开数据和日志目录」→ `logs\core-<日期>.log`），找 `claude.ai` 和 `anthropic.com` 的连接：
   - 应该看到 `using agent-egress[relay]`，说明走了出口；
   - 如果只有 `DIRECT`，或者完全没有这些域名，说明没走代理。
5. 记录结果（这些是 PR 里要写的证据）：
   - Claude 版本号（`app-` 后面的数字）；
   - 日志里命中的几行；
   - 桌面版能否正常登录和对话。

**判定**：

- **走了出口** → 做第 3 节（方案 A）。
- **没走出口** → 不要写代码硬改。去 Issue 里报告实测结果（命令、日志、版本号），等维护者决定。在 0.3 系统代理接管做出来之前，临时办法见第 6 节。

## 3. 方案 A：实现桌面应用入口

### 3.1 档案（`profile.rs`）

- `Entry` 新增一个变体，例如：
  ```rust
  #[serde(rename_all = "camelCase")]
  App { id: String, app: DesktopApp, #[serde(default)] extra_args: Vec<String> }
  ```
  `DesktopApp` 先只有 `Claude`（序列化为 `"claude"`）。保持 `deny_unknown_fields`。
- 默认档案（`ProfileSet::default`）在 `codex-cli` 后面加一项 `{ "type": "app", "id": "claude-desktop", "app": "claude" }`。
- **老用户兼容**：已有的 `profiles.json` 里没有这一项。加载档案时，缺少的默认入口要补进去：只补缺失的 id，不改用户已有的项。补齐逻辑写成纯函数并配测试，放在 `store.rs` 或 `profile.rs`。
- 更新 `validate` 和已有测试（JSON 往返、字段错误）。

### 3.2 定位和启动（`entries.rs` + `live.rs`）

- 定位写成纯函数：输入是 `%LOCALAPPDATA%` 和「列目录」闭包，输出是 `app-*` 里版本号最大的 `claude.exe`。用假目录测试：没有安装、只有启动器、多个版本、版本号比较（`2.10.0` 大于 `2.9.0`）。
- 参数写成纯函数并测试：
  - `--proxy-server=http://127.0.0.1:<mixed 端口>`；
  - `--proxy-bypass-list=<profile.proxy_scope.bypass 用分号拼接>`；
  - `--lang=<profile.locale.browser_lang>`；
  - 最后追加 `extra_args`。
- 环境变量：照 CLI 入口的做法注入 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY`（复用 `cli::entry_env`）。这样桌面版的 Node 部分发出的请求也走出口。
- **已在运行的实例**：Electron 是单实例，已有实例时，新启动会把参数交给旧实例然后退出，代理参数就失效了。处理方式和浏览器一样：
  - 发现有不是本应用启动的 `claude.exe` 在运行时，返回 `ENTRY_BUSY`，提示用户先完全退出 Claude（包括托盘图标）；
  - **绝不替用户关掉它。**
  - 判断方法：按进程映像路径匹配 `AnthropicClaude\app-*\claude.exe`。
- 用 `spawn_detached`（标准输入输出置空，不继承句柄）启动真正的 `app-*\claude.exe`，**不要用启动器**。记录 `OwnedProcess`，role 用 `entry:app:<id>`。
- 找不到安装时返回 `ENTRY_NOT_INSTALLED`，消息里说明「不会自动下载」。

### 3.3 退出时关闭（`live.rs::close_entry`）

- 和浏览器一样：只对自己启动、并且身份核对一致（pid 加创建时间）的进程发 WM_CLOSE，等它退出，**不强杀**。
- Electron 的主窗口关了，进程可能还留在托盘里。等不到退出时返回 `ENTRY_STILL_RUNNING`，让用户自己退出。不要升级成结束进程。

### 3.4 检测（`controller.rs::full_report`）

- 桌面版没有检查页，所以**不产生** G5 和 R3–R5。这和 CLI 入口一样。确认 `is_browser` 的判断不会把 `App` 当成浏览器，否则会一直等 30 秒的浏览器报告。
- G1–G4 不变：G1–G3 是内核层面的检测，桌面版走同一个内核；G4 看系统时区，桌面版直接使用系统时区。

### 3.5 托盘文字（`aec-tray/src/text.rs`）

- `entry_label` 给 `App { app: Claude }` 返回「打开 Claude 桌面版」。
- 已有的 `match` 都要覆盖新变体，编译器会提示漏掉的地方。

### 3.6 文档

- `docs/SPEC.md` 4.4/4.5：写明桌面应用入口的启动、实例判定、关闭规则，以及它不参与 G5。
- `CONTEXT.md`：「入口」词条补上桌面应用。
- `CHANGELOG.md` 的「未发布」：新增「Claude 桌面版入口」。
- README 的「它怎么工作」里，入口列表加上 Claude 桌面版。

## 4. 验证（PR 里逐条给证据）

在仓库根目录：

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

真机验证（在 PR 里写成 **[已验证]**，附命令输出、日志片段或截图；**截图和日志里的出口 IP 要打码**）：

1. 先完全退出 Claude，然后：进入 Agent 模式 →「打开 Claude 桌面版」→ 能登录、能对话 → 内核日志里 `claude.ai` 和 `anthropic.com` 都是 `using agent-egress[relay]`。
2. Claude 已经开着的时候点这个菜单：提示先退出 Claude，并且**没有关掉它**。
3. 退出 Agent 模式：桌面版被关闭；如果关不掉，提示用户自己退出。之后没有残留的 aec 进程。
4. 用 0.1.0 的旧 `profiles.json` 启动：菜单里出现新入口，用户原来的入口和设置没有被改动。
5. 没装 Claude 桌面版的机器（或临时改名安装目录）：提示「未安装，不会自动下载」。

做不到的项写成 **[推断]** 并说明原因，不要省略。

## 5. 提交

1. Fork `zxs4KHf/agent-egress-controller`，从 `main` 开分支 `feat/claude-desktop-entry`。
2. 提交信息用英文祈使句，例如 `Add a Claude desktop app entry`。一个 PR 只做这一件事。
3. 开 Pull Request 到 `main`：
   - 按模板写清「改了什么 / 为什么 / 怎么验证 / 怎么回滚」；
   - 关联对应的 Issue；
   - CI 必须是绿的。
4. **不要提交**：
   - 自己的出口地址、账号、密码；
   - `data\` 目录、日志、`runtime\`；
   - 没打码的截图；
   - mihomo 二进制。

## 6. 临时办法（方案 A 实测不可行时）

在 0.3 系统代理接管做出来之前，可以手动设置系统代理：

1. 进入 Agent 模式。
2. Windows「设置 → 网络和 Internet → 代理 → 手动设置代理」，地址填 `127.0.0.1`，端口填 `17893`，打开。
3. 正常打开 Claude 桌面版使用。
4. **用完先关掉这个手动代理，再退出 Agent 模式。** 否则退出后所有软件都连不上网，因为本机代理已经停了。

注意：这样做的时候，电脑上**所有**遵守系统代理的软件都会走出口。本程序不会帮你恢复这个手动设置，也不会检测到它。这正是 0.1.0 不自动做这件事的原因。
