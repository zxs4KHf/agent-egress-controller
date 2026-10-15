//! Everything the tray does that is not window plumbing: where files live, and the worker thread
//! that owns the controller. Commands run one at a time, off the UI thread; the UI reads [`Shared`].

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use aec_core::controller::{Controller, EnterOptions, KeepOutcome, Mode, Settings};
use aec_core::core::process::CoreManager;
use aec_core::credentials::WindowsCredentials;
use aec_core::live::LivePlatform;
use aec_core::paths::{strip_verbatim, APP_DIR_NAME, PORTABLE_FLAG};
use aec_core::profile::{self, AgentProfile, FieldError, ProfileSet};
use aec_core::prompts::{self, PromptContext};
use aec_core::setup::{self, CredentialChange, ExitForm, SetupBackend, SetupView};
use aec_core::snapshot::TakeoverGate;
use aec_core::store::ProfileStore;

use crate::text;

/// Periodic fixed-exit recovery never writes globals or changes the saved path/exit identity.
trait FixedGate {
    fn verified(&self) -> bool;
    fn verify(&self) -> Result<(), aec_core::controller::Fault>;
    fn block(&self) -> Result<(), aec_core::controller::Fault>;
    fn open(&self) -> Result<(), aec_core::controller::Fault>;
}

enum FixedPoll {
    Healthy,
    Recovered,
    Blocked {
        was_verified: bool,
        probe_error: aec_core::controller::Fault,
        block_error: Option<aec_core::controller::Fault>,
    },
}

fn poll_fixed(gate: &impl FixedGate) -> FixedPoll {
    let was_verified = gate.verified();
    let result = gate.verify().and_then(|()| {
        if was_verified {
            Ok(false)
        } else {
            // open() reselects only the saved first hop and requires another exact probe.
            gate.open().map(|()| true)
        }
    });
    match result {
        Ok(false) => FixedPoll::Healthy,
        Ok(true) => FixedPoll::Recovered,
        Err(probe_error) => FixedPoll::Blocked {
            was_verified,
            probe_error,
            block_error: gate.block().err(),
        },
    }
}

fn needs_refresh(poll: &FixedPoll, _mode: Mode) -> bool {
    _mode == Mode::AgentUnverified && matches!(poll, FixedPoll::Healthy)
}

struct LiveFixedGate<'a> {
    platform: &'a LivePlatform,
    profile: &'a AgentProfile,
}

impl FixedGate for LiveFixedGate<'_> {
    fn verified(&self) -> bool {
        self.platform.fixed_verified()
    }
    fn verify(&self) -> Result<(), aec_core::controller::Fault> {
        self.platform.verify_fixed(self.profile)
    }
    fn block(&self) -> Result<(), aec_core::controller::Fault> {
        self.platform.block_fixed()
    }
    fn open(&self) -> Result<(), aec_core::controller::Fault> {
        self.platform.open_fixed(self.profile)
    }
}

/// Where the binaries and the data are.
#[derive(Debug, Clone)]
pub struct Layout {
    pub data_dir: PathBuf,
    pub mihomo: PathBuf,
    pub relay: PathBuf,
}

impl Layout {
    /// Portable (`portable.flag` next to the exe), development (exe built inside this repository),
    /// or installed (data under %LOCALAPPDATA%).
    pub fn detect() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .map(|p| strip_verbatim(&p))
            .unwrap_or_else(|| PathBuf::from("."));
        let relay = exe_dir.join("aec-relay.exe");
        let bundled = exe_dir.join("bin").join(aec_core::core::pin::EXE_NAME);
        if exe_dir.join(PORTABLE_FLAG).is_file() {
            return Self {
                data_dir: exe_dir.join("data"),
                mihomo: bundled,
                relay,
            };
        }
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        if let Ok(repo) = repo.canonicalize().map(|p| strip_verbatim(&p)) {
            if exe_dir.starts_with(repo.join("target")) {
                return Self {
                    data_dir: repo.join("runtime").join("dev-data"),
                    mihomo: repo
                        .join("runtime")
                        .join("bin")
                        .join(aec_core::core::pin::EXE_NAME),
                    relay,
                };
            }
        }
        let lad = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| exe_dir.clone());
        Self {
            data_dir: lad.join(APP_DIR_NAME),
            mihomo: bundled,
            relay,
        }
    }

    pub fn platform(&self) -> LivePlatform {
        let core = CoreManager {
            data_dir: self.data_dir.clone(),
            bin: self.mihomo.clone(),
            relay_bin: self.relay.clone(),
            ports: Settings::load(&self.data_dir).core_ports(),
        };
        LivePlatform::new(core, &self.data_dir)
    }

    pub fn report_path(&self) -> PathBuf {
        self.data_dir.join("检测结果.txt")
    }
}

