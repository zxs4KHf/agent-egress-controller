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
    Startup(bool, Sender<Result<(), Vec<FieldError>>>),
    Shortcuts(Sender<Result<(), Vec<FieldError>>>),
    Save(Box<ExitForm>, Sender<Result<(), Vec<FieldError>>>),
    Rules(
        aec_core::dashboard::RulesForm,
        Sender<Result<(), Vec<FieldError>>>,
    ),
    Path(String, Sender<Result<(), Vec<FieldError>>>),
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
    pub switches: Vec<(String, String, bool)>,
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
        std::fs::create_dir_all(&layout.data_dir)
            .map_err(|e| format!("{}: {e}", layout.data_dir.display()))?;
        let controller = Controller::open(
            layout.platform(),
            &layout.data_dir,
            Settings::load(&layout.data_dir),
        )
        .map_err(|e| e.to_string())?;
        let w = Self {
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
                Cmd::Startup(..) | Cmd::Shortcuts(..) => "正在保存启动入口…",
                Cmd::Save(..) | Cmd::Rules(..) => "正在保存设置…",
                Cmd::Path(..) => "正在核对固定出口…",
                Cmd::Enter => "正在进入 Agent 模式…",
                Cmd::Open(_) => "正在打开并检测…",
                Cmd::Switch { .. } => "正在切换路由并重新检测…",
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
                "系统代理被其他程序改写",
                &format!(
                    "当前系统代理为 {found}。Agent 就绪状态已撤销；请关闭冲突程序的系统代理开关，再退出并重新进入 Agent 模式。"
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
            self.last_fixed_probe = std::time::Instant::now();
            if let Err(e) = self.controller.platform().verify_fixed(&p) {
                let was_verified = self.controller.platform().fixed_verified();
                let blocked = self.controller.platform().block_fixed();
                self.shared.lock().expect("lock").fixed_blocked = true;
                let _ = self.controller.invalidate_routing_checks();
                if let Err(block_error) = blocked {
                    self.note(
                        "固定出口失去验证，阻断操作失败",
                        &format!(
                            "{}；{}。请停止使用受保护服务并恢复会话。",
                            e.message, block_error.message
                        ),
                        true,
                    );
                } else if was_verified {
                    self.note("固定出口已阻断", &e.message, true);
                }
            }
        }
        self.write_report();
        self.publish(None);
    }

    fn switch_route(&mut self, group: &str, member: &str) {
        if agent_profile(&self.layout.data_dir).egress.fixed_exit {
            self.note(
                "请使用看板管理规则",
                "固定出口模式的变更需要阻断、断开旧连接和重新核对出口。",
                true,
            );
            return;
        }
        let result = (|| -> Result<(), String> {
            let store = ProfileStore::new(&self.layout.data_dir);
            let mut set = store.load().map_err(|e| e.to_string())?;
            let g = set.agent[0]
                .routing
                .groups
                .iter_mut()
                .find(|g| g.name == group && g.switchable)
                .ok_or("路由组不可切换")?;
            if !g.members.iter().any(|m| m == member) {
                return Err("所选出站不属于这个组".into());
            }
            let previous = g
                .selected
                .clone()
                .or_else(|| g.members.first().cloned())
                .ok_or("路由组为空")?;
            g.selected = Some(member.into());
            self.controller
                .invalidate_routing_checks()
                .map_err(|e| e.to_string())?;
            let core = &self.controller.platform().core;
            core.select(&aec_core::windows::WindowsHost, group, member)
                .map_err(|e| e.to_string())?;
            if let Err(e) = store.save(&set) {
                let rollback = core.select(&aec_core::windows::WindowsHost, group, &previous);
                return Err(format!(
                    "选择保存失败：{e}；回退{}",
                    if rollback.is_ok() {
                        "成功"
                    } else {
                        "失败，请退出 Agent 模式"
                    }
                ));
            }
            self.controller
                .recheck(&set.agent[0])
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        self.write_report();
        match result {
            Ok(()) => self.note(
                "路由已切换",
                "新连接使用所选出站；已重新检测，请查看检测结果。现有连接保留原路由。",
                false,
            ),
            Err(e) => self.note("路由切换未完成", &e, true),
        }
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
}

impl SetupBackend for Backend {
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
        let set = load_set(&self.data_dir);
        let user = WindowsCredentials
            .user_name(setup::CREDENTIAL_REF)
            .ok()
            .flatten();
        let tunnel_stored = WindowsCredentials
            .exists(setup::TUNNEL_CREDENTIAL_REF)
            .unwrap_or(false);
        setup::view(
            &set,
            &setup::ViewInput {
                core_mixed_port: Settings::load(&self.data_dir).core_mixed_port,
                stored_user: user.as_deref(),
                tunnel_password_stored: tunnel_stored,
                takeover_enabled: Settings::load(&self.data_dir).takeover_enabled,
                locked: self.locked(),
            },
        )
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
