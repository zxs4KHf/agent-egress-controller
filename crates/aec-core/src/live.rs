//! The real machine behind the [`Platform`] trait (Windows).
//!
//! Real global writes: time zone and optional system proxy, only when the controller calls
//! `write_global` behind the per-item takeover gate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::checkpage::{BrowserSignals, CheckPageServer};
use crate::checks::{self, CheckInput, CheckReport, CoreObserver};
use crate::controller::{CloseOutcome, Fault, GlobalItem, GlobalValue, Platform};
use crate::core::config;
use crate::core::connlog::{self, ConnRecord};
use crate::core::preflight::OwnedProcess;
use crate::core::process::{
    http_get_local, process_identity, still_ours, stop_std_handle_inheritance, CoreManager,
};
use crate::credentials::WindowsCredentials;
use crate::entries::{self, InstanceDecision, LaunchRecord};
use crate::probe::{SystemProxyReader, TimezoneReader};
use crate::profile::{AgentProfile, Browser, DetectionMode, Entry};
use crate::providers::{ProviderStore, Purpose};
use crate::snapshot::rfc3339_utc;
use crate::status::{self, EnvironmentStatus};
use crate::store::write_atomic;
use crate::windows::WindowsHost;

/// How long a browser gets to load the check page and report (a fresh profile's first start is slow).
const REPORT_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Default)]
struct EntryRuntime {
    server: Option<CheckPageServer>,
    signals: Option<BrowserSignals>,
}

pub struct LivePlatform {
    fixed_verified: std::sync::atomic::AtomicBool,
    pub data_dir: PathBuf,
    pub core: CoreManager,
    entries: Mutex<HashMap<String, EntryRuntime>>,
}

fn find_browser(b: Browser) -> Option<PathBuf> {
    let exe = match b {
        Browser::Edge => "msedge.exe",
        Browser::Chrome => "chrome.exe",
    };
    crate::windows::app_path(exe).or_else(|| {
        entries::standard_locations(b, &|k| std::env::var(k).ok())
            .into_iter()
            .find(|p| p.is_file())
    })
}

/// Start a long-lived program that must not keep any of our handles. Without explicit null stdio,
/// std hands the child inheritable copies of our standard handles; a browser then holds the
/// caller's pipe open for as long as it runs (observed 2026-10-11 with Edge and `| Out-String`).
fn spawn_detached(exe: &Path, args: &[String]) -> Result<u32, Fault> {
    use std::process::{Command, Stdio};
    stop_std_handle_inheritance();
    let child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Fault::new("ENTRY_START_FAILED", format!("{}: {e}", exe.display())))?;
    Ok(child.id())
}