pub enum Cmd {
    Configure(serde_json::Value, Sender<Result<(), Vec<FieldError>>>),
    AgentAction(String, Sender<Result<(), Vec<FieldError>>>),
    Startup(bool, Sender<Result<(), Vec<FieldError>>>),
    Shortcuts(Sender<Result<(), Vec<FieldError>>>),
    Save(Box<ExitForm>, Sender<Result<(), Vec<FieldError>>>),
    Rules(
        aec_core::dashboard::RulesForm,
        Sender<Result<(), Vec<FieldError>>>,
    ),
    Path(String, Sender<Result<(), Vec<FieldError>>>),
    Route(
        aec_core::route_switch::Form,
        Sender<Result<(), Vec<FieldError>>>,
    ),
    Enter,
    Open(String),
    Switch {
        group: String,
        member: String,
    },
    Recheck,
    /// Leave Agent mode, recover an interrupted session or retry a failed restore.
    Exit,
    Ack,
    CopyPrompts,
    /// Restore if needed and reply; the worker keeps running (log-off that may still be cancelled).
    Restore(Sender<()>),
    /// Restore if needed, then reply and stop so the UI can quit.
    Shutdown(Sender<()>),
    /// Periodic observation: notify if another program changes the system proxy.
    Keep,
}

/// A notification for the UI to show once.
#[derive(Debug, Clone)]
pub struct Note {
    pub title: String,
    pub text: String,
    pub error: bool,
}

#[derive(Debug, Clone)]
pub struct Shared {
    pub fixed_blocked: bool,
    pub mode: Mode,
    /// Set while a command runs; menu actions are disabled.
    pub busy: Option<&'static str>,
    pub configured: bool,
    /// (entry id, menu label)
    pub entries: Vec<(String, String)>,
    /// (group, member, selected); menu IDs are allocated from a separate range.
    pub switches: Vec<(String, String, bool, bool)>,
    pub has_report: bool,
    pub note: Option<Note>,
    pub summary: Option<String>,
    /// Text for the UI thread to put on the clipboard: only a window-owned clipboard accepts data.
    pub clipboard: Option<String>,
}

pub type SharedRef = Arc<Mutex<Shared>>;

fn load_set(data_dir: &Path) -> ProfileSet {
    let store = ProfileStore::new(data_dir);
    let _ = store.init_if_missing();
    store.load().unwrap_or_default()
}

fn agent_profile(data_dir: &Path) -> AgentProfile {
    load_set(data_dir).agent.remove(0)
}

pub struct Worker {
    _session: aec_core::store::SessionLease,
    last_fixed_probe: std::time::Instant,
    layout: Layout,
    controller: Controller<LivePlatform>,
    shared: SharedRef,
    notify: Box<dyn Fn() + Send>,
}

impl Worker {
    pub fn new(
        layout: Layout,
        shared: SharedRef,
        notify: Box<dyn Fn() + Send>,
    ) -> Result<Self, String> {
        let session = aec_core::store::SessionLease::acquire(&layout.data_dir)?;
        std::fs::create_dir_all(&layout.data_dir)
            .map_err(|e| format!("{}: {e}", layout.data_dir.display()))?;
        let controller = Controller::open(
            layout.platform(),
            &layout.data_dir,
            Settings::load(&layout.data_dir),
        )
        .map_err(|e| e.to_string())?;
        let w = Self {
            _session: session,
            last_fixed_probe: std::time::Instant::now(),
            layout,
            controller,
            shared,
            notify,
        };
        w.publish(None);
        if w.controller.recovery_pending() {
            w.note(
                "上次的 Agent 会话没有正常结束",
                "从托盘菜单点「恢复上次未完成的会话」，把时区等设置改回去。",
                true,
            );
        }
        Ok(w)
    }

    fn note(&self, title: &str, text: &str, error: bool) {
        self.shared.lock().expect("lock").note = Some(Note {
            title: title.into(),
            text: text.into(),
            error,
        });
        (self.notify)();
    }

    /// Refresh what the menu needs from the controller and the profile.
    fn publish(&self, busy: Option<&'static str>) {
        let set = load_set(&self.layout.data_dir);
        let p = &set.agent[0];
        {
            let mut s = self.shared.lock().expect("lock");
            s.mode = self.controller.state().mode;
            s.fixed_blocked = p.egress.fixed_exit && !self.controller.platform().fixed_verified();
            s.busy = busy;
            s.configured = profile::validate(&set).is_empty();
            s.entries = p
                .entries
                .iter()
                .map(|e| (e.id().to_string(), text::entry_label(e)))
                .collect();
            s.has_report = self.controller.state().last_report.is_some();
            s.switches = p
                .routing
                .groups
                .iter()
                .filter(|g| g.switchable)
                .flat_map(|g| {
                    g.members.iter().map(|member| {
                        (
                            g.name.clone(),
                            member.clone(),
                            g.selected.as_ref().or_else(|| g.members.first()) == Some(member),
                            aec_core::route_switch::fixed_member(&p.routing, member),
                        )
                    })
                })
                .collect();
            s.summary = self
                .controller
                .state()
                .last_report
                .as_ref()
                .map(text::summary);
        }
        (self.notify)();
    }

    fn write_report(&self) {
        if let Some(r) = &self.controller.state().last_report {
            let _ = std::fs::write(
                self.layout.report_path(),
                text::report(self.controller.state().mode, r),
            );
        }
    }

