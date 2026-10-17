//! `aec-tray`: the first usable version of Agent Egress Controller. A tray icon whose colour is the
//! mode, a menu for every action, and a local settings page for the user's own exit.
//!
//! The window thread only draws and dispatches; the controller lives on a worker thread
//! ([`app::Worker`]) so checks never freeze the menu.

#![cfg_attr(not(test), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod startup;
mod text;
#[cfg(windows)]
mod update;
#[cfg(windows)]
mod win;

#[cfg(not(windows))]
fn main() {
    eprintln!("aec-tray runs on Windows only");
}

#[cfg(windows)]
fn main() {
    match aec_core::independent_launch::enter() {
        Ok(Some(code)) => std::process::exit(code),
        Ok(None) => {}
        Err(error) => {
            win::message_box(std::ptr::null_mut(), &error, "Agent Egress 启动失败");
            std::process::exit(1);
        }
    }
    if let Err(error) = aec_core::shell_identity::initialize_tray_process() {
        win::message_box(std::ptr::null_mut(), &error, "Agent Egress 启动失败");
        std::process::exit(1);
    }
    for arg in std::env::args().skip(1) {
        let operation = match arg.as_str() {
            "--enable-autostart" => Some(startup::set(true)),
            "--disable-autostart" => Some(startup::set(false)),
            "--create-shortcuts" => Some(startup::shortcuts()),
            _ => None,
        };
        if let Some(result) = operation {
            if let Err(e) = result {
                win::message_box(std::ptr::null_mut(), &e, "Agent 出口控制器");
                std::process::exit(1);
            }
            return;
        }
    }
    tray::run();
}

#[cfg(windows)]
mod tray {
    use std::cell::RefCell;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING,
        NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use aec_core::controller::Mode;
    use aec_core::setup::SetupServer;

    use crate::app::{Backend, Cmd, Layout, Shared, SharedRef, Worker};
    use crate::text;
    use crate::win::{self, wide};

    const WM_TRAY: u32 = WM_APP + 1;
    const WM_REFRESH: u32 = WM_APP + 2;
    const WM_QUIT_READY: u32 = WM_APP + 3;

    const ID_ENTER: usize = 1;
    const ID_RECHECK: usize = 2;
    const ID_REPORT: usize = 3;
    const ID_PROMPTS: usize = 4;
    const ID_EXIT: usize = 5;
    const ID_ACK: usize = 6;
    const ID_SETTINGS: usize = 7;
    const ID_DATA: usize = 8;
    const ID_QUIT: usize = 9;
    const ID_DASHBOARD: usize = 10;
    const ID_STARTUP: usize = 11;
    const ID_SHORTCUTS: usize = 12;
    const ID_PROXY_HELP: usize = 13;
    const ID_UPDATE: usize = 14;
    const ID_UPDATE_UI: usize = 15;
    const ID_COOPERATION: usize = 16;
    const ID_ENTRY_BASE: usize = 100;
    const ID_SWITCH_BASE: usize = 10000;
    const TIMER_REFRESH: usize = 1;

    const GRAY: (u8, u8, u8) = (138, 145, 141);
    const GREEN: (u8, u8, u8) = (46, 157, 91);
    const AMBER: (u8, u8, u8) = (224, 161, 0);
    const RED: (u8, u8, u8) = (214, 69, 69);

    struct Ui {
        hwnd: HWND,
        tx: Sender<Cmd>,
        shared: SharedRef,
        layout: Layout,
        icons: [HICON; 4],
        setup: Option<SetupServer>,
        _agent: Option<aec_core::agent::Server>,
        taskbar_created: u32,
        quitting: bool,
    }

    thread_local! {
        static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    }

