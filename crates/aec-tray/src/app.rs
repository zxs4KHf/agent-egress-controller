//! Everything the tray does that is not window plumbing: where files live, and the worker thread
//! that owns the controller. Commands run one at a time, off the UI thread; the UI reads [`Shared`].

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use aec_core::controller::{Controller, EnterOptions, Mode, Settings};
use aec_core::core::process::CoreManager;
use aec_core::core::CorePorts;
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
            ports: CorePorts::default(),
        };
        LivePlatform::new(core, &self.data_dir)
    }

    pub fn report_path(&self) -> PathBuf {
        self.data_dir.join("检测结果.txt")
    }
}

#[derive(Debug)]
pub enum Cmd {
    Enter,
    Open(String),
    Recheck,
    /// Leave Agent mode, recover an interrupted session or retry a failed restore.
    Exit,
    Ack,
    CopyPrompts,
    /// Restore if needed and reply; the worker keeps running (log-off that may still be cancelled).
    Restore(Sender<()>),
    /// Restore if needed, then reply and stop so the UI can quit.
    Shutdown(Sender<()>),
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
    pub mode: Mode,
    /// Set while a command runs; menu actions are disabled.
    pub busy: Option<&'static str>,
    pub configured: bool,
    /// (entry id, menu label)
    pub entries: Vec<(String, String)>,
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
            s.busy = busy;
            s.configured = profile::validate(&set).is_empty();
            s.entries = p
                .entries
                .iter()
                .map(|e| (e.id().to_string(), text::entry_label(e)))
                .collect();
            s.has_report = self.controller.state().last_report.is_some();
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
            let label = match &cmd {
                Cmd::Enter => "正在进入 Agent 模式…",
                Cmd::Open(_) => "正在打开并检测…",
                Cmd::Recheck => "正在重新检测…",
                Cmd::Exit | Cmd::Restore(_) | Cmd::Shutdown(_) => "正在退出并恢复…",
                Cmd::Ack => "正在处理…",
                Cmd::CopyPrompts => "正在生成提示词…",
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
            Cmd::Enter => {
                let set = load_set(&data_dir);
                if !profile::validate(&set).is_empty() {
                    self.note(
                        "还没有设置出口",
                        "先在托盘菜单点「设置出口和时区…」。",
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
            Cmd::Recheck => match self.controller.recheck(&agent_profile(&data_dir)) {
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
        }
        false
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
        let gate = TakeoverGate::evaluate(
            Settings::load(data_dir).takeover_enabled,
            &st.foreign_writers,
            true,
        );
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

/// The settings page's view of this installation.
pub struct Backend {
    pub data_dir: PathBuf,
    pub shared: SharedRef,
    pub notify: Box<dyn Fn() + Send + Sync>,
}

impl Backend {
    fn locked(&self) -> bool {
        self.shared.lock().expect("lock").mode != Mode::Daily
    }
}

impl SetupBackend for Backend {
    fn view(&self) -> SetupView {
        let set = load_set(&self.data_dir);
        let user = WindowsCredentials
            .user_name(setup::CREDENTIAL_REF)
            .ok()
            .flatten();
        setup::view(
            &set,
            user.as_deref(),
            Settings::load(&self.data_dir).takeover_enabled,
            self.locked(),
        )
    }

    fn save(&self, form: ExitForm) -> Result<(), Vec<FieldError>> {
        let fe = |m: String| {
            vec![FieldError {
                path: "form".into(),
                message: m,
            }]
        };
        if self.locked() {
            return Err(fe("现在处于 Agent 模式，先退出再修改设置".into()));
        }
        let mut set = load_set(&self.data_dir);
        let creds = WindowsCredentials;
        let user = creds.user_name(setup::CREDENTIAL_REF).map_err(fe)?;
        let (change, takeover) = setup::apply(&mut set, &form, user.as_deref())?;
        match change {
            CredentialChange::Store { user, pass } => creds
                .store(setup::CREDENTIAL_REF, &user, &pass)
                .map_err(fe)?,
            CredentialChange::Delete => {
                creds.delete(setup::CREDENTIAL_REF).map_err(fe)?;
            }
            CredentialChange::Keep => {}
        }
        ProfileStore::new(&self.data_dir)
            .save(&set)
            .map_err(|e| fe(e.to_string()))?;
        Settings {
            takeover_enabled: takeover,
        }
        .save(&self.data_dir)
        .map_err(|e| fe(e.to_string()))?;
        self.shared.lock().expect("lock").configured = true;
        (self.notify)();
        Ok(())
    }
}