    pub fn run(mut self, rx: Receiver<Cmd>) {
        for cmd in rx {
            if matches!(cmd, Cmd::Keep) {
                self.keep();
                continue;
            }
            let label = match &cmd {
                Cmd::AgentAction(..) => "正在执行 Agent 操作…",
                Cmd::Startup(..) | Cmd::Shortcuts(..) => "正在保存启动入口…",
                Cmd::Save(..) | Cmd::Rules(..) | Cmd::Configure(..) => "正在保存设置…",
                Cmd::Path(..) => "正在核对固定出口…",
                Cmd::Enter => "正在进入 Agent 模式…",
                Cmd::Open(_) => "正在打开并检测…",
                Cmd::Switch { .. } | Cmd::Route(..) => "正在切换路由并重新检测…",
                Cmd::Recheck => "正在重新检测…",
                Cmd::Exit | Cmd::Restore(_) | Cmd::Shutdown(_) => "正在退出并恢复…",
                Cmd::Ack => "正在处理…",
                Cmd::CopyPrompts => "正在生成提示词…",
                Cmd::Keep => unreachable!("handled above"),
            };
            self.publish(Some(label));
            let quit = self.handle(cmd);
            self.publish(None);
            if quit {
                return;
            }
        }
    }

    fn after_checks(&self, title: &str) {
        self.write_report();
        let mode = self.controller.state().mode;
        let summary = self
            .controller
            .state()
            .last_report
            .as_ref()
            .map(text::summary)
            .unwrap_or_default();
        let ok = mode == Mode::AgentReady;
        self.note(
            title,
            &format!(
                "{}。{}",
                summary,
                if ok {
                    "从托盘打开浏览器或 CLI 即可使用。"
                } else {
                    "点托盘「查看检测结果」看详情。"
                }
            ),
            !ok,
        );
    }

    /// Returns true when the worker should stop.
    fn handle(&mut self, cmd: Cmd) -> bool {
        let data_dir = self.layout.data_dir.clone();
        match cmd {
            Cmd::Route(form, done) => {
                let result = self.change_route(form).map_err(field_error);
                let _ = done.send(result);
            }
            Cmd::Configure(patch, done) => {
                // Merge in the serial worker, after earlier panel/API saves have completed.
                let form = aec_core::agent::patch_settings(
                    Backend::read_view(&data_dir, self.controller.state().mode != Mode::Daily),
                    patch,
                );
                let result = form.and_then(|form| self.save_settings(form));
                let _ = done.send(result);
            }
            Cmd::AgentAction(action, done) => {
                let result = match action.as_str() {
                    "enter" => {
                        let set = load_set(&data_dir);
                        if !profile::validate(&set).is_empty() {
                            Err(aec_core::controller::Fault::new(
                                "PROFILE_INVALID",
                                "请先在看板配置出口",
                            ))
                        } else {
                            self.controller.set_settings(Settings::load(&data_dir));
                            self.controller
                                .enter(&set.agent[0], EnterOptions::default())
                                .and_then(|out| out.fault.map_or(Ok(()), Err))
                        }
                    }
                    "recheck" => self.recheck().map(|_| ()),
                    "exit" => self.controller.exit().and_then(|out| {
                        if out.mode_after == Mode::Daily {
                            Ok(())
                        } else {
                            Err(aec_core::controller::Fault::new(
                                "RESTORE_INCOMPLETE",
                                "恢复未完成，请查看看板",
                            ))
                        }
                    }),
                    _ => Err(aec_core::controller::Fault::new(
                        "ACTION_INVALID",
                        "不支持的操作",
                    )),
                };
                self.write_report();
                // Publish the final state before the synchronous reply, so API success cannot
                // expose an old ready state or merely mean "queued".
                self.publish(None);
                let _ =
                    done.send(result.map_err(|_| {
                        field_error("操作失败，请查看看板中的实际状态和检测报告".into())
                    }));
            }
            Cmd::Startup(enabled, done) => {
                let result = crate::startup::set(enabled);
                match &result {
                    Ok(()) => self.note(
                        "登录自启设置已保存",
                        if enabled {
                            "下次登录启动本安装并尝试进入 Agent 模式；未恢复会话需要先恢复。"
                        } else {
                            "下次登录不再自动启动本安装。"
                        },
                        false,
                    ),
                    Err(e) => self.note("登录自启未保存", e, true),
                }
                let _ = done.send(result.map_err(field_error));
            }
            Cmd::Shortcuts(done) => {
                let result = crate::startup::shortcuts();
                match &result {
                    Ok(()) => self.note(
                        "启动入口已创建",
                        "桌面和开始菜单已有托盘、看板快捷方式；可在 Windows 搜索 Agent Egress。",
                        false,
                    ),
                    Err(e) => self.note("创建入口失败", e, true),
                }
                let _ = done.send(result.map_err(field_error));
            }
            Cmd::Save(form, done) => {
                let result = self.save_settings(*form);
                let _ = done.send(result);
            }
            Cmd::Rules(form, done) => {
                let result = self.edit_rules(form);
                let _ = done.send(result.map_err(field_error));
            }
            Cmd::Path(path, done) => {
                let result = self.change_path(path);
                let _ = done.send(result.map_err(field_error));
            }
            Cmd::Enter => {
                let set = load_set(&data_dir);
                if !profile::validate(&set).is_empty() {
                    self.note(
                        "还没有设置出口",
                        "先在托盘菜单点「设置出口和分流…」。",
                        true,
                    );
                    return false;
                }
                self.controller.set_settings(Settings::load(&data_dir));
                let p = set.agent[0].clone();
                match self.controller.enter(&p, EnterOptions::default()) {
                    Ok(out) => match out.fault {
                        Some(f) => {
                            self.note(
                                "没能进入 Agent 模式（已自动撤回）",
                                &format!("{}：{}", f.code, f.message),
                                true,
                            );
                        }
                        None => self.after_checks(text::mode_label(out.mode)),
                    },
                    Err(f) => self.note(
                        "没能进入 Agent 模式",
                        &format!("{}：{}", f.code, f.message),
                        true,
                    ),
                }
            }
            Cmd::Open(id) => match self.controller.open_entry(&agent_profile(&data_dir), &id) {
                Ok(_) => self.after_checks(&format!("已打开 {id}")),
                Err(f) => self.note(
                    &format!("没能打开 {id}"),
                    &format!("{}：{}", f.code, f.message),
                    true,
                ),
            },
            Cmd::Recheck => match self.recheck() {
                Ok(_) => self.after_checks("重新检测完成"),
                Err(f) => self.note("没能重新检测", &f.to_string(), true),
            },
            Cmd::Exit => self.exit(),
            Cmd::Ack => match self.controller.acknowledge() {
                Ok(()) => self.note(
                    "已回到日常模式",
                    "没恢复的项目保持现状，记录在日志里。",
                    false,
                ),
                Err(f) => self.note("操作失败", &f.to_string(), true),
            },
            Cmd::CopyPrompts => {
                let text = self.prompts();
                self.shared.lock().expect("lock").clipboard = Some(text);
                self.note(
                    "排查提示词已复制",
                    "粘贴给任意 AI 助手即可。内容已去掉密码，不会自动发送到任何地方。",
                    false,
                );
            }
            Cmd::Restore(done) => {
                if self.controller.state().mode != Mode::Daily {
                    self.exit();
                }
                let _ = done.send(());
            }
            Cmd::Shutdown(done) => {
                if self.controller.state().mode != Mode::Daily {
                    self.exit();
                }
                let _ = done.send(());
                return true;
            }
            Cmd::Switch { group, member } => self.switch_route(&group, &member),
            Cmd::Keep => self.keep(),
        }
        false
    }