    /// Borrow the UI state. Never call anything that pumps messages (menus, message boxes,
    /// ShellExecute) inside `f`: the nested messages would find the state borrowed. Such calls are
    /// returned as an [`Effect`] and run after the borrow ends.
    fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
        UI.with(|u| u.try_borrow_mut().ok()?.as_mut().map(f))
    }

    /// Work that pumps messages, done outside the UI borrow.
    enum Effect {
        None,
        Open(String),
        Control(String),
        Warn(String),
    }

    fn run_effect(hwnd: HWND, e: Effect) {
        match e {
            Effect::None => {}
            Effect::Open(target) => win::shell_open(&target),
            Effect::Control(url) => {
                let app = std::env::current_exe()
                    .ok()
                    .and_then(|path| path.parent().map(|dir| dir.join("aec-control.exe")));
                match app.filter(|path| path.is_file()) {
                    Some(app) => {
                        if let Err(error) = std::process::Command::new(app).arg(url).spawn() {
                            win::message_box(
                                hwnd,
                                &format!("桌面控制中心无法启动：{error}"),
                                "Agent 出口控制器",
                            );
                        }
                    }
                    None => win::message_box(
                        hwnd,
                        "桌面控制中心程序不存在，请重新解压完整便携包。",
                        "Agent 出口控制器",
                    ),
                }
            }
            Effect::Warn(text) => win::message_box(hwnd, &text, "Agent 出口控制器"),
        }
    }

    /// Show the tray menu and act on the choice. The menu's modal loop runs outside the UI borrow.
    fn show_menu(hwnd: HWND) {
        let Some((m, s)) = with_ui(|u| u.build_menu()) else {
            return;
        };
        let id = unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            SetForegroundWindow(hwnd);
            let id = TrackPopupMenu(
                m,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                0,
                hwnd,
                std::ptr::null(),
            ) as usize;
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(m);
            id
        };
        if id != 0 {
            let effect = with_ui(|u| u.command(id, &s)).unwrap_or(Effect::None);
            run_effect(hwnd, effect);
        }
    }

    fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut n: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        n.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        n.hWnd = hwnd;
        n.uID = 1;
        n
    }

    impl Ui {
        fn state(&self) -> Shared {
            self.shared.lock().expect("lock").clone()
        }

        fn icon_and_tip(&self, s: &Shared) -> (HICON, String) {
            let icon = if s.busy.is_some() {
                self.icons[2]
            } else {
                match s.mode {
                    Mode::Daily => self.icons[0],
                    Mode::AgentReady => self.icons[1],
                    Mode::AgentUnverified => self.icons[2],
                    Mode::Entering | Mode::Exiting | Mode::RestoreIncomplete => self.icons[3],
                }
            };
            let tip = format!(
                "Agent 出口控制器\n{}",
                s.busy.unwrap_or(text::mode_label(s.mode))
            );
            (icon, tip)
        }

        fn add_icon(&self) {
            let s = self.state();
            let (icon, tip) = self.icon_and_tip(&s);
            let mut n = nid(self.hwnd);
            n.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            n.uCallbackMessage = WM_TRAY;
            n.hIcon = icon;
            win::fill(&mut n.szTip, &tip);
            unsafe { Shell_NotifyIconW(NIM_ADD, &n) };
        }

        fn refresh(&mut self) {
            let (s, mut note, clip) = {
                let mut g = self.shared.lock().expect("lock");
                let note = g.note.take();
                let clip = g.clipboard.take();
                (g.clone(), note, clip)
            };
            if let Some(text) = clip {
                if let Err(e) = win::set_clipboard(self.hwnd, &text) {
                    note = Some(crate::app::Note {
                        title: "复制失败".into(),
                        text: e,
                        error: true,
                    });
                }
            }
            let (icon, tip) = self.icon_and_tip(&s);
            let mut n = nid(self.hwnd);
            n.uFlags = NIF_ICON | NIF_TIP;
            n.hIcon = icon;
            win::fill(&mut n.szTip, &tip);
            if let Some(note) = note {
                n.uFlags |= NIF_INFO;
                win::fill(&mut n.szInfoTitle, &note.title);
                win::fill(&mut n.szInfo, &note.text);
                n.dwInfoFlags = if note.error { NIIF_WARNING } else { NIIF_INFO };
            }
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &n) };
        }

        fn open_page(&mut self, dashboard: bool) -> Effect {
            if self.setup.is_none() {
                let token = match aec_core::crypto::random_hex(16) {
                    Ok(t) => t,
                    Err(e) => return Effect::Warn(format!("设置页打不开：{e}")),
                };
                let backend = Backend {
                    tx: self.tx.clone(),
                    data_dir: self.layout.data_dir.clone(),
                    shared: self.shared.clone(),
                };
                match SetupServer::start(0, token, Arc::new(backend)) {
                    Ok(s) => self.setup = Some(s),
                    Err(e) => return Effect::Warn(format!("本机设置页没能启动：{e}")),
                }
            }
            self.setup.as_ref().map_or(Effect::None, |s| {
                Effect::Control(if dashboard {
                    s.control_tab_url("dashboard")
                } else {
                    s.control_tab_url("setup")
                })
            })
        }

        fn open_proxy_help(&mut self) -> Effect {
            match self.open_page(false) {
                Effect::Control(_) => self
                    .setup
                    .as_ref()
                    .map_or(Effect::None, |s| Effect::Control(s.control_tab_url("help"))),
                effect => effect,
            }
        }

        fn send(&self, cmd: Cmd) {
            let _ = self.tx.send(cmd);
        }

        /// Build the menu for the current state; [`show_menu`] shows it outside the borrow.
        fn build_menu(&self) -> (HMENU, Shared) {
            let s = self.state();
            let idle = s.busy.is_none() && !self.quitting;
            let agent = matches!(s.mode, Mode::AgentReady | Mode::AgentUnverified);
            unsafe {
                let m = CreatePopupMenu();
                let add = |menu: HMENU, flags: u32, id: usize, label: &str| {
                    AppendMenuW(menu, flags, id, wide(label).as_ptr());
                };
                let on = |ok: bool| if ok { MF_STRING } else { MF_STRING | MF_GRAYED };
                let submenu = |parent: HMENU, child: HMENU, label: &str, enabled: bool| {
                    add(parent, MF_POPUP | on(enabled), child as usize, label);
                };
                add(
                    m,
                    MF_STRING | MF_GRAYED,
                    0,
                    s.busy.unwrap_or(text::mode_label(s.mode)),
                );
                AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
                add(m, MF_STRING, ID_DASHBOARD, "打开控制中心");
                match s.mode {
                    Mode::Daily => add(
                        m,
                        on(idle),
                        ID_ENTER,
                        if s.configured {
                            "进入 Agent 模式"
                        } else {
                            "进入 Agent 模式（先设置出口）"
                        },
                    ),
                    Mode::AgentReady | Mode::AgentUnverified => {
                        add(m, on(idle), ID_EXIT, "退出 Agent 模式并恢复");
                    }
                    Mode::RestoreIncomplete => {
                        add(m, on(idle), ID_EXIT, "重试恢复");
                        add(m, on(idle), ID_ACK, "保持现状，回到日常模式");
                    }
                    Mode::Entering | Mode::Exiting => {
                        add(m, on(idle), ID_EXIT, "恢复上次未完成的会话");
                    }
                }
                if agent && !s.entries.is_empty() {
                    let entries = CreatePopupMenu();
                    for (i, (_, label)) in s.entries.iter().enumerate() {
                        add(entries, on(idle), ID_ENTRY_BASE + i, label);
                    }
                    submenu(m, entries, "打开 AI 浏览器或终端", idle);
                }
                if agent && !s.client_cooperation && !s.switches.is_empty() {
                    let routes = CreatePopupMenu();
                    let mut groups: Vec<&str> = Vec::new();
                    for (group, _, _, _) in &s.switches {
                        if !groups.contains(&group.as_str()) {
                            groups.push(group);
                        }
                    }
                    for group in groups {
                        let members = CreatePopupMenu();
                        for (i, (name, member, selected, fixed)) in s.switches.iter().enumerate() {
                            if name != group {
                                continue;
                            }
                            add(
                                members,
                                on(idle) | if *selected { MF_CHECKED } else { 0 },
                                ID_SWITCH_BASE + i,
                                &format!(
                                    "{member}（{}）",
                                    if *fixed { "固定 ISP" } else { "普通出口" }
                                ),
                            );
                        }
                        submenu(routes, members, group, idle);
                    }
                    submenu(m, routes, "出口与分流", idle);
                }
                let checks = CreatePopupMenu();
                add(
                    checks,
                    on(agent && idle),
                    ID_RECHECK,
                    "重新检测（不重载规则）",
                );
                add(checks, on(s.has_report), ID_REPORT, "查看检测结果");
                add(checks, on(idle), ID_PROMPTS, "复制排查提示词");
                add(checks, MF_STRING, ID_PROXY_HELP, "代理共存说明");
                if let Some(summary) = &s.summary {
                    add(checks, MF_STRING | MF_GRAYED, 0, summary);
                }
                submenu(m, checks, "检测与排查", true);
                let settings = CreatePopupMenu();
                add(
                    settings,
                    MF_STRING,
                    ID_COOPERATION,
                    "Clash 站点协作与 DNS/ECS…",
                );
                add(
                    settings,
                    MF_STRING,
                    ID_SETTINGS,
                    if s.client_cooperation {
                        "连接方式：保留客户端系统代理（两种选项…）"
                    } else {
                        "连接方式：客户端系统代理 / AEC 本地接管…"
                    },
                );
                add(
                    settings,
                    MF_STRING
                        | if crate::startup::enabled() {
                            MF_CHECKED
                        } else {
                            0
                        },
                    ID_STARTUP,
                    "登录时自动启动",
                );
                add(
                    settings,
                    on(idle),
                    ID_SHORTCUTS,
                    "修复桌面和开始菜单快捷方式",
                );
                add(settings, MF_STRING, ID_DATA, "打开数据和日志目录");
                AppendMenuW(settings, MF_SEPARATOR, 0, std::ptr::null());
                add(settings, on(idle), ID_UPDATE_UI, "仅更新界面（不停服务）…");
                add(settings, on(idle), ID_UPDATE, "检查完整更新…");
                submenu(m, settings, "设置与维护", true);
                AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
                add(
                    m,
                    on(!self.quitting),
                    ID_QUIT,
                    if s.mode == Mode::Daily {
                        "退出程序"
                    } else {
                        "恢复设置并退出程序"
                    },
                );
                (m, s)
            }
        }

        fn command(&mut self, id: usize, s: &Shared) -> Effect {
            match id {
                ID_ENTER if !s.configured => return self.open_page(false),
                ID_ENTER => self.send(Cmd::Enter),
                ID_RECHECK => self.send(Cmd::Recheck),
                ID_EXIT => self.send(Cmd::Exit),
                ID_ACK => self.send(Cmd::Ack),
                ID_PROMPTS => self.send(Cmd::CopyPrompts),
                ID_REPORT => {
                    return Effect::Open(self.layout.report_path().to_string_lossy().into())
                }
                ID_SETTINGS => return self.open_page(false),
                ID_DASHBOARD | ID_COOPERATION => return self.open_page(true),
                ID_PROXY_HELP => return self.open_proxy_help(),
                ID_UPDATE | ID_UPDATE_UI => {
                    if let Err(error) = crate::update::launch(
                        id == ID_UPDATE_UI,
                        s.mode,
                        s.busy.is_some(),
                        &self.layout.data_dir,
                    ) {
                        return Effect::Warn(error);
                    }
                }
                ID_STARTUP => {
                    let (tx, _rx) = channel();
                    self.send(Cmd::Startup(!crate::startup::enabled(), tx));
                }
                ID_SHORTCUTS => {
                    let (tx, _rx) = channel();
                    self.send(Cmd::Shortcuts(tx));
                }
                ID_DATA => return Effect::Open(self.layout.data_dir.to_string_lossy().into()),
                ID_QUIT => self.quit(),
                n if n >= ID_SWITCH_BASE => {
                    if let Some((group, member, _, _)) = s.switches.get(n - ID_SWITCH_BASE) {
                        self.send(Cmd::Switch {
                            group: group.clone(),
                            member: member.clone(),
                        });
                    }
                }
                n if n >= ID_ENTRY_BASE => {
                    if let Some((entry, _)) = s.entries.get(n - ID_ENTRY_BASE) {
                        self.send(Cmd::Open(entry.clone()));
                    }
                }
                _ => {}
            }
            Effect::None
        }

        /// Restore on the worker, then close once it answers.
        fn quit(&mut self) {
            self.quitting = true;
            let (done_tx, done_rx) = channel();
            self.send(Cmd::Shutdown(done_tx));
            let hwnd = self.hwnd as usize;
            std::thread::spawn(move || {
                let _ = done_rx.recv_timeout(Duration::from_secs(120));
                unsafe { PostMessageW(hwnd as HWND, WM_QUIT_READY, 0, 0) };
            });
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_CLOSE => {
                with_ui(|u| {
                    if !u.quitting {
                        u.quit();
                    }
                });
                0
            }
            WM_TRAY => {
                let ev = lp as u32;
                if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP {
                    show_menu(hwnd);
                }
                0
            }
            WM_REFRESH => {
                if with_ui(|u| u.refresh()).is_none() {
                    // The state is borrowed further up this stack: try again shortly.
                    SetTimer(hwnd, TIMER_REFRESH, 150, None);
                }
                0
            }
            WM_TIMER if wp == TIMER_REFRESH => {
                KillTimer(hwnd, TIMER_REFRESH);
                PostMessageW(hwnd, WM_REFRESH, 0, 0);
                0
            }
            WM_QUIT_READY => {
                DestroyWindow(hwnd);
                0
            }
            // Log-off or shutdown: restore before Windows ends the session. Waiting here is what
            // keeps the session open long enough; the worker finishes its current step first.
            WM_QUERYENDSESSION => {
                let pending = with_ui(|u| {
                    let (tx, rx) = channel();
                    let _ = u.tx.send(Cmd::Restore(tx));
                    rx
                });
                if let Some(rx) = pending {
                    let _ = rx.recv_timeout(Duration::from_secs(25));
                }
                1
            }
            // Menu ids also arrive as WM_COMMAND, which lets scripts drive the tray in tests.
            WM_COMMAND => {
                let id = wp & 0xffff;
                let effect = with_ui(|u| {
                    let s = u.state();
                    u.command(id, &s)
                });
                run_effect(hwnd, effect.unwrap_or(Effect::None));
                0
            }
            WM_DESTROY => {
                let n = nid(hwnd);
                Shell_NotifyIconW(NIM_DELETE, &n);
                UI.with(|u| u.borrow_mut().take());
                PostQuitMessage(0);
                0
            }
            _ => {
                let recreate = with_ui(|u| u.taskbar_created != 0 && msg == u.taskbar_created)
                    .unwrap_or(false);
                if recreate {
                    // Explorer restarted: the icon has to be added again.
                    with_ui(|u| u.add_icon());
                    return 0;
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
    }

    pub fn run() {
        let name = wide("Local\\AgentEgressControllerTray");
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
        if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            let request = std::env::args().find_map(|a| match a.as_str() {
                "--dashboard" => Some(ID_DASHBOARD),
                "--enter" => Some(ID_ENTER),
                "--quit" => Some(ID_QUIT),
                "--recheck" => Some(ID_RECHECK),
                _ => None,
            });
            if let Some(request) = request {
                let existing = unsafe {
                    FindWindowW(wide("AgentEgressControllerTray").as_ptr(), std::ptr::null())
                };
                if !existing.is_null() {
                    unsafe { PostMessageW(existing, WM_COMMAND, request, 0) };
                    return;
                }
            }
            win::message_box(
                std::ptr::null_mut(),
                "Agent 出口控制器已经在运行，请看任务栏右下角的托盘图标（可能藏在「^」里）。",
                "Agent 出口控制器",
            );
            return;
        }
        if std::env::args().any(|a| a == "--quit" || a == "--recheck") {
            return;
        }

        let layout = Layout::detect();
        let class = wide("AgentEgressControllerTray");
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        unsafe { RegisterClassExW(&wc) };
        let title = wide("Agent Egress Controller");
        // A hidden top-level window: message-only windows do not get WM_QUERYENDSESSION.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            win::message_box(
                std::ptr::null_mut(),
                "托盘窗口创建失败。",
                "Agent 出口控制器",
            );
            return;
        }

        let shared: SharedRef = Arc::new(Mutex::new(Shared {
            fixed_blocked: true,
            mode: Mode::Daily,
            busy: Some("正在启动…"),
            configured: false,
            entries: Vec::new(),
            switches: Vec::new(),
            has_report: false,
            note: None,
            summary: None,
            scope_evidence: None,
            client_cooperation: false,
            clipboard: None,
        }));
        let (tx, rx) = channel::<Cmd>();
        let (init_tx, init_rx) = channel::<Result<(), String>>();
        {
            let (layout, shared, h) = (layout.clone(), shared.clone(), hwnd as usize);
            std::thread::spawn(move || {
                let notify = Box::new(move || unsafe {
                    PostMessageW(h as HWND, WM_REFRESH, 0, 0);
                });
                match Worker::new(layout, shared, notify) {
                    Ok(w) => {
                        let _ = init_tx.send(Ok(()));
                        w.run(rx);
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                    }
                }
            });
        }
        if let Ok(Err(e)) = init_rx.recv() {
            win::message_box(hwnd, &format!("启动失败：{e}"), "Agent 出口控制器");
            unsafe { DestroyWindow(hwnd) };
            return;
        }
        // Observe external system-proxy changes; never compete with another global writer.
        {
            let keep_tx = tx.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(30));
                if keep_tx.send(Cmd::Keep).is_err() {
                    break;
                }
            });
        }

        let tc = wide("TaskbarCreated");
        let agent = aec_core::agent::Server::start(
            &layout.data_dir,
            Arc::new(Backend {
                tx: tx.clone(),
                data_dir: layout.data_dir.clone(),
                shared: shared.clone(),
            }),
        );
        if agent.is_err() {
            win::message_box(
                hwnd,
                "本机 Agent 控制接口未能启动；网络控制继续运行。请从看板查看 Agent 接入状态。",
                "Agent 出口控制器",
            );
        }
        let ui = Ui {
            hwnd,
            tx,
            shared: shared.clone(),
            layout,
            icons: [
                win::dot_icon(GRAY),
                win::dot_icon(GREEN),
                win::dot_icon(AMBER),
                win::dot_icon(RED),
            ],
            setup: None,
            _agent: agent.ok(),
            taskbar_created: unsafe { RegisterWindowMessageW(tc.as_ptr()) },
            quitting: false,
        };
        ui.add_icon();
        UI.with(|u| *u.borrow_mut() = Some(ui));
        let first_run = with_ui(|u| {
            u.refresh();
            let s = u.state();
            // First run: nothing works without an exit, so start there.
            if std::env::args().any(|a| a == "--dashboard") {
                u.open_page(true)
            } else if !s.configured && s.mode == Mode::Daily {
                u.open_page(false)
            } else {
                Effect::None
            }
        });
        run_effect(hwnd, first_run.unwrap_or(Effect::None));
        if std::env::args().any(|a| a == "--enter") {
            with_ui(|u| {
                if u.state().mode == Mode::Daily {
                    let _ = u.tx.send(Cmd::Enter);
                }
            });
        }

        let mut msg: MSG = unsafe { std::mem::zeroed() };
        while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    #[cfg(test)]
    mod menu_tests {
        use super::*;
        use aec_core::{core::CorePorts, local_upstream, profile::*, route_switch};

        #[test]
        fn native_menu_selects_clash_then_xxyun_without_changing_claude_protection() {
            let mut profiles = ProfileSet::default();
            profiles.agent[0].egress.fixed_exit = true;
            profiles.agent[0].egress.expected.ip = Some("198.51.100.8".into());
            profiles.agent[0].targets = vec![Target {
                domain: "claude.ai".into(),
                trace: true,
            }];
            profiles.agent[0].routing.outbounds.push(serde_json::from_value(serde_json::json!({"name":"xxyun","kind":"http","server":"127.0.0.1","port":7892})).unwrap());
            profiles.agent[0].routing.final_target = "xxyun".into();
            profiles = local_upstream::plan(
                &profiles,
                &local_upstream::Command::Save {
                    name: "Clash".into(),
                    host: "127.0.0.1".into(),
                    port: 7890,
                    protocol: UpstreamProto::Http,
                },
                CorePorts::default(),
            )
            .unwrap();
            let original = profiles.agent[0].clone();
            let switches = |set: &ProfileSet| {
                aec_core::dashboard::view(set, "agent_ready", false, false, None)
                    .switches
                    .into_iter()
                    .map(|c| (c.group, c.member, c.selected, c.fixed))
                    .collect()
            };
            let shared = Arc::new(Mutex::new(Shared {
                fixed_blocked: false,
                mode: Mode::AgentReady,
                busy: None,
                configured: true,
                entries: vec![],
                switches: switches(&profiles),
                has_report: false,
                note: None,
                summary: None,
                scope_evidence: None,
                client_cooperation: false,
                clipboard: None,
            }));
            let (tx, rx) = channel();
            let class = wide(&format!("AecMenuFixture-{}", std::process::id()));
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..unsafe { std::mem::zeroed() }
            };
            assert_ne!(unsafe { RegisterClassW(&wc) }, 0);
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    wide("Isolated AEC menu test").as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null());
            struct Cleanup(HWND, Vec<u16>, windows_sys::Win32::Foundation::HINSTANCE);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    unsafe {
                        DestroyWindow(self.0);
                        UnregisterClassW(self.1.as_ptr(), self.2);
                    }
                }
            }
            let _cleanup = Cleanup(hwnd, class, instance);
            UI.with(|cell| {
                *cell.borrow_mut() = Some(Ui {
                    hwnd,
                    tx,
                    shared: shared.clone(),
                    layout: Layout {
                        data_dir: "unused-fixture".into(),
                        mihomo: "unused".into(),
                        relay: "unused".into(),
                    },
                    icons: [std::ptr::null_mut(); 4],
                    setup: None,
                    _agent: None,
                    taskbar_created: 0,
                    quitting: false,
                })
            });
            for desired in ["Clash", "xxyun"] {
                let (menu, snapshot) = with_ui(|ui| ui.build_menu()).unwrap();
                assert!(unsafe { GetMenuItemCount(menu) } <= 12,
                    "The root tray menu must not expand every route, diagnostic and maintenance action");
                let index = snapshot
                    .switches
                    .iter()
                    .position(|(g, m, _, _)| g == local_upstream::GROUP && m == desired)
                    .unwrap();
                let command = (ID_SWITCH_BASE + index) as u32;
                let mut label = [0u16; 256];
                let n = unsafe {
                    GetMenuStringW(
                        menu,
                        command,
                        label.as_mut_ptr(),
                        label.len() as i32,
                        MF_BYCOMMAND,
                    )
                };
                assert!(String::from_utf16_lossy(&label[..n as usize]).contains(desired));
                assert_eq!(
                    unsafe { GetMenuState(menu, command, MF_BYCOMMAND) } & MF_GRAYED,
                    0
                );
                unsafe {
                    SendMessageW(hwnd, WM_COMMAND, command as usize, 0);
                    DestroyMenu(menu);
                }
                let Cmd::Switch { group, member } =
                    rx.recv_timeout(Duration::from_secs(1)).unwrap()
                else {
                    panic!("Menu did not dispatch a route selection")
                };
                assert_eq!(group, local_upstream::GROUP);
                assert_eq!(member, desired);
                profiles = route_switch::plan(&profiles, &route_switch::Form { group, member })
                    .unwrap()
                    .profiles;
                assert_eq!(profiles.agent[0].targets, original.targets);
                assert_eq!(profiles.agent[0].egress, original.egress);
                assert_eq!(
                    local_upstream::view(&profiles).selected.as_deref(),
                    Some(desired)
                );
                shared.lock().unwrap().switches = switches(&profiles);
                let (menu, _) = with_ui(|ui| ui.build_menu()).unwrap();
                assert_ne!(
                    unsafe { GetMenuState(menu, command, MF_BYCOMMAND) } & MF_CHECKED,
                    0
                );
                unsafe {
                    DestroyMenu(menu);
                }
            }
        }
    }
}