fn wait_exit(pid: u32, created: u64, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match process_identity(pid) {
            Some((_, c)) if c == created => {}
            _ => return true,
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

impl LivePlatform {
    pub fn fixed_verified(&self) -> bool {
        self.fixed_verified
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn block_fixed(&self) -> Result<(), Fault> {
        self.fixed_verified
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // Both steps are attempted: failure to select must not preserve existing connections.
        // Only the relay's own connections are cut. A protected site loses its path when the exit
        // is unverified; traffic that never used the exit keeps running, as the legacy chain did.
        let gate = self.core.select(&WindowsHost, "relay", "REJECT");
        let close = self.core.close_relay_connections(&WindowsHost);
        gate.and(close).map_err(|e| fault(e, "FIXED_BLOCK_FAILED"))
    }

    pub fn verify_fixed(&self, profile: &AgentProfile) -> Result<(), Fault> {
        let port = crate::fixed::probe_port(&self.core.ports)
            .ok_or_else(|| Fault::new("CFG_INVALID", "probe port collision"))?;
        crate::fixed::verify(
            &crate::http::WinHttpClient,
            port,
            profile.egress.expected.ip.as_deref(),
        )
        .map_err(|e| Fault::new("FIXED_EXIT_UNVERIFIED", e))
    }

    /// Called only with the gate blocked. Unknown evidence leaves it blocked.
    pub fn open_fixed(&self, profile: &AgentProfile) -> Result<(), Fault> {
        let member = crate::fixed::path_member(
            profile,
            profile
                .egress
                .connection_path
                .as_deref()
                .unwrap_or("direct"),
        )
        .ok_or_else(|| Fault::new("CFG_INVALID", "connection path unavailable"))?;
        self.core
            .select(&WindowsHost, crate::fixed::FIRST_HOP, &member)
            .map_err(|e| fault(e, "FIXED_PATH_FAILED"))?;
        self.verify_fixed(profile)?;
        self.core
            .select(&WindowsHost, "relay", crate::fixed::TRANSPORT)
            .map_err(|e| fault(e, "FIXED_OPEN_FAILED"))?;
        self.fixed_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn dev(repo_root: &Path, data_dir: &Path) -> Self {
        Self::new(CoreManager::dev(repo_root, data_dir), data_dir)
    }

    /// Any layout: the core manager says where the binaries are.
    pub fn new(core: CoreManager, data_dir: &Path) -> Self {
        Self {
            fixed_verified: std::sync::atomic::AtomicBool::new(false),
            data_dir: data_dir.to_path_buf(),
            core,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn launch_cli(
        &self,
        id: &str,
        command: &str,
        extra_env: &std::collections::BTreeMap<String, String>,
    ) -> Result<OwnedProcess, Fault> {
        use crate::cli;
        // What a new terminal would see: PATH from the registry, not this process's possibly stale copy.
        let dirs = cli::split_path(
            &crate::windows::fresh_path()
                .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default()),
        );
        let pathext: Vec<String> = std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .map(str::to_string)
            .collect();
        let exe = cli::resolve(command, &dirs, &pathext, &|p| p.is_file(), &|d| {
            cli::is_shim_dir(d)
        })
        .ok_or_else(|| {
            Fault::new(
                "ENTRY_NOT_INSTALLED",
                format!("'{command}' was not found on PATH outside egress shim directories"),
            )
        })?;
        let env = cli::entry_env(
            self.core.ports.mixed,
            &dirs,
            &|d| cli::is_shim_dir(d),
            extra_env,
        );
        let pid = cli::launch_in_new_console(&exe, &env, None)
            .map_err(|e| Fault::new("ENTRY_START_FAILED", format!("{}: {e}", exe.display())))?;
        std::thread::sleep(Duration::from_millis(300));
        let (cmd_exe, created) = process_identity(pid).ok_or_else(|| {
            Fault::new(
                "ENTRY_START_FAILED",
                format!("terminal pid {pid} exited right away"),
            )
        })?;
        Ok(OwnedProcess {
            role: format!("entry:cli:{id}"),
            pid,
            exe: cmd_exe,
            created,
            started_at_unix_ms: self.now_ms(),
        })
    }

    fn launch_browser(
        &self,
        profile: &AgentProfile,
        id: &str,
        browser: Browser,
        open_check_page: bool,
        extra: &[String],
    ) -> Result<OwnedProcess, Fault> {
        let name = entries::browser_name(browser);
        let exe = find_browser(browser).ok_or_else(|| {
            Fault::new(
                "ENTRY_NOT_INSTALLED",
                format!("{name} is not installed (nothing is downloaded)"),
            )
        })?;
        let udd = entries::user_data_dir(&self.data_dir, browser);
        std::fs::create_dir_all(&udd)
            .map_err(|e| Fault::new("IO_WRITE", format!("{}: {e}", udd.display())))?;
        let record_path = udd.join(entries::LAUNCH_RECORD);
        let record: Option<LaunchRecord> = std::fs::read(&record_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());

        let server = if open_check_page {
            let local_only = profile.detection.mode == DetectionMode::LocalOnly;
            let stun = ProviderStore::new(&self.data_dir)
                .load()
                .unwrap_or_default()
                .usable(Purpose::Stun, local_only)
                .next()
                .map(|p| p.url.clone());
            let token = crate::crypto::random_hex(12)
                .map_err(|e| Fault::new("ENTRY_START_FAILED", e.to_string()))?;
            Some(
                CheckPageServer::start(entries::CHECK_PAGE_PORT, token, stun).map_err(|e| {
                    Fault::new(
                        "PORT_IN_USE",
                        format!("check page port {}: {e}", entries::CHECK_PAGE_PORT),
                    )
                })?,
            )
        } else {
            None
        };
        let url = server.as_ref().map(|s| s.url());
        let args = entries::environment_browser_args(
            profile,
            &self.data_dir,
            browser,
            self.core.ports.mixed,
            extra,
            url.as_deref(),
        )
        .map_err(|code| Fault::new(code, "浏览器设置有冲突或尚未恢复，请先在看板处理"))?;

        let running = crate::windows::chromium_profile_in_use(&udd);
        let owned = match entries::decide(running, record.as_ref(), &args) {
            InstanceDecision::AskUserToClose { reason } => {
                return Err(Fault::new("ENTRY_BUSY", format!("{reason}; close the {name} window that uses {} and try again (it is never closed for you)", udd.display())))
            }
            InstanceDecision::OpenWindow => {
                let r = record.expect("decided on a record");
                let owned = OwnedProcess { role: format!("entry:browser:{id}"), pid: r.pid, exe: r.exe.clone(), created: r.created, started_at_unix_ms: self.now_ms() };
                if !still_ours(&owned) {
                    return Err(Fault::new("ENTRY_BUSY", format!("the {name} instance on {} is not the one this application started", udd.display())));
                }
                spawn_detached(&exe, &args)?;
                owned
            }
            InstanceDecision::Start => {
                let pid = spawn_detached(&exe, &args)?;
                std::thread::sleep(Duration::from_millis(500));
                let (exe_seen, created) = process_identity(pid).ok_or_else(|| {
                    Fault::new("ENTRY_START_FAILED", format!("{name} pid {pid} exited right away (another instance may have taken the launch)"))
                })?;
                let rec = LaunchRecord { exe: exe_seen.clone(), args: args.clone(), pid, created };
                write_atomic(&record_path, &serde_json::to_vec_pretty(&rec).expect("serializes")).map_err(|e| Fault::new("IO_WRITE", e.to_string()))?;
                OwnedProcess { role: format!("entry:browser:{id}"), pid, exe: exe_seen, created, started_at_unix_ms: self.now_ms() }
            }
        };
        self.entries.lock().expect("lock").insert(
            id.to_string(),
            EntryRuntime {
                server,
                signals: None,
            },
        );
        Ok(owned)
    }

    fn core_reachable(&self) -> Result<(), String> {
        let secret = std::fs::read_to_string(self.core.config_path())
            .ok()
            .and_then(|t| {
                t.lines().find_map(|l| {
                    l.strip_prefix("secret: ")
                        .map(|v| v.trim_matches('"').to_string())
                })
            })
            .ok_or("this application's core is not running")?;
        match http_get_local(self.core.ports.controller, "/version", Some(&secret)) {
            Ok((200, _)) => Ok(()),
            other => Err(format!("controller did not answer: {other:?}")),
        }
    }
}

/// Reads this application's core log from a byte offset on. The path is fixed when the observer is
/// made, so a run that crosses midnight UTC still reads the file it started with.
pub struct CoreLogObserver {
    pub log: PathBuf,
    pub dns_port: u16,
}

impl CoreLogObserver {
    pub fn for_core(core: &CoreManager) -> Self {
        Self {
            log: core.log_path(),
            dns_port: core.ports.dns,
        }
    }
}

impl CoreObserver for CoreLogObserver {
    fn mark(&self) -> u64 {
        std::fs::metadata(&self.log).map(|m| m.len()).unwrap_or(0)
    }

    fn records_since(&self, mark: u64) -> Result<Vec<ConnRecord>, String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f =
            std::fs::File::open(&self.log).map_err(|e| format!("{}: {e}", self.log.display()))?;
        f.seek(SeekFrom::Start(mark)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        Ok(connlog::parse(&String::from_utf8_lossy(&bytes)))
    }

    fn dns_listening(&self) -> Result<bool, String> {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.dns_port));
        Ok(std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(1)).is_ok())
    }
}

fn fault(e: impl std::fmt::Display, code: &str) -> Fault {
    Fault::new(code, e.to_string())
}

impl Platform for LivePlatform {
    fn cooperation_check(&self, profile: &AgentProfile) -> checks::CheckResult {
        let result = (|| {
            let root = crate::cooperation::windows::client_root().ok()?;
            let runtime = crate::cooperation::windows::ClientRuntime::open(&root).ok()?;
            let GlobalValue::SystemProxy { value } =
                self.read_global(GlobalItem::SystemProxy).ok()?
            else {
                return None;
            };
            Some(crate::cooperation::scope_check(
                &runtime,
                &crate::cooperation::Selection::for_profile(profile),
                &value,
            ))
        })();
        result.unwrap_or_else(|| {
            checks::CheckResult::new(
                "G7",
                checks::Kind::Hard,
                checks::Status::Unknown,
                Some("cooperation_controller_or_entry_unavailable"),
            )
        })
    }
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn correlation_id(&self) -> String {
        crate::crypto::random_hex(4).unwrap_or_else(|_| format!("{:x}", self.now_ms()))
    }

    fn read_status(&self) -> EnvironmentStatus {
        status::collect(&WindowsHost, Some(&self.data_dir))
    }

    fn core_preflight(&self, profile: &AgentProfile, direct_test: bool) -> Result<(), Fault> {
        if !direct_test {
            config::relay_config(profile, &self.core.ports, &WindowsCredentials)
                .map_err(|e| fault(e, "CFG_INVALID"))?;
            // The core's own configuration is built here so a template or credential problem is
            // reported while nothing has been changed yet.
            config::from_profile(
                profile,
                &self.core.ports,
                "<preflight>",
                &WindowsCredentials,
            )
            .map_err(|e| fault(e, "CFG_INVALID"))?;
            // The user's own proxy (split mode) is not probed here: when it is down, non-target
            // traffic goes direct and the core notices it coming back within 15 seconds, so a
            // client that is switched off is no reason to refuse Agent mode.
            if profile.routing.uses_geodata() {
                self.core
                    .geodata_ready()
                    .map_err(|e| Fault::new(e.code, e.message))?;
            }
            if !self.core.relay_bin.is_file() {
                return Err(Fault::new(
                    "RELAY_BINARY_MISSING",
                    format!("{} not found", self.core.relay_bin.display()),
                ));
            }
        }
        self.core
            .preflight(&WindowsHost)
            .map_err(|e| Fault::new(e.code, e.message))
    }

    fn core_up(&self, profile: &AgentProfile, direct_test: bool) -> Result<(), Fault> {
        // A previous live session can carry different relay candidates and gate topology. Do not
        // adopt it for a new controller session: recovery must finish before entering again.
        if self.core.status(&WindowsHost).alive || self.core.status(&WindowsHost).relay_alive {
            return Err(Fault::new(
                "CORE_SESSION_ACTIVE",
                "已有内核或 relay 会话；请先恢复并退出该会话，再进入 Agent 模式",
            ));
        }
        let group = profile
            .routing
            .primary_group_name()
            .unwrap_or("agent-egress")
            .to_string();
        let result = if direct_test {
            self.core
                .up(&WindowsHost, |p, s| Ok(config::direct_test(p, s, &group)))
        } else {
            let relay = config::relay_config(profile, &self.core.ports, &WindowsCredentials)
                .map_err(|e| fault(e, "CFG_INVALID"))?;
            self.core.up_with_relay(&WindowsHost, &relay, |p, s| {
                config::from_profile(profile, p, s, &WindowsCredentials).map_err(|e| e.to_string())
            })
        };
        result.map_err(|e| Fault::new(e.code, e.message))?;
        if !direct_test {
            if let Err(e) = self.core.apply_selections(&WindowsHost, &profile.routing) {
                let _ = self.core.down(&WindowsHost);
                return Err(Fault::new(e.code, e.message));
            }
            if profile.egress.fixed_exit {
                self.block_fixed()?;
                // Failed evidence keeps the core blocked and alive, permitting another path.
                let _ = self.open_fixed(profile);
            }
        }
        Ok(())
    }

    fn core_down(&self) -> Result<(), Fault> {
        self.core
            .down(&WindowsHost)
            .map(|_| ())
            .map_err(|e| Fault::new(e.code, e.message))
    }

    fn core_port(&self) -> u16 {
        self.core.ports.mixed
    }

    fn run_checks(&self, profile: &AgentProfile) -> CheckReport {
        let providers = ProviderStore::new(&self.data_dir)
            .load()
            .unwrap_or_default();
        let observer = CoreLogObserver::for_core(&self.core);
        let input = CheckInput {
            profile,
            providers: &providers,
            proxy_port: self.core.ports.mixed,
            core: self.core_reachable(),
            observer: Some(&observer),
            system_dns: crate::windows::system_dns_servers().map_err(|e| e.to_string()),
            generated_at: rfc3339_utc(self.now_ms() / 1000),
        };
        let mut report = checks::run(&input, &crate::http::WinHttpClient);
        if let Err(code) = crate::browser_environment::launch_flags(&self.data_dir) {
            report.results.push(crate::checks::CheckResult::new(
                "BROWSER_ENV",
                crate::checks::Kind::Hard,
                crate::checks::Status::Unknown,
                Some(code),
            ));
        }
        if profile.egress.fixed_exit {
            report.results.push(crate::checks::CheckResult::new(
                "FIXED",
                crate::checks::Kind::Hard,
                if self.fixed_verified() {
                    crate::checks::Status::Pass
                } else {
                    crate::checks::Status::Unknown
                },
                if self.fixed_verified() {
                    None
                } else {
                    Some("fixed_exit_blocked")
                },
            ));
        }
        report.results.push(crate::desktop::observe(
            profile,
            self.core.ports.mixed,
            &crate::desktop::WindowsDesktopProbe(&self.core),
        ));
        report
    }

    fn launch_entry(&self, profile: &AgentProfile, entry_id: &str) -> Result<OwnedProcess, Fault> {
        match profile.entries.iter().find(|e| e.id() == entry_id) {
            Some(Entry::Browser {
                browser,
                open_check_page,
                extra_args,
                ..
            }) => self.launch_browser(profile, entry_id, *browser, *open_check_page, extra_args),
            Some(Entry::Cli { command, env, .. }) => self.launch_cli(entry_id, command, env),
            None => Err(Fault::new(
                "ENTRY_UNKNOWN",
                format!("the profile has no entry '{entry_id}'"),
            )),
        }
    }

    fn entry_signals(&self, entry_id: &str) -> Option<BrowserSignals> {
        let server = {
            let mut map = self.entries.lock().expect("lock");
            let rt = map.get_mut(entry_id)?;
            if rt.signals.is_some() {
                return rt.signals.clone();
            }
            rt.server.take()?
        };
        // Wait without holding the lock; the server stops when it goes out of scope.
        let signals = server.wait(REPORT_TIMEOUT);
        if let Some(rt) = self.entries.lock().expect("lock").get_mut(entry_id) {
            rt.signals = signals.clone();
        }
        signals
    }

    fn browser_profile(&self, profile: &AgentProfile) -> AgentProfile {
        crate::browser_environment::effective_profile(&self.data_dir, profile)
            .unwrap_or_else(|_| profile.clone())
    }

    fn close_entry(&self, process: &OwnedProcess) -> Result<CloseOutcome, Fault> {
        let id = process
            .role
            .rsplit(':')
            .next()
            .unwrap_or_default()
            .to_string();
        self.entries.lock().expect("lock").remove(&id);
        // A CLI console may be a tab of a shared terminal window: never close it (SPEC 4.4 X1).
        if process.role.starts_with("entry:cli:") {
            let open = still_ours(process);
            return Ok(CloseOutcome::LeftOpen(if open {
                format!("terminal pid {} is still open; close it yourself. Its proxy (this application's core) is stopped, so requests from it will fail", process.pid)
            } else {
                format!("terminal pid {} was already closed", process.pid)
            }));
        }
        if !still_ours(process) {
            return Ok(CloseOutcome::Closed);
        }
        let asked = crate::windows::close_windows_of(process.pid);
        if wait_exit(process.pid, process.created, CLOSE_TIMEOUT) {
            Ok(CloseOutcome::Closed)
        } else {
            Err(Fault::new(
                "ENTRY_STILL_RUNNING",
                format!("pid {} ({}) did not exit within {}s after {asked} window(s) were asked to close; close it by hand (it is never forced)", process.pid, process.exe, CLOSE_TIMEOUT.as_secs()),
            ))
        }
    }

    fn timezone_privilege(&self) -> Result<bool, String> {
        crate::windows::token_has_privilege("SeTimeZonePrivilege").map_err(|e| e.to_string())
    }

    fn read_global(&self, item: GlobalItem) -> Result<GlobalValue, Fault> {
        match item {
            GlobalItem::SystemProxy => SystemProxyReader::read(&WindowsHost)
                .map(|value| GlobalValue::SystemProxy { value })
                .map_err(|e| fault(e, "ADAPTER_READ_FAILED")),
            GlobalItem::Timezone => TimezoneReader::read(&WindowsHost)
                .map(|t| GlobalValue::Timezone {
                    windows_id: t.windows_id,
                })
                .map_err(|e| fault(e, "ADAPTER_READ_FAILED")),
        }
    }

    fn write_global(&self, value: &GlobalValue) -> Result<(), Fault> {
        match value {
            GlobalValue::Timezone { windows_id } => crate::windows::set_timezone(windows_id)
                .map_err(|e| fault(e, "ADAPTER_WRITE_FAILED")),
            GlobalValue::SystemProxy { value } => crate::windows::set_system_proxy(value)
                .map_err(|e| fault(e, "ADAPTER_WRITE_FAILED")),
        }
    }
}