    fn keep(&mut self) {
        match self.controller.keep_system_proxy() {
            None => {}
            Some(KeepOutcome::Changed { found }) => self.note(
                "系统代理发生冲突",
                &format!(
                    "检测到系统代理被改为 {found}，已撤销就绪状态；本程序不会与其他程序争写。请关闭另一代理客户端的「系统代理」开关（可保留内核并切换节点），不要同时启用 VPN/TUN/全局代理。具体步骤见托盘菜单「代理冲突说明」，处理后退出并重新进入 Agent 模式。"
                ),
                true,
            ),
            Some(KeepOutcome::Failed(e)) => self.note("系统代理巡检失败", &e, true),
        }
        let p = agent_profile(&self.layout.data_dir);
        if p.egress.fixed_exit
            && matches!(
                self.controller.state().mode,
                Mode::AgentReady | Mode::AgentUnverified
            )
            && self.last_fixed_probe.elapsed() >= std::time::Duration::from_secs(30)
        {
            let poll = poll_fixed(&LiveFixedGate {
                platform: self.controller.platform(),
                profile: &p,
            });
            let refresh_unverified = needs_refresh(&poll, self.controller.state().mode);
            match poll {
                FixedPoll::Healthy => {}
                FixedPoll::Recovered => {
                    self.publish(Some("固定出口已恢复，正在重新检测…"));
                    if let Err(e) = self.controller.recheck(&p) {
                        let _ = self.controller.invalidate_routing_checks();
                        self.note("固定出口恢复后的检测失败", &e.message, true);
                    }
                }
                FixedPoll::Blocked {
                    was_verified,
                    probe_error,
                    block_error,
                } => {
                    self.shared.lock().expect("lock").fixed_blocked = true;
                    let _ = self.controller.invalidate_routing_checks();
                    if let Some(block_error) = block_error {
                        self.note(
                            "固定出口失去验证，阻断操作失败",
                            &format!(
                                "{}；{}。请停止使用受保护服务并恢复会话。",
                                probe_error.message, block_error.message
                            ),
                            true,
                        );
                    } else if was_verified {
                        self.note("固定出口已阻断", &format!("{}。将定期重新核对同一出口，取得精确证据后自动恢复；也可点击重新检测。", probe_error.message), true);
                    }
                }
            }
            if refresh_unverified {
                self.publish(Some("网络已恢复，正在重新检测…"));
                if let Err(e) = self.controller.recheck(&p) {
                    self.note("网络恢复后的检测未通过", &e.message, true);
                }
            }
            // A full recovery check can take time; do not immediately poll again afterwards.
            self.last_fixed_probe = std::time::Instant::now();
        }
        self.write_report();
        self.publish(None);
    }

    fn switch_route(&mut self, group: &str, member: &str) {
        if let Err(e) = self.change_route(aec_core::route_switch::Form {
            group: group.into(),
            member: member.into(),
        }) {
            self.note("路由切换未完成", &e, true);
        }
    }

    fn change_route(&mut self, form: aec_core::route_switch::Form) -> Result<(), String> {
        if !self.active() && self.controller.state().mode != Mode::Daily {
            return Err("当前正在恢复或切换模式，请完成后重试".into());
        }
        let store = ProfileStore::new(&self.layout.data_dir);
        let previous = store.load().map_err(|e| e.to_string())?;
        let plan = aec_core::route_switch::plan(&previous, &form)?;
        let p = &plan.profiles.agent[0];
        if self.active() {
            self.controller
                .invalidate_routing_checks()
                .map_err(|e| e.to_string())?;
            if p.egress.fixed_exit {
                self.block()?;
            }
        }
        store.save(&plan.profiles).map_err(|e| e.to_string())?;
        if self.active() {
            if let Err(e) = self.controller.platform().core.reload(
                &aec_core::windows::WindowsHost,
                p,
                &WindowsCredentials,
            ) {
                let restored = store.save(&previous).is_ok();
                return Err(format!(
                    "规则重载失败：{e}；旧档案回存{}，请重新检测或恢复会话",
                    if restored { "成功" } else { "失败" }
                ));
            }
            if p.egress.fixed_exit {
                if let Err(e) = self.open_fixed(p) {
                    self.note(
                        "路由已保存，固定 ISP 尚未验证",
                        "普通出口规则已生效；受保护站点继续阻断，等待同一 ISP 验证。",
                        true,
                    );
                    if plan.fixed {
                        return Err(e);
                    }
                    return Ok(());
                }
            } else {
                self.controller.recheck(p).map_err(|e| e.to_string())?;
            }
        }
        self.write_report();
        self.note(
            "路由已切换",
            &format!(
                "{} → {}；涉及 {} 个域名。{}",
                form.group,
                form.member,
                plan.domains.len(),
                if plan.fixed {
                    "固定 ISP 保护已启用；是否就绪请查看检测结果。"
                } else {
                    "这些域名使用所选出口，其公网 IP 可随上游节点改变。"
                }
            ),
            false,
        );
        Ok(())
    }

    fn exit(&mut self) {
        match self.controller.exit() {
            Ok(r) => {
                let ok = r.mode_after == Mode::Daily;
                self.note(
                    if ok {
                        "已退出 Agent 模式"
                    } else {
                        "恢复未完成"
                    },
                    &text::restore_summary(&r),
                    !ok,
                );
            }
            Err(f) => self.note("退出失败", &f.to_string(), true),
        }
    }

    fn prompts(&self) -> String {
        let data_dir = &self.layout.data_dir;
        let set = load_set(data_dir);
        let profile = &set.agent[0];
        let gaps = profile::validate(&set);
        let st = aec_core::status::collect(&aec_core::windows::WindowsHost, Some(data_dir));
        let gate =
            TakeoverGate::evaluate(self.controller.enabled(profile), &st.foreign_writers, true);
        let state = self.controller.state();
        let data_dir_text = data_dir.display().to_string();
        let ctx = PromptContext {
            mode: state.mode.as_str(),
            checked_at: state.last_report.as_ref().map(|r| r.generated_at.as_str()),
            profile,
            data_dir: &data_dir_text,
            mask_ip: false,
        };
        let text = prompts::all(
            state.last_report.as_ref(),
            &state.unacknowledged_restore_failures,
            &gaps,
            Some(&gate),
            &ctx,
        );
        if text.trim().is_empty() {
            "当前没有需要排查的问题。".into()
        } else {
            text
        }
    }
}

fn field_error(message: String) -> Vec<FieldError> {
    vec![FieldError {
        path: "form".into(),
        message,
    }]
}

impl Worker {
    fn active(&self) -> bool {
        matches!(
            self.controller.state().mode,
            Mode::AgentReady | Mode::AgentUnverified
        )
    }
    fn block(&mut self) -> Result<(), String> {
        self.shared.lock().expect("lock").fixed_blocked = true;
        let invalidate = self.controller.invalidate_routing_checks();
        let block = self.controller.platform().block_fixed();
        invalidate.and(block).map_err(|e| e.to_string())
    }
    fn open_fixed(&mut self, p: &AgentProfile) -> Result<(), String> {
        self.controller
            .platform()
            .open_fixed(p)
            .map_err(|e| e.to_string())?;
        self.shared.lock().expect("lock").fixed_blocked = false;
        self.last_fixed_probe = std::time::Instant::now();
        self.controller.recheck(p).map_err(|e| e.to_string())?;
        self.write_report();
        Ok(())
    }
    fn recheck(&mut self) -> Result<(), aec_core::controller::Fault> {
        let p = agent_profile(&self.layout.data_dir);
        if p.egress.fixed_exit {
            self.block()
                .and_then(|()| self.open_fixed(&p))
                .map_err(|e| aec_core::controller::Fault::new("FIXED_EXIT_UNVERIFIED", e))?;
        } else {
            self.controller.recheck(&p)?;
        }
        Ok(())
    }
    fn change_path(&mut self, path: String) -> Result<(), String> {
        let store = ProfileStore::new(&self.layout.data_dir);
        let mut set = store.load().map_err(|e| e.to_string())?;
        if aec_core::fixed::path_member(&set.agent[0], &path).is_none() {
            return Err("所选连接路径不可用".into());
        }
        if !set.agent[0].egress.fixed_exit && self.controller.state().mode != Mode::Daily {
            return Err("首次启用固定出口保护需要先退出 Agent 模式".into());
        }
        if !self.active() && self.controller.state().mode != Mode::Daily {
            return Err("当前模式不可修改连接路径".into());
        }
        set.agent[0].egress.fixed_exit = true;
        set.agent[0].egress.connection_path = Some(path);
        let errors = profile::validate(&set);
        if !errors.is_empty() {
            return Err("请先在出口设置中配置精确 IP 和有效出口参数".into());
        }
        if self.active() {
            self.block()?;
        }
        store.save(&set).map_err(|e| e.to_string())?;
        if self.active() {
            self.open_fixed(&set.agent[0])?;
        }
        Ok(())
    }
    fn edit_rules(&mut self, form: aec_core::dashboard::RulesForm) -> Result<(), String> {
        let store = ProfileStore::new(&self.layout.data_dir);
        let mut set = store.load().map_err(|e| e.to_string())?;
        if self.active() && !set.agent[0].egress.fixed_exit {
            return Err("先退出 Agent 模式，再启用固定出口保护或修改规则".into());
        }
        if !self.active() && self.controller.state().mode != Mode::Daily {
            return Err("当前模式不可编辑规则".into());
        }
        aec_core::dashboard::apply(&mut set, form).map_err(|errors| {
            errors
                .into_iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        if self.active() {
            self.block()?;
        }
        store.save(&set).map_err(|e| e.to_string())?;
        if self.active() {
            self.controller
                .platform()
                .core
                .reload(
                    &aec_core::windows::WindowsHost,
                    &set.agent[0],
                    &WindowsCredentials,
                )
                .map_err(|e| e.to_string())?;
            self.open_fixed(&set.agent[0])?;
        }
        Ok(())
    }
    fn save_settings(&mut self, form: ExitForm) -> Result<(), Vec<FieldError>> {
        if self.controller.state().mode != Mode::Daily {
            return Err(field_error(
                "现在处于 Agent 模式，先退出再修改出口设置".into(),
            ));
        }
        let mut set = load_set(&self.layout.data_dir);
        // Validate installation settings before any profile or credential mutation.
        let settings = setup::apply_settings(&form, Settings::load(&self.layout.data_dir))?;
        let creds = WindowsCredentials;
        let user = creds
            .user_name(setup::CREDENTIAL_REF)
            .map_err(field_error)?;
        let stored = creds
            .exists(setup::TUNNEL_CREDENTIAL_REF)
            .map_err(field_error)?;
        let applied = setup::apply(&mut set, &form, user.as_deref(), stored)?;
        for (reference, change) in [
            (setup::CREDENTIAL_REF, applied.exit),
            (setup::TUNNEL_CREDENTIAL_REF, applied.tunnel),
        ] {
            match change {
                CredentialChange::Store { user, pass } => {
                    creds.store(reference, &user, &pass).map_err(field_error)?
                }
                CredentialChange::Delete => {
                    creds.delete(reference).map_err(field_error)?;
                }
                CredentialChange::Keep => {}
            }
        }
        ProfileStore::new(&self.layout.data_dir)
            .save(&set)
            .map_err(|e| field_error(e.to_string()))?;
        settings
            .save(&self.layout.data_dir)
            .map_err(|e| field_error(e.to_string()))?;
        // Daily mode has no active core: recreate the adapter so the next enter uses new ports.
        self.controller = Controller::open(self.layout.platform(), &self.layout.data_dir, settings)
            .map_err(|e| field_error(e.to_string()))?;
        Ok(())
    }
}

/// The settings page's view of this installation.
pub struct Backend {
    pub tx: Sender<Cmd>,
    pub data_dir: PathBuf,
    pub shared: SharedRef,
}

impl Backend {
    fn locked(&self) -> bool {
        self.shared.lock().expect("lock").mode != Mode::Daily
    }
    fn read_view(data_dir: &Path, locked: bool) -> SetupView {
        let set = load_set(data_dir);
        let user = WindowsCredentials
            .user_name(setup::CREDENTIAL_REF)
            .ok()
            .flatten();
        let tunnel_stored = WindowsCredentials
            .exists(setup::TUNNEL_CREDENTIAL_REF)
            .unwrap_or(false);
        let settings = Settings::load(data_dir);
        setup::view(
            &set,
            &setup::ViewInput {
                core_mixed_port: settings.core_mixed_port,
                stored_user: user.as_deref(),
                tunnel_password_stored: tunnel_stored,
                takeover_enabled: settings.takeover_enabled,
                locked,
            },
        )
    }
}

impl SetupBackend for Backend {
    fn route(&self, form: aec_core::route_switch::Form) -> Result<(), Vec<FieldError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Cmd::Route(form, tx))
            .map_err(|_| field_error("托盘服务已退出".into()))?;
        rx.recv()
            .map_err(|_| field_error("托盘服务已退出".into()))?
    }
    fn update(&self, ui_only: bool) -> Result<(), Vec<FieldError>> {
        let state = self.shared.lock().expect("lock").clone();
        crate::update::launch(ui_only, state.mode, state.busy.is_some(), &self.data_dir)
            .map_err(field_error)
    }
    fn configure(&self, patch: serde_json::Value) -> Result<(), Vec<FieldError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Cmd::Configure(patch, tx))
            .map_err(|_| field_error("托盘服务已退出".into()))?;
        rx.recv()
            .map_err(|_| field_error("托盘服务已退出".into()))?
    }
    fn action(&self, action: &str) -> Result<(), Vec<FieldError>> {
        if !["enter", "exit", "recheck"].contains(&action) {
            return Err(field_error("不支持的模式操作".into()));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Cmd::AgentAction(action.into(), tx))
            .map_err(|_| field_error("托盘服务已退出".into()))?;
        rx.recv()
            .map_err(|_| field_error("托盘服务已退出".into()))?
    }
    fn agent_access(&self, enabled: bool) -> Result<(), Vec<FieldError>> {
        // Revoking access must not wait behind a minutes-long network check. This flag cannot
        // change routing or global state. Serialize its atomic writes using the shared state lock.
        let _guard = self.shared.lock().expect("lock");
        aec_core::store::write_atomic(
            &self.data_dir.join(aec_core::agent::ACCESS),
            if enabled { b"true" } else { b"false" },
        )
        .map_err(|_| field_error("Agent 接入选项保存失败".into()))
    }
    fn agent_info(&self) -> serde_json::Value {
        let exe = std::env::current_exe()
            .unwrap_or_default()
            .with_file_name("aec-agent.exe");
        serde_json::json!({"available":self.data_dir.join(aec_core::agent::DISCOVERY).is_file(),"enabled":aec_core::agent::enabled(&self.data_dir),"mcpConfig":aec_core::agent::config(&self.data_dir,&exe)})
    }
    fn dashboard(&self) -> Option<aec_core::dashboard::View> {
        let s = self.shared.lock().expect("lock").clone();
        let mut view = aec_core::dashboard::view(
            &load_set(&self.data_dir),
            s.mode.as_str(),
            s.busy.is_some(),
            s.fixed_blocked,
            s.summary,
        );
        view.autostart = crate::startup::enabled();
        Some(view)
    }
    fn startup(&self, enabled: bool) -> Result<(), Vec<FieldError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Cmd::Startup(enabled, tx))
            .map_err(|_| field_error("托盘服务已退出".into()))?;
        rx.recv()
            .map_err(|_| field_error("托盘服务已退出".into()))?
    }
    fn shortcuts(&self) -> Result<(), Vec<FieldError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Cmd::Shortcuts(tx))
            .map_err(|_| field_error("托盘服务已退出".into()))?;
        rx.recv()
            .map_err(|_| field_error("托盘服务已退出".into()))?
    }
    fn rules(&self, form: aec_core::dashboard::RulesForm) -> Result<(), Vec<FieldError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Cmd::Rules(form, tx))
            .map_err(|_| field_error("托盘服务已退出".into()))?;
        rx.recv()
            .map_err(|_| field_error("托盘服务已退出".into()))?
    }
    fn path(&self, path: String) -> Result<(), Vec<FieldError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Cmd::Path(path, tx))
            .map_err(|_| field_error("托盘服务已退出".into()))?;
        rx.recv()
            .map_err(|_| field_error("托盘服务已退出".into()))?
    }
    fn view(&self) -> SetupView {
        Self::read_view(&self.data_dir, self.locked())
    }

    fn save(&self, form: ExitForm) -> Result<(), Vec<FieldError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Cmd::Save(Box::new(form), tx))
            .map_err(|_| field_error("托盘服务已退出".into()))?;
        rx.recv()
            .map_err(|_| field_error("托盘服务已退出".into()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::controller::Fault;
    use aec_core::http::{HttpClient, HttpError, Request, Response};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    struct Client(Result<Response, HttpError>);
    impl HttpClient for Client {
        fn send(&self, request: &Request) -> Result<Response, HttpError> {
            assert_eq!(request.route, aec_core::http::Route::Proxy(17327));
            self.0.clone()
        }
    }

    fn evidence(body: Option<&str>) -> Result<(), Fault> {
        let response = body.map_or(Err(HttpError::Timeout), |body| {
            Ok(Response {
                status: 200,
                body: body.into(),
                elapsed: std::time::Duration::ZERO,
            })
        });
        aec_core::fixed::verify(&Client(response), 17327, Some("198.51.100.7"))
            .map_err(|message| Fault::new("FIXED_PROBE", message))
    }

    struct Gate {
        verified: Cell<bool>,
        probes: RefCell<VecDeque<Result<(), Fault>>>,
        reopen: RefCell<VecDeque<Result<(), Fault>>>,
        calls: RefCell<Vec<&'static str>>,
        block_fails: bool,
    }
    impl Gate {
        fn new(verified: bool, probes: Vec<Result<(), Fault>>) -> Self {
            Self {
                verified: Cell::new(verified),
                probes: RefCell::new(probes.into()),
                reopen: RefCell::new(vec![evidence(Some("ip=198.51.100.7\n"))].into()),
                calls: RefCell::new(Vec::new()),
                block_fails: false,
            }
        }
    }
    impl FixedGate for Gate {
        fn verified(&self) -> bool {
            self.verified.get()
        }
        fn verify(&self) -> Result<(), Fault> {
            self.calls.borrow_mut().push("verify");
            self.probes.borrow_mut().pop_front().expect("probe fixture")
        }
        fn block(&self) -> Result<(), Fault> {
            self.calls.borrow_mut().push("block");
            self.verified.set(false);
            if self.block_fails {
                Err(Fault::new("BLOCK", "control unavailable"))
            } else {
                Ok(())
            }
        }
        fn open(&self) -> Result<(), Fault> {
            self.calls.borrow_mut().push("open_saved_path");
            // Like LivePlatform::open_fixed: a second exact probe is required before release.
            self.reopen
                .borrow_mut()
                .pop_front()
                .expect("reopen fixture")?;
            self.verified.set(true);
            Ok(())
        }
    }

    #[test]
    fn transient_failure_recovers_only_after_fresh_exact_evidence() {
        let gate = Gate::new(
            true,
            vec![evidence(None), evidence(Some("ip=198.51.100.7\n"))],
        );
        assert!(matches!(
            poll_fixed(&gate),
            FixedPoll::Blocked {
                was_verified: true,
                block_error: None,
                ..
            }
        ));
        assert!(!gate.verified());
        assert!(
            matches!(poll_fixed(&gate), FixedPoll::Recovered),
            "restored evidence must reopen the previously blocked saved path"
        );
        assert!(gate.verified());
        assert_eq!(
            *gate.calls.borrow(),
            ["verify", "block", "verify", "open_saved_path"]
        );
    }

    #[test]
    fn wrong_or_unknown_exit_never_reopens() {
        for probe in [
            evidence(None),
            evidence(Some("ip=198.51.100.8\n")),
            evidence(Some("invalid")),
        ] {
            let gate = Gate::new(false, vec![probe]);
            assert!(matches!(poll_fixed(&gate), FixedPoll::Blocked { .. }));
            assert!(!gate.verified());
            assert_eq!(*gate.calls.borrow(), ["verify", "block"]);
        }
    }

    #[test]
    fn failed_second_probe_or_reopen_stays_blocked() {
        for failure in [
            evidence(None),
            evidence(Some("ip=198.51.100.8\n")),
            Err(Fault::new("SELECT", "selection failed")),
        ] {
            let gate = Gate::new(false, vec![evidence(Some("ip=198.51.100.7\n"))]);
            *gate.reopen.borrow_mut() = vec![failure].into();
            assert!(matches!(poll_fixed(&gate), FixedPoll::Blocked { .. }));
            assert!(!gate.verified());
            assert_eq!(*gate.calls.borrow(), ["verify", "open_saved_path", "block"]);
        }
    }

    #[test]
    fn healthy_verified_gate_does_not_reselect_or_close_connections() {
        let gate = Gate::new(true, vec![evidence(Some("ip=198.51.100.7\n"))]);
        assert!(matches!(poll_fixed(&gate), FixedPoll::Healthy));
        assert!(gate.verified());
        assert_eq!(*gate.calls.borrow(), ["verify"]);
    }

    #[test]
    fn healthy_exit_retries_a_previously_failed_report_without_reopening() {
        let gate = Gate::new(true, vec![evidence(Some("ip=198.51.100.7\n"))]);
        let poll = poll_fixed(&gate);
        assert!(
            needs_refresh(&poll, Mode::AgentUnverified),
            "a stale unverified report needs a new complete check after exact exit evidence"
        );
        assert!(!needs_refresh(&poll, Mode::AgentReady));
        assert_eq!(*gate.calls.borrow(), ["verify"]);
    }

    #[test]
    fn block_failure_is_not_hidden() {
        let mut gate = Gate::new(true, vec![evidence(None)]);
        gate.block_fails = true;
        assert!(matches!(
            poll_fixed(&gate),
            FixedPoll::Blocked {
                block_error: Some(_),
                ..
            }
        ));
        assert!(!gate.verified());
    }
}
