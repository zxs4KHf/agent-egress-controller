//! Agent/Daily mode state machine (SPEC 4.4). The single seam the tests drive: everything that
//! touches the machine goes through [`Platform`].
//!
//! Safety rules enforced here, not in the platform:
//! - global state is written for real only when the takeover gate is open; otherwise every global
//!   change is a dry-run journal entry and [`Platform::write_global`] is never called;
//! - every state transition is persisted before the step it starts, so a crash leaves a record;
//! - on startup a non-daily state is reported, never acted on until the user asks for recovery;
//! - leaving restores only what this application changed, and never overwrites a value somebody
//!   else changed in the meantime.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::checkpage::BrowserSignals;
use crate::checks::{
    browser_checks, timezone_checks, CheckReport, CheckResult, Kind, Status, TimezoneContext,
};
use crate::core::preflight::OwnedProcess;
use crate::core::CorePorts;
use crate::log::{Level, Logger};
use crate::model::ProxySettings;
use crate::profile::Entry;
use crate::profile::{validate_agent_profile, AgentProfile, ProxyScopeMode};
use crate::snapshot::{Enabled, EnvironmentSnapshot, GateState, ItemGate, TakeoverGate};
use crate::status::EnvironmentStatus;
use crate::store::{write_atomic, SnapshotStore, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Daily,
    Entering,
    AgentReady,
    AgentUnverified,
    Exiting,
    RestoreIncomplete,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Daily => "daily",
            Mode::Entering => "entering",
            Mode::AgentReady => "agent_ready",
            Mode::AgentUnverified => "agent_unverified",
            Mode::Exiting => "exiting",
            Mode::RestoreIncomplete => "restore_incomplete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preflight,
    Snapshot,
    Core,
    Global,
    Entries,
    Checks,
    RestoreEntries,
    RestoreGlobal,
    StopCore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum GlobalValue {
    SystemProxy { value: ProxySettings },
    Timezone { windows_id: String },
}

impl GlobalValue {
    pub fn item(&self) -> GlobalItem {
        match self {
            GlobalValue::SystemProxy { .. } => GlobalItem::SystemProxy,
            GlobalValue::Timezone { .. } => GlobalItem::Timezone,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalItem {
    SystemProxy,
    Timezone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEntry {
    Core,
    Global {
        before: GlobalValue,
        after: GlobalValue,
        dry_run: bool,
    },
    /// An Agent entry this application started (or opened a window in, when it was already ours).
    Entry {
        id: String,
        process: OwnedProcess,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerState {
    pub schema_version: u32,
    pub mode: Mode,
    pub phase: Option<Phase>,
    pub profile_id: Option<String>,
    pub active_snapshot_id: Option<String>,
    pub journal: Vec<JournalEntry>,
    pub last_report: Option<CheckReport>,
    pub unacknowledged_restore_failures: Vec<RestoreItem>,
    pub correlation_id: Option<String>,
    /// The time zone item of the takeover gate as evaluated when entering; G4 depends on it.
    /// Whether the system proxy could be written is a separate question and stops nothing else.
    #[serde(default)]
    pub timezone_writable: bool,
    /// Foreign writers seen when entering.
    #[serde(default)]
    pub foreign_writers: Vec<String>,
    /// The system proxy item of the takeover gate as evaluated when entering; G6 depends on it.
    #[serde(default)]
    pub system_proxy_gate: ItemGate,
}

impl Default for ControllerState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            mode: Mode::Daily,
            phase: None,
            profile_id: None,
            active_snapshot_id: None,
            journal: Vec::new(),
            last_report: None,
            unacknowledged_restore_failures: Vec::new(),
            correlation_id: None,
            timezone_writable: false,
            foreign_writers: Vec::new(),
            system_proxy_gate: ItemGate::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreStatus {
    Restored,
    /// Nothing was really changed (dry-run), so nothing needed restoring.
    DryRun,
    /// Somebody else changed the value after we did; left as it is.
    Conflict,
    /// The user selected another system proxy during this session; preserving it completes exit.
    PreservedExternal,
    Failed,
    /// Deliberately left to the user (a CLI session); reported, not a failure.
    LeftOpen,
}

/// What closing an entry did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseOutcome {
    Closed,
    /// Not closed on purpose; the text tells the user what remains.
    LeftOpen(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreItem {
    pub item: String,
    pub status: RestoreStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub items: Vec<RestoreItem>,
    pub mode_after: Mode,
}

/// An error with a code from the SPEC 4.11 families.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fault {
    pub code: String,
    pub message: String,
}

impl Fault {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Fault {}

/// Everything the state machine needs from the machine.
pub trait Platform {
    fn now_ms(&self) -> u64;
    fn correlation_id(&self) -> String;
    fn read_status(&self) -> EnvironmentStatus;
    /// Ports, binary and anything else the core needs, checked before anything changes.
    fn core_preflight(&self, profile: &AgentProfile, direct_test: bool) -> Result<(), Fault>;
    fn core_up(&self, profile: &AgentProfile, direct_test: bool) -> Result<(), Fault>;
    fn core_down(&self) -> Result<(), Fault>;
    fn core_port(&self) -> u16;
    fn run_checks(&self, profile: &AgentProfile) -> CheckReport;
    /// Start one of the profile's entries. Never ends a process it did not start.
    fn launch_entry(&self, profile: &AgentProfile, entry_id: &str) -> Result<OwnedProcess, Fault>;
    /// What a browser entry's check page reported; waits for the first report, then returns the
    /// same one. `None` when nothing came back.
    fn entry_signals(&self, entry_id: &str) -> Option<BrowserSignals>;
    /// Ask an entry this application started to close (no forced kill).
    fn close_entry(&self, process: &OwnedProcess) -> Result<CloseOutcome, Fault>;
    /// Whether this process holds SeTimeZonePrivilege (evidence for G4).
    fn timezone_privilege(&self) -> Result<bool, String>;
    fn read_global(&self, item: GlobalItem) -> Result<GlobalValue, Fault>;
    /// Real write of one global value. Only called with the takeover gate open.
    fn write_global(&self, value: &GlobalValue) -> Result<(), Fault>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// "Take over global state" (default off, SPEC 4.7).
    pub takeover_enabled: bool,
    /// The core's mixed-port, for installations that must occupy a port other programs already
    /// name. `None` and `0` keep [`CorePorts::default`], so a missing field never moves the port.
    pub core_mixed_port: Option<u16>,
}

impl Settings {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("settings.json")
    }

    /// The ports this installation runs its core on. Only the mixed-port is overridable so far;
    /// the controller, DNS and relay ports stay at [`CorePorts::default`].
    pub fn core_ports(&self) -> CorePorts {
        let ports = CorePorts::default();
        match self.core_mixed_port {
            Some(mixed) if mixed != 0 => CorePorts { mixed, ..ports },
            _ => ports,
        }
    }

    /// Missing or unreadable settings mean the defaults (takeover off), never an open gate.
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read(Self::path(data_dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), StoreError> {
        write_atomic(
            &Self::path(data_dir),
            &serde_json::to_vec_pretty(self).expect("serializes"),
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct EnterOptions {
    /// Run the core with the DIRECT test configuration instead of the profile's candidates.
    pub direct_test: bool,
    /// Entry ids to start (E5), in order.
    pub entries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterOutcome {
    pub mode: Mode,
    pub gate: TakeoverGate,
    pub report: Option<CheckReport>,
    /// Set when entering failed and was rolled back.
    pub fault: Option<Fault>,
    pub rollback: Option<RestoreReport>,
}

/// Hard checks every report must contain before Agent mode may be called ready. Checks that later
/// tickets implement are listed so that their absence reads as "unknown", never as "pass".
fn required_hard_checks(profile: &AgentProfile) -> Vec<String> {
    let mut ids = vec!["G1".to_string()];
    ids.extend(profile.targets.iter().map(|t| format!("G2:{}", t.domain)));
    ids.push("G3".into());
    ids.push("G4".into());
    if profile.proxy_scope.mode == ProxyScopeMode::SystemAndExplicit {
        ids.push("G6".into());
    }
    ids
}

fn complete_report(mut report: CheckReport, profile: &AgentProfile) -> CheckReport {
    for id in required_hard_checks(profile) {
        if !report.results.iter().any(|r| r.id == id) {
            report.results.push(CheckResult {
                id,
                kind: Kind::Hard,
                status: Status::Unknown,
                reason: Some("not_implemented".into()),
                evidence: Vec::new(),
            });
        }
    }
    report
}

fn is_ready(report: &CheckReport) -> bool {
    report.hard_checks_pass()
        && report
            .results
            .iter()
            .filter(|r| r.kind == Kind::Hard)
            .all(|r| r.status == Status::Pass)
}

/// What [`Controller::keep_system_proxy`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepOutcome {
    /// An external change was observed. No system setting is rewritten (ADR-0004).
    Changed {
        found: String,
    },
    Failed(String),
}

pub struct Controller<P: Platform> {
    platform: P,
    last_proxy_change: Option<GlobalValue>,
    settings: Settings,
    state_path: PathBuf,
    snapshots: SnapshotStore,
    log: Logger,
    state: ControllerState,
    /// The state file said we were not in daily mode when we started.
    recovery_pending: bool,
}

impl<P: Platform> Controller<P> {
    /// Load persisted state. A non-daily state is reported via [`Self::recovery_pending`] and
    /// otherwise left alone: nothing is written to the system on startup.
    pub fn open(platform: P, data_dir: &Path, settings: Settings) -> Result<Self, StoreError> {
        let state_path = data_dir.join("state.json");
        let state: ControllerState = match std::fs::read(&state_path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| StoreError::Parse {
                path: state_path.clone(),
                message: e.to_string(),
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ControllerState::default(),
            Err(e) => {
                return Err(StoreError::Io {
                    path: state_path,
                    source: e,
                })
            }
        };
        let recovery_pending = state.mode != Mode::Daily;
        let c = Self {
            platform,
            last_proxy_change: None,
            settings,
            snapshots: SnapshotStore::new(data_dir),
            log: Logger::new(data_dir),
            state_path,
            state,
            recovery_pending,
        };
        if recovery_pending {
            c.event(
                Level::Warn,
                "startup.unfinished_session",
                json!({ "mode": c.state.mode.as_str(), "phase": c.state.phase }),
            );
        }
        c.log.prune();
        Ok(c)
    }

    pub fn state(&self) -> &ControllerState {
        &self.state
    }
    pub fn recovery_pending(&self) -> bool {
        self.recovery_pending
    }
    pub fn platform(&self) -> &P {
        &self.platform
    }
    pub fn settings(&self) -> Settings {
        self.settings
    }
    /// New settings take effect at the next enter; a running session keeps its gate.
    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    fn event(&self, level: Level, event: &str, data: serde_json::Value) {
        let phase = self.state.phase.map(|p| {
            serde_json::to_value(p)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        });
        self.log.write(
            self.platform.now_ms(),
            level,
            event,
            self.state.mode.as_str(),
            phase.as_deref(),
            self.state.correlation_id.as_deref().unwrap_or("-"),
            data,
        );
    }

    fn persist(&self) -> Result<(), Fault> {
        write_atomic(
            &self.state_path,
            &serde_json::to_vec_pretty(&self.state).expect("serializes"),
        )
        .map_err(|e| Fault::new("IO_WRITE", e.to_string()))
    }

    fn set(&mut self, mode: Mode, phase: Option<Phase>) -> Result<(), Fault> {
        self.state.mode = mode;
        self.state.phase = phase;
        self.persist()
    }

    /// What the user switched on: the time zone in the settings, system-level routing in the profile.
    pub fn enabled(&self, profile: &AgentProfile) -> Enabled {
        Enabled {
            system_proxy: profile.proxy_scope.mode == ProxyScopeMode::SystemAndExplicit,
            timezone: self.settings.takeover_enabled,
        }
    }

    /// The system proxy this application sets for a profile: the core's mixed port, with the
    /// profile's bypass list, and nothing automatic left on.
    fn system_proxy_target(&self, profile: &AgentProfile) -> ProxySettings {
        ProxySettings {
            flags: crate::model::proxy_flags::DIRECT | crate::model::proxy_flags::PROXY,
            proxy_server: Some(format!("127.0.0.1:{}", self.platform.core_port())),
            bypass: Some(profile.proxy_scope.bypass.join(";")),
            auto_config_url: None,
        }
    }

    // Preserving an external choice is only a completed handoff if no active endpoint can
    // still depend on the core we're about to stop. PAC/WPAD and unparseable values are unknown.
    fn independent_system_proxy(&self, value: &ProxySettings) -> bool {
        if value.pac_enabled() || value.auto_detect() {
            return false;
        }
        if !value.proxy_enabled() {
            return true;
        }
        value.proxy_server.as_deref().is_some_and(|servers| {
            !servers.trim().is_empty()
                && servers.split(';').all(|server| {
                    let endpoint = server.rsplit('=').next().unwrap_or("").trim();
                    endpoint
                        .rsplit_once(':')
                        .and_then(|(_, port)| port.parse::<u16>().ok())
                        .is_some_and(|port| port != 0 && port != self.platform.core_port())
                })
        })
    }

    /// Global changes the profile asks for.
    fn plan(
        &self,
        profile: &AgentProfile,
        before: &EnvironmentSnapshot,
    ) -> Vec<(GlobalValue, GlobalValue)> {
        let mut plan = Vec::new();
        if let Some(tz) = before.timezone.value() {
            if tz.windows_id != profile.locale.timezone.windows_id {
                plan.push((
                    GlobalValue::Timezone {
                        windows_id: tz.windows_id.clone(),
                    },
                    GlobalValue::Timezone {
                        windows_id: profile.locale.timezone.windows_id.clone(),
                    },
                ));
            }
        }
        if profile.proxy_scope.mode == ProxyScopeMode::SystemAndExplicit {
            if let Some(p) = before.system_proxy.value() {
                let target = self.system_proxy_target(profile);
                if *p != target {
                    plan.push((
                        GlobalValue::SystemProxy { value: p.clone() },
                        GlobalValue::SystemProxy { value: target },
                    ));
                }
            }
        }
        plan
    }

    /// Enter Agent mode (E1–E6). Any failure from E2 on rolls back what was done.
    pub fn enter(
        &mut self,
        profile: &AgentProfile,
        opts: EnterOptions,
    ) -> Result<EnterOutcome, Fault> {
        if self.state.mode != Mode::Daily {
            return Err(Fault::new(
                "STATE_INVALID",
                format!("cannot enter from {}", self.state.mode.as_str()),
            ));
        }
        self.state.correlation_id = Some(self.platform.correlation_id());
        self.state.profile_id = Some(profile.id.clone());
        self.event(
            Level::Info,
            "enter.start",
            json!({ "profile": profile.id, "directTest": opts.direct_test }),
        );

        // E1 preflight: nothing has changed yet, so a failure simply returns.
        self.last_proxy_change = None;
        let problems: Vec<_> = validate_agent_profile(profile)
            .into_iter()
            .filter(|e| !(opts.direct_test && e.path == "agent.egress.candidates"))
            .collect();
        if !problems.is_empty() {
            let f = Fault::new(
                "CFG_INVALID",
                problems
                    .iter()
                    .map(|e| format!("{}: {}", e.path, e.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            self.event(
                Level::Error,
                "enter.preflight",
                json!({ "code": f.code, "message": f.message }),
            );
            return Err(f);
        }
        if let Err(f) = self.platform.core_preflight(profile, opts.direct_test) {
            self.event(
                Level::Error,
                "enter.preflight",
                json!({ "code": f.code, "message": f.message }),
            );
            return Err(f);
        }

        // E2 snapshot
        self.set(Mode::Entering, Some(Phase::Snapshot))?;
        let status = self.platform.read_status();
        let (id, ms) = self.snapshots.next_id();
        let enabled = self.enabled(profile);
        let snap =
            EnvironmentSnapshot::from_status(id, ms, &status, Some(profile.id.clone()), enabled);
        if !snap.is_restorable() {
            return self.abort(
                profile,
                Fault::new(
                    "SNAPSHOT_UNREADABLE",
                    "system proxy or time zone could not be read",
                ),
            );
        }
        if let Err(e) = self.snapshots.save(&snap, true) {
            return self.abort(profile, Fault::new("IO_WRITE", e.to_string()));
        }
        self.state.active_snapshot_id = Some(snap.id.clone());
        let gate = TakeoverGate::evaluate(enabled, &status.foreign_writers, true);
        self.state.system_proxy_gate = gate.system_proxy.clone();
        self.state.timezone_writable = gate.allows(GlobalItem::Timezone);
        // R10 asks who maintains the time zone. A client that only holds the system proxy's port
        // leaves the time zone alone, so naming it there reported a conflict that does not exist.
        self.state.foreign_writers = status
            .foreign_writers
            .writers
            .iter()
            .filter(|w| crate::snapshot::writer_items(w.kind).contains(&GlobalItem::Timezone))
            .map(|w| w.evidence.clone())
            .collect();
        self.event(
            Level::Info,
            "enter.snapshot",
            json!({ "snapshot": snap.id, "gate": gate.state, "gateReasons": gate.reasons }),
        );

        // E3 core
        self.set(Mode::Entering, Some(Phase::Core))?;
        if let Err(f) = self.platform.core_up(profile, opts.direct_test) {
            return self.abort(profile, f);
        }
        self.state.journal.push(JournalEntry::Core);
        self.persist()?;
        self.event(
            Level::Info,
            "enter.core",
            json!({ "port": self.platform.core_port() }),
        );

        // E4 global state: each item is real only behind its own gate, otherwise recorded as
        // dry-run. A foreign writer that only holds the system proxy's port leaves the time zone
        // writable, and the other way round.
        self.set(Mode::Entering, Some(Phase::Global))?;
        for (before, after) in self.plan(profile, &snap) {
            let real = gate.allows(after.item());
            if real {
                if let Err(f) = self.platform.write_global(&after) {
                    return self.abort(profile, f);
                }
                match self.platform.read_global(after.item()) {
                    Ok(now) if now == after => {}
                    Ok(now) => {
                        return self.abort(
                            profile,
                            Fault::new(
                                "ADAPTER_VERIFY_FAILED",
                                format!("{:?} reads back as {now:?}", after.item()),
                            ),
                        )
                    }
                    Err(f) => return self.abort(profile, f),
                }
            }
            self.event(
                Level::Info,
                "enter.global",
                json!({ "item": after.item(), "dryRun": !real, "before": before, "after": after }),
            );
            self.state.journal.push(JournalEntry::Global {
                before,
                after,
                dry_run: !real,
            });
            self.persist()?;
        }

        // E5 entries, in the order asked for. Each started entry is journaled before the next one,
        // so a failure part-way closes exactly what was started.
        self.set(Mode::Entering, Some(Phase::Entries))?;
        for id in &opts.entries {
            if !profile.entries.iter().any(|e| e.id() == id) {
                return self.abort(
                    profile,
                    Fault::new("ENTRY_UNKNOWN", format!("the profile has no entry '{id}'")),
                );
            }
            match self.platform.launch_entry(profile, id) {
                Ok(process) => {
                    self.event(
                        Level::Info,
                        "enter.entry",
                        json!({ "id": id, "pid": process.pid, "exe": process.exe }),
                    );
                    self.state.journal.push(JournalEntry::Entry {
                        id: id.clone(),
                        process,
                    });
                    self.persist()?;
                }
                Err(f) => return self.abort(profile, f),
            }
        }

        // E6 checks
        self.set(Mode::Entering, Some(Phase::Checks))?;
        let report = self.full_report(profile);
        let mode = if is_ready(&report) {
            Mode::AgentReady
        } else {
            Mode::AgentUnverified
        };
        self.state.last_report = Some(report.clone());
        self.set(mode, None)?;
        self.event(Level::Info, "enter.done", json!({ "mode": mode.as_str(), "hard": report.results.iter().filter(|r| r.kind == Kind::Hard).map(|r| (r.id.clone(), r.status)).collect::<Vec<_>>() }));
        Ok(EnterOutcome {
            mode,
            gate,
            report: Some(report),
            fault: None,
            rollback: None,
        })
    }

    /// Network checks from the platform plus the checks that need this state machine's context
    /// (G4, R10), with missing required checks filled in as unknown.
    fn full_report(&self, profile: &AgentProfile) -> CheckReport {
        let mut report = self.platform.run_checks(profile);

        // Browser entries started in this session: G5, R3–R5 from their check pages.
        let mut browser_timezones = Vec::new();
        let mut browser_results = Vec::new();
        for e in &self.state.journal {
            let JournalEntry::Entry { id, .. } = e else {
                continue;
            };
            let is_browser = profile
                .entries
                .iter()
                .any(|x| x.id() == id && matches!(x, Entry::Browser { .. }));
            if !is_browser {
                continue;
            }
            let signals = self.platform.entry_signals(id);
            let (results, tz) =
                browser_checks(profile, id, signals.as_ref(), report.exit_ip.as_deref());
            browser_results.extend(results);
            if let Some(tz) = tz {
                browser_timezones.push((id.clone(), tz));
            }
        }

        let ctx = TimezoneContext {
            current: match self.platform.read_global(GlobalItem::Timezone) {
                Ok(GlobalValue::Timezone { windows_id }) => Ok(windows_id),
                Ok(other) => Err(format!("unexpected value {other:?}")),
                Err(f) => Err(f.to_string()),
            },
            timezone_writable: self.state.timezone_writable,
            changed_by_us: self.state.journal.iter().any(|e| {
                matches!(
                    e,
                    JournalEntry::Global {
                        after: GlobalValue::Timezone { .. },
                        dry_run: false,
                        ..
                    }
                )
            }),
            foreign_writers: self.state.foreign_writers.clone(),
            privilege: self.platform.timezone_privilege(),
            browser_timezones,
        };
        let mut tz = timezone_checks(profile, &ctx).into_iter();
        let at = report
            .results
            .iter()
            .rposition(|r| r.id == "G3" || r.id.starts_with("G2:"))
            .map_or(report.results.len(), |i| i + 1);
        if let Some(g4) = tz.next() {
            report.results.insert(at, g4);
        }
        // Hard browser checks right after G4, read-only ones at the end.
        let (hard, soft): (Vec<_>, Vec<_>) = browser_results
            .into_iter()
            .partition(|r| r.kind == Kind::Hard);
        let hard_count = hard.len();
        for (i, r) in hard.into_iter().enumerate() {
            report.results.insert(at + 1 + i, r);
        }
        if let Some(g6) = self.system_scope_check(profile) {
            report
                .results
                .insert((at + 1 + hard_count).min(report.results.len()), g6);
        }
        report.results.extend(tz);
        report.results.extend(soft);
        complete_report(report, profile)
    }

    /// G6 (SPEC 4.5): with system-level routing on, the system proxy must point at the core.
    fn system_scope_check(&self, profile: &AgentProfile) -> Option<CheckResult> {
        if profile.proxy_scope.mode != ProxyScopeMode::SystemAndExplicit {
            return None;
        }
        let target = self.system_proxy_target(profile);
        let want = target.proxy_server.clone().unwrap_or_default();
        let r = match self.platform.read_global(GlobalItem::SystemProxy) {
            Ok(GlobalValue::SystemProxy { value }) if value == target => {
                CheckResult::new("G6", Kind::Hard, Status::Pass, None).with(
                    "system proxy",
                    format!("points at this application's core ({want})"),
                )
            }
            Ok(GlobalValue::SystemProxy { value }) => {
                let seen = format!(
                    "proxy {} (enabled: {}), bypass {}, setup script {}",
                    value.proxy_server.as_deref().unwrap_or("none"),
                    value.proxy_enabled(),
                    value.bypass.as_deref().unwrap_or("none"),
                    value.auto_config_url.as_deref().unwrap_or("none")
                );
                if self.state.system_proxy_gate.state == GateState::Open {
                    CheckResult::new("G6", Kind::Hard, Status::Fail, Some("system_proxy_changed"))
                        .with("system proxy", seen)
                        .with("wanted", want)
                } else {
                    let mut r = CheckResult::new(
                        "G6",
                        Kind::Hard,
                        Status::Unknown,
                        Some("global_state_not_taken_over"),
                    )
                    .with("system proxy", seen);
                    for reason in &self.state.system_proxy_gate.reasons {
                        r = r.with("gate", reason.clone());
                    }
                    r
                }
            }
            Ok(other) => CheckResult::new("G6", Kind::Hard, Status::Unknown, Some("read_failed"))
                .with("system proxy", format!("unexpected value {other:?}")),
            Err(f) => CheckResult::new("G6", Kind::Hard, Status::Unknown, Some("read_failed"))
                .with("system proxy", f.to_string()),
        };
        Some(r)
    }

    /// Observe the system proxy while Agent mode runs. An external change is a conflict, not a
    /// reason to compete for a single Windows-wide setting (ADR-0004).
    pub fn keep_system_proxy(&mut self) -> Option<KeepOutcome> {
        if !matches!(self.state.mode, Mode::AgentReady | Mode::AgentUnverified) {
            return None;
        }
        if !self.settings.takeover_enabled || self.state.system_proxy_gate.state != GateState::Open
        {
            return None;
        }
        let ours = self.state.journal.iter().rev().find_map(|e| match e {
            JournalEntry::Global {
                after: after @ GlobalValue::SystemProxy { .. },
                dry_run: false,
                ..
            } => Some(after.clone()),
            _ => None,
        })?;
        let current = match self.platform.read_global(GlobalItem::SystemProxy) {
            Ok(v) => v,
            Err(f) => {
                self.state.mode = Mode::AgentUnverified;
                if let Some(report) = &mut self.state.last_report {
                    if let Some(g6) = report.results.iter_mut().find(|r| r.id == "G6") {
                        *g6 = CheckResult::new(
                            "G6",
                            Kind::Hard,
                            Status::Unknown,
                            Some("system_proxy_unreadable"),
                        );
                    }
                }
                if let Err(persist) = self.persist() {
                    return Some(KeepOutcome::Failed(persist.to_string()));
                }
                return Some(KeepOutcome::Failed(f.to_string()));
            }
        };
        if current == ours {
            self.last_proxy_change = None;
            if let Err(e) = self.proxy_check(Status::Pass, None) {
                return Some(KeepOutcome::Failed(e.to_string()));
            }
            return None;
        }
        if self.last_proxy_change.as_ref() == Some(&current) {
            return None;
        }
        if let Err(e) = self.proxy_check(Status::Fail, Some("system_proxy_changed")) {
            return Some(KeepOutcome::Failed(e.to_string()));
        }
        self.last_proxy_change = Some(current.clone());
        let found = match &current {
            GlobalValue::SystemProxy { value } if value.proxy_enabled() => value
                .proxy_server
                .clone()
                .unwrap_or_else(|| "(empty)".into()),
            GlobalValue::SystemProxy { .. } => "off".into(),
            other => format!("{other:?}"),
        };
        self.event(
            Level::Warn,
            "observe.system_proxy.changed",
            json!({ "found": found }),
        );
        Some(KeepOutcome::Changed { found })
    }

    /// Periodic observation must reflect actual G6 evidence, while preserving every other gate.
    fn proxy_check(&mut self, status: Status, reason: Option<&str>) -> Result<(), Fault> {
        let Some(report) = &mut self.state.last_report else {
            return Ok(());
        };
        let Some(g6) = report.results.iter_mut().find(|r| r.id == "G6") else {
            return Ok(());
        };
        if g6.status == status && g6.reason.as_deref() == reason {
            return Ok(());
        }
        *g6 = CheckResult::new("G6", Kind::Hard, status, reason)
            .with("source", "system proxy periodic observation");
        self.state.mode = if is_ready(report) {
            Mode::AgentReady
        } else {
            Mode::AgentUnverified
        };
        self.persist()
    }

    fn abort(&mut self, _profile: &AgentProfile, fault: Fault) -> Result<EnterOutcome, Fault> {
        self.event(
            Level::Error,
            "enter.failed",
            json!({ "code": fault.code, "message": fault.message }),
        );
        let rollback = self.leave("rollback");
        let gate = TakeoverGate::evaluate(self.enabled(_profile), &Default::default(), false);
        Ok(EnterOutcome {
            mode: rollback.mode_after,
            gate,
            report: None,
            fault: Some(fault),
            rollback: Some(rollback),
        })
    }

    /// A route selection invalidates earlier evidence before a request to the control API starts.
    pub fn invalidate_routing_checks(&mut self) -> Result<(), Fault> {
        if !matches!(self.state.mode, Mode::AgentReady | Mode::AgentUnverified) {
            return Err(Fault::new(
                "MODE_INVALID",
                "route switching requires Agent mode",
            ));
        }
        self.state.mode = Mode::AgentUnverified;
        self.state.last_report = None;
        self.persist()
    }

    /// Re-run the checks while in Agent mode.
    pub fn recheck(&mut self, profile: &AgentProfile) -> Result<CheckReport, Fault> {
        if !matches!(self.state.mode, Mode::AgentReady | Mode::AgentUnverified) {
            return Err(Fault::new(
                "STATE_INVALID",
                format!("cannot recheck in {}", self.state.mode.as_str()),
            ));
        }
        let report = self.full_report(profile);
        let mode = if is_ready(&report) {
            Mode::AgentReady
        } else {
            Mode::AgentUnverified
        };
        if self.state.mode == Mode::AgentReady && mode == Mode::AgentUnverified {
            self.event(Level::Warn, "recheck.degraded", json!({}));
        }
        self.state.last_report = Some(report.clone());
        self.set(mode, None)?;
        Ok(report)
    }

    /// Start one more entry while in Agent mode, journal it, and re-run the checks so its browser
    /// signals are part of the report. An entry that is already running is reused by the platform
    /// (a browser opens a new window) and journaled once.
    pub fn open_entry(
        &mut self,
        profile: &AgentProfile,
        entry_id: &str,
    ) -> Result<CheckReport, Fault> {
        if !matches!(self.state.mode, Mode::AgentReady | Mode::AgentUnverified) {
            return Err(Fault::new(
                "STATE_INVALID",
                format!("cannot open an entry in {}", self.state.mode.as_str()),
            ));
        }
        if !profile.entries.iter().any(|e| e.id() == entry_id) {
            return Err(Fault::new(
                "ENTRY_UNKNOWN",
                format!("the profile has no entry '{entry_id}'"),
            ));
        }
        let process = match self.platform.launch_entry(profile, entry_id) {
            Ok(p) => p,
            Err(f) => {
                self.event(
                    Level::Error,
                    "entry.open_failed",
                    json!({ "id": entry_id, "code": f.code, "message": f.message }),
                );
                return Err(f);
            }
        };
        self.event(
            Level::Info,
            "entry.open",
            json!({ "id": entry_id, "pid": process.pid, "exe": process.exe }),
        );
        let known = self.state.journal.iter().any(|e| {
            matches!(e, JournalEntry::Entry { id, process: p } if id == entry_id && p.pid == process.pid && p.created == process.created)
        });
        if !known {
            self.state.journal.push(JournalEntry::Entry {
                id: entry_id.to_string(),
                process,
            });
            self.persist()?;
        }
        self.recheck(profile)
    }

    /// Leave Agent mode (X1–X4). Also the way out of an interrupted session and of a failed restore.
    pub fn exit(&mut self) -> Result<RestoreReport, Fault> {
        if self.state.mode == Mode::Daily && self.state.journal.is_empty() {
            return Ok(RestoreReport {
                items: Vec::new(),
                mode_after: Mode::Daily,
            });
        }
        if self.state.correlation_id.is_none() {
            self.state.correlation_id = Some(self.platform.correlation_id());
        }
        let r = self.leave("exit");
        self.recovery_pending = false;
        Ok(r)
    }

    fn leave(&mut self, why: &str) -> RestoreReport {
        let _ = self.set(Mode::Exiting, Some(Phase::RestoreEntries));
        self.event(
            Level::Info,
            &format!("{why}.start"),
            json!({ "journal": self.state.journal.len() }),
        );
        let mut items = Vec::new();
        let mut keep = Vec::new();
        let journal = std::mem::take(&mut self.state.journal);
        let (entries, journal): (Vec<_>, Vec<_>) = journal
            .into_iter()
            .partition(|e| matches!(e, JournalEntry::Entry { .. }));

        // X1 entries this application started, newest first. One that will not close is a
        // failure the user resolves; it is never forced.
        for entry in entries.into_iter().rev() {
            let JournalEntry::Entry { id, process } = &entry else {
                unreachable!()
            };
            let item = match self.platform.close_entry(process) {
                Ok(CloseOutcome::Closed) => RestoreItem {
                    item: format!("Entry:{id}"),
                    status: RestoreStatus::Restored,
                    detail: format!("pid {} closed", process.pid),
                },
                Ok(CloseOutcome::LeftOpen(why)) => RestoreItem {
                    item: format!("Entry:{id}"),
                    status: RestoreStatus::LeftOpen,
                    detail: why,
                },
                Err(f) => {
                    keep.push(entry.clone());
                    RestoreItem {
                        item: format!("Entry:{id}"),
                        status: RestoreStatus::Failed,
                        detail: f.to_string(),
                    }
                }
            };
            self.event(
                if item.status == RestoreStatus::Failed {
                    Level::Error
                } else {
                    Level::Info
                },
                &format!("{why}.entry"),
                json!(item),
            );
            items.push(item);
        }

        // X2 global state, newest change first. Restored entries leave the journal; the rest stay
        // for a retry.
        let _ = self.set(Mode::Exiting, Some(Phase::RestoreGlobal));
        let (globals, others): (Vec<_>, Vec<_>) = journal
            .into_iter()
            .partition(|e| matches!(e, JournalEntry::Global { .. }));
        for entry in globals.into_iter().rev() {
            let JournalEntry::Global {
                before,
                after,
                dry_run,
            } = entry.clone()
            else {
                unreachable!()
            };
            let name = format!("{:?}", after.item());
            let item = if dry_run {
                RestoreItem {
                    item: name,
                    status: RestoreStatus::DryRun,
                    detail: "not changed for real; nothing to restore".into(),
                }
            } else {
                match self.platform.read_global(after.item()) {
                    Ok(now) if now == after => match self.platform.write_global(&before) {
                        Ok(()) => match self.platform.read_global(before.item()) {
                            Ok(v) if v == before => RestoreItem {
                                item: name,
                                status: RestoreStatus::Restored,
                                detail: "restored and verified".into(),
                            },
                            Ok(v) => RestoreItem {
                                item: name,
                                status: RestoreStatus::Failed,
                                detail: format!("reads back as {v:?}"),
                            },
                            Err(f) => RestoreItem {
                                item: name,
                                status: RestoreStatus::Failed,
                                detail: f.to_string(),
                            },
                        },
                        Err(f) => RestoreItem {
                            item: name,
                            status: RestoreStatus::Failed,
                            detail: f.to_string(),
                        },
                    },
                    Ok(now) if now == before => RestoreItem {
                        item: name,
                        status: RestoreStatus::Restored,
                        detail: "already back to the snapshot value".into(),
                    },
                    Ok(GlobalValue::SystemProxy { value })
                        if self.independent_system_proxy(&value) =>
                    {
                        RestoreItem {
                            item: name,
                            status: RestoreStatus::PreservedExternal,
                            detail: "保留你后来选择的系统代理；AEC 已交还控制，不覆盖该选择".into(),
                        }
                    }
                    Ok(now) => RestoreItem {
                        item: name,
                        status: RestoreStatus::Conflict,
                        detail: format!("changed by somebody else to {now:?}; left as it is"),
                    },
                    Err(f) => RestoreItem {
                        item: name,
                        status: RestoreStatus::Failed,
                        detail: f.to_string(),
                    },
                }
            };
            if matches!(item.status, RestoreStatus::Failed | RestoreStatus::Conflict) {
                keep.push(entry);
            }
            self.event(
                if item.status == RestoreStatus::Failed {
                    Level::Error
                } else {
                    Level::Info
                },
                &format!("{why}.global"),
                json!(item),
            );
            items.push(item);
        }
        keep.reverse();

        // X3 core
        let _ = self.set(Mode::Exiting, Some(Phase::StopCore));
        for entry in others {
            if entry == JournalEntry::Core {
                let item = match self.platform.core_down() {
                    Ok(()) => RestoreItem {
                        item: "Core".into(),
                        status: RestoreStatus::Restored,
                        detail: "core stopped".into(),
                    },
                    Err(f) => {
                        keep.push(JournalEntry::Core);
                        RestoreItem {
                            item: "Core".into(),
                            status: RestoreStatus::Failed,
                            detail: f.to_string(),
                        }
                    }
                };
                self.event(
                    if item.status == RestoreStatus::Failed {
                        Level::Error
                    } else {
                        Level::Info
                    },
                    &format!("{why}.core"),
                    json!(item),
                );
                items.push(item);
            }
        }

        // X4
        self.state.journal = keep;
        let failures: Vec<RestoreItem> = items
            .iter()
            .filter(|i| matches!(i.status, RestoreStatus::Failed | RestoreStatus::Conflict))
            .cloned()
            .collect();
        let mode_after = if failures.is_empty() {
            Mode::Daily
        } else {
            Mode::RestoreIncomplete
        };
        self.state.unacknowledged_restore_failures = failures;
        self.state.mode = mode_after;
        self.state.phase = None;
        // Log before the correlation id is cleared, so the last line of the operation carries it.
        self.event(
            Level::Info,
            &format!("{why}.done"),
            json!({ "mode": mode_after.as_str(), "items": items.len() }),
        );
        if mode_after == Mode::Daily {
            self.state.correlation_id = None;
            self.state.timezone_writable = false;
            self.state.foreign_writers.clear();
            self.state.system_proxy_gate = ItemGate::default();
        }
        let _ = self.persist();
        RestoreReport { items, mode_after }
    }

    /// Accept the remaining restore failures and return to daily mode. The record of them stays in
    /// the log; the journal entries are dropped because the user chose to keep the current values.
    pub fn acknowledge(&mut self) -> Result<(), Fault> {
        if self.state.mode != Mode::RestoreIncomplete {
            return Err(Fault::new(
                "STATE_INVALID",
                format!("nothing to acknowledge in {}", self.state.mode.as_str()),
            ));
        }
        self.event(
            Level::Warn,
            "restore.acknowledged",
            json!({ "items": self.state.unacknowledged_restore_failures }),
        );
        self.state.unacknowledged_restore_failures.clear();
        self.state.journal.clear();
        self.state.correlation_id = None;
        self.set(Mode::Daily, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::{CheckReport, CheckResult};
    use crate::model::proxy_flags;
    use crate::profile::{DetectionMode, ProfileSet};
    use crate::snapshot::GateState;
    use crate::status::collect;
    use crate::testing::FakeHost;
    use std::cell::{Cell, RefCell};

    /// Fake machine. `fail_at` makes one step fail; `gate_open` controls foreign writers.
    struct FakePlatform {
        host: RefCell<FakeHost>,
        fail_at: Option<&'static str>,
        core_running: Cell<bool>,
        hard: Status,
        writes: RefCell<Vec<GlobalValue>>,
        fail_restore_write: Cell<bool>,
        fail_read_global: Cell<bool>,
        /// Entry pids currently "running".
        entries: RefCell<Vec<u32>>,
        /// Entries refuse to close while set.
        stubborn: Cell<bool>,
        /// What every browser check page reports.
        signals: RefCell<Option<BrowserSignals>>,
    }

    fn good_signals() -> BrowserSignals {
        BrowserSignals {
            language: Some("en-US".into()),
            languages: vec!["en-US".into(), "en".into()],
            timezone: Some("America/New_York".into()),
            cookie_enabled: Some(true),
            canvas_sha256: Some("ab".into()),
            webgl_vendor: None,
            webgl_renderer: Some("ANGLE".into()),
            candidates: vec![],
            user_agent: None,
            accept_language_header: Some("en-US,en;q=0.9".into()),
        }
    }

    impl FakePlatform {
        fn new() -> Self {
            Self {
                host: RefCell::new(FakeHost::default()),
                fail_at: None,
                core_running: Cell::new(false),
                hard: Status::Pass,
                writes: RefCell::new(Vec::new()),
                fail_restore_write: Cell::new(false),
                fail_read_global: Cell::new(false),
                entries: RefCell::new(Vec::new()),
                stubborn: Cell::new(false),
                signals: RefCell::new(Some(good_signals())),
            }
        }
        fn failing(step: &'static str) -> Self {
            Self {
                fail_at: Some(step),
                ..Self::new()
            }
        }
        fn fail(&self, step: &str) -> Result<(), Fault> {
            if self.fail_at == Some(step) {
                Err(Fault::new("TEST_FAULT", step))
            } else {
                Ok(())
            }
        }
    }

    impl Platform for FakePlatform {
        fn now_ms(&self) -> u64 {
            1_791_701_261_000
        }
        fn correlation_id(&self) -> String {
            "cid".into()
        }
        fn read_status(&self) -> EnvironmentStatus {
            if self.fail_at == Some("status") {
                let mut h = self.host.borrow().clone();
                h.fail_timezone = Some(crate::probe::ProbeError::new("tz", None));
                return collect(&h, None);
            }
            collect(&*self.host.borrow(), None)
        }
        fn core_preflight(&self, _: &AgentProfile, _: bool) -> Result<(), Fault> {
            self.fail("preflight")
        }
        fn core_up(&self, _: &AgentProfile, _: bool) -> Result<(), Fault> {
            self.fail("core_up")?;
            self.core_running.set(true);
            Ok(())
        }
        fn core_down(&self) -> Result<(), Fault> {
            self.fail("core_down")?;
            self.core_running.set(false);
            Ok(())
        }
        fn core_port(&self) -> u16 {
            17893
        }
        fn timezone_privilege(&self) -> Result<bool, String> {
            Ok(true)
        }
        fn run_checks(&self, p: &AgentProfile) -> CheckReport {
            let mut results: Vec<CheckResult> = ["G1", "G3"]
                .iter()
                .map(|id| CheckResult {
                    id: (*id).into(),
                    kind: Kind::Hard,
                    status: self.hard,
                    reason: None,
                    evidence: vec![],
                })
                .collect();
            results.extend(p.targets.iter().map(|t| CheckResult {
                id: format!("G2:{}", t.domain),
                kind: Kind::Hard,
                status: Status::Pass,
                reason: None,
                evidence: vec![],
            }));
            CheckReport {
                generated_at: "t".into(),
                mode: DetectionMode::Standard,
                proxy_port: 17893,
                results,
                exit_ip: Some("198.51.100.7".into()),
            }
        }
        fn launch_entry(&self, _: &AgentProfile, id: &str) -> Result<OwnedProcess, Fault> {
            self.fail(&format!("launch:{id}"))?;
            let pid = 1000 + self.entries.borrow().len() as u32;
            self.entries.borrow_mut().push(pid);
            let kind = if id.ends_with("-cli") {
                "cli"
            } else {
                "browser"
            };
            Ok(OwnedProcess {
                role: format!("entry:{kind}:{id}"),
                pid,
                exe: "fake.exe".into(),
                created: 1,
                started_at_unix_ms: 1,
            })
        }
        fn entry_signals(&self, _: &str) -> Option<BrowserSignals> {
            self.signals.borrow().clone()
        }
        fn close_entry(&self, p: &OwnedProcess) -> Result<CloseOutcome, Fault> {
            if p.role.starts_with("entry:cli:") {
                return Ok(CloseOutcome::LeftOpen("CLI session left open".into()));
            }
            if self.stubborn.get() {
                return Err(Fault::new(
                    "ENTRY_STILL_RUNNING",
                    format!("pid {} did not close", p.pid),
                ));
            }
            self.entries.borrow_mut().retain(|&x| x != p.pid);
            Ok(CloseOutcome::Closed)
        }
        fn read_global(&self, item: GlobalItem) -> Result<GlobalValue, Fault> {
            if self.fail_read_global.get() {
                return Err(Fault::new("ADAPTER_READ_FAILED", "test"));
            }
            let h = self.host.borrow();
            Ok(match item {
                GlobalItem::Timezone => GlobalValue::Timezone {
                    windows_id: h.timezone.windows_id.clone(),
                },
                GlobalItem::SystemProxy => GlobalValue::SystemProxy {
                    value: h.proxy.clone(),
                },
            })
        }
        fn write_global(&self, v: &GlobalValue) -> Result<(), Fault> {
            if self.fail_restore_write.get() {
                return Err(Fault::new("ADAPTER_WRITE_FAILED", "test"));
            }
            self.fail("write_global")?;
            self.writes.borrow_mut().push(v.clone());
            let mut h = self.host.borrow_mut();
            match v {
                GlobalValue::Timezone { windows_id } => h.timezone.windows_id = windows_id.clone(),
                GlobalValue::SystemProxy { value } => h.proxy = value.clone(),
            }
            Ok(())
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "aec-ctl-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn profile() -> AgentProfile {
        let mut p = ProfileSet::default().agent.remove(0);
        p.egress.candidates.push(crate::profile::Candidate {
            id: "c1".into(),
            host: "198.51.100.7".into(),
            port: 1,
            proto: crate::profile::UpstreamProto::Socks5,
            credential_ref: None,
        });
        p
    }

    fn open(p: FakePlatform, dir: &Path, takeover: bool) -> Controller<FakePlatform> {
        Controller::open(
            p,
            dir,
            Settings {
                takeover_enabled: takeover,
                core_mixed_port: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn settings_port_override_moves_only_the_mixed_port() {
        let base = CorePorts::default();
        assert_eq!(Settings::default().core_ports(), base);
        let zero = Settings {
            takeover_enabled: false,
            core_mixed_port: Some(0),
        };
        assert_eq!(zero.core_ports(), base, "0 means unset, not a port");
        let moved = Settings {
            takeover_enabled: false,
            core_mixed_port: Some(7893),
        };
        let ports = moved.core_ports();
        assert_eq!(ports.mixed, 7893);
        assert_eq!(ports.controller, base.controller);
        assert_eq!(ports.dns, base.dns);
        assert_eq!(ports.relay, base.relay);
    }

    #[test]
    fn settings_written_before_the_port_field_keep_the_default() {
        let s: Settings = serde_json::from_str(r#"{"takeoverEnabled":true}"#).unwrap();
        assert!(s.takeover_enabled);
        assert_eq!(s.core_ports(), CorePorts::default());
    }

    #[test]
    fn enter_dry_run_then_exit_leaves_the_machine_untouched() {
        let d = tmp("dry");
        let mut c = open(FakePlatform::new(), &d, false);
        let before = c.platform().host.borrow().clone();
        let out = c.enter(&profile(), EnterOptions::default()).unwrap();
        // The profile wants another time zone and the gate is closed: G4 cannot pass.
        assert_eq!(out.mode, Mode::AgentUnverified);
        let g4 = out
            .report
            .as_ref()
            .unwrap()
            .results
            .iter()
            .find(|r| r.id == "G4")
            .unwrap();
        assert_eq!(g4.reason.as_deref(), Some("global_state_not_taken_over"));
        assert_eq!(out.gate.state, GateState::Closed);
        assert!(
            c.platform().writes.borrow().is_empty(),
            "gate closed: no real write"
        );
        assert!(
            c.state()
                .journal
                .iter()
                .any(|e| matches!(e, JournalEntry::Global { dry_run: true, .. })),
            "timezone change is journaled as dry-run"
        );
        let r = c.exit().unwrap();
        assert_eq!(r.mode_after, Mode::Daily);
        assert!(r.items.iter().any(|i| i.status == RestoreStatus::DryRun));
        assert!(!c.platform().core_running.get());
        let after = c.platform().host.borrow().clone();
        assert_eq!(
            (after.proxy, after.timezone),
            (before.proxy, before.timezone)
        );
        assert!(c.state().journal.is_empty());
    }

    #[test]
    fn any_failed_or_unknown_hard_check_blocks_ready() {
        for s in [Status::Fail, Status::Unknown, Status::Warn] {
            let d = tmp(&format!("hard-{s:?}"));
            let mut p = FakePlatform::new();
            p.hard = s;
            // Gate open, so G4 passes and only the injected status decides.
            let mut c = open(p, &d, true);
            assert_eq!(
                c.enter(&profile(), EnterOptions::default()).unwrap().mode,
                Mode::AgentUnverified,
                "{s:?}"
            );
        }
        let d = tmp("hard-pass");
        let mut c = open(FakePlatform::new(), &d, true);
        assert_eq!(
            c.enter(&profile(), EnterOptions::default()).unwrap().mode,
            Mode::AgentReady,
            "positive control"
        );
    }

    #[test]
    fn timezone_kept_by_someone_else_passes_g4_and_warns() {
        let d = tmp("tz-external");
        let p = FakePlatform::new();
        {
            let mut h = p.host.borrow_mut();
            h.timezone.windows_id = "Eastern Standard Time".into();
            h.mutexes.insert("Local\\AIEgressGuard".into());
        }
        let mut c = open(p, &d, true);
        let out = c.enter(&profile(), EnterOptions::default()).unwrap();
        let rep = out.report.unwrap();
        let get = |id: &str| rep.results.iter().find(|r| r.id == id).unwrap().clone();
        assert_eq!(get("G4").status, Status::Pass);
        assert_eq!(
            (get("R10").status, get("R10").reason.as_deref()),
            (Status::Warn, Some("maintained_externally"))
        );
        assert_eq!(
            out.mode,
            Mode::AgentReady,
            "a read-only warning does not block readiness"
        );
        assert!(c.platform().writes.borrow().is_empty());
    }

    #[test]
    fn missing_required_checks_count_as_unknown() {
        let d = tmp("missing");
        struct Partial(FakePlatform);
        // Only G1 and G2 are produced, as in the real runner before tickets 06/07.
        impl Platform for Partial {
            fn now_ms(&self) -> u64 {
                self.0.now_ms()
            }
            fn correlation_id(&self) -> String {
                self.0.correlation_id()
            }
            fn read_status(&self) -> EnvironmentStatus {
                self.0.read_status()
            }
            fn core_preflight(&self, p: &AgentProfile, d: bool) -> Result<(), Fault> {
                self.0.core_preflight(p, d)
            }
            fn core_up(&self, p: &AgentProfile, d: bool) -> Result<(), Fault> {
                self.0.core_up(p, d)
            }
            fn core_down(&self) -> Result<(), Fault> {
                self.0.core_down()
            }
            fn core_port(&self) -> u16 {
                17893
            }
            fn timezone_privilege(&self) -> Result<bool, String> {
                Ok(true)
            }
            fn run_checks(&self, p: &AgentProfile) -> CheckReport {
                let mut r = self.0.run_checks(p);
                r.results
                    .retain(|c| c.id == "G1" || c.id.starts_with("G2:"));
                r
            }
            fn launch_entry(&self, p: &AgentProfile, id: &str) -> Result<OwnedProcess, Fault> {
                self.0.launch_entry(p, id)
            }
            fn entry_signals(&self, id: &str) -> Option<BrowserSignals> {
                self.0.entry_signals(id)
            }
            fn close_entry(&self, p: &OwnedProcess) -> Result<CloseOutcome, Fault> {
                self.0.close_entry(p)
            }
            fn read_global(&self, i: GlobalItem) -> Result<GlobalValue, Fault> {
                self.0.read_global(i)
            }
            fn write_global(&self, v: &GlobalValue) -> Result<(), Fault> {
                self.0.write_global(v)
            }
        }
        let mut c =
            Controller::open(Partial(FakePlatform::new()), &d, Settings::default()).unwrap();
        let out = c.enter(&profile(), EnterOptions::default()).unwrap();
        assert_eq!(out.mode, Mode::AgentUnverified);
        let rep = out.report.unwrap();
        let g3 = rep.results.iter().find(|r| r.id == "G3").unwrap();
        assert_eq!(
            (g3.status, g3.reason.as_deref()),
            (Status::Unknown, Some("not_implemented"))
        );
        assert!(
            rep.results.iter().any(|r| r.id == "G4"),
            "G4 comes from the controller itself"
        );
    }

    #[test]
    fn failures_before_any_change_return_without_touching_state() {
        let d = tmp("preflight");
        let mut c = open(FakePlatform::failing("preflight"), &d, false);
        assert_eq!(
            c.enter(&profile(), EnterOptions::default())
                .unwrap_err()
                .code,
            "TEST_FAULT"
        );
        assert_eq!(c.state().mode, Mode::Daily);
        let mut bad = profile();
        bad.egress.candidates.clear();
        let mut c = open(FakePlatform::new(), &d, false);
        assert_eq!(
            c.enter(&bad, EnterOptions::default()).unwrap_err().code,
            "CFG_INVALID"
        );
        assert!(
            c.enter(
                &bad,
                EnterOptions {
                    direct_test: true,
                    ..Default::default()
                }
            )
            .unwrap()
            .fault
            .is_none(),
            "direct test mode needs no candidates"
        );
    }

    #[test]
    fn failures_after_changes_roll_back() {
        for step in ["status", "core_up", "write_global"] {
            let d = tmp(&format!("rb-{step}"));
            let takeover = step == "write_global";
            let mut c = open(FakePlatform::failing(step), &d, takeover);
            let out = c.enter(&profile(), EnterOptions::default()).unwrap();
            assert!(out.fault.is_some(), "{step}");
            assert_eq!(out.mode, Mode::Daily, "{step}: {out:?}");
            assert!(
                !c.platform().core_running.get(),
                "{step}: core must be stopped by the rollback"
            );
            assert_eq!(c.state().mode, Mode::Daily);
            let log = Logger::new(&d).tail(50).join("\n");
            assert!(
                log.contains("enter.failed") && log.contains("rollback.done"),
                "{step}: {log}"
            );
            let done = log.lines().find(|l| l.contains("rollback.done")).unwrap();
            assert!(
                done.contains("\"correlationId\":\"cid\""),
                "{step}: the closing line keeps the id: {done}"
            );
        }
    }

    #[test]
    fn open_gate_writes_for_real_and_restores_with_verification() {
        let d = tmp("real");
        let p = FakePlatform::new();
        p.host.borrow_mut().proxy = ProxySettings {
            flags: proxy_flags::DIRECT,
            proxy_server: None,
            bypass: None,
            auto_config_url: None,
        };
        let mut c = open(p, &d, true);
        let mut prof = profile();
        prof.proxy_scope.mode = ProxyScopeMode::SystemAndExplicit;
        let out = c.enter(&prof, EnterOptions::default()).unwrap();
        assert_eq!(out.gate.state, GateState::Open);
        assert_eq!(
            c.platform().host.borrow().timezone.windows_id,
            "Eastern Standard Time"
        );
        assert_eq!(
            c.platform().host.borrow().proxy.proxy_server.as_deref(),
            Some("127.0.0.1:17893")
        );
        let r = c.exit().unwrap();
        assert_eq!(r.mode_after, Mode::Daily, "{r:?}");
        assert!(r
            .items
            .iter()
            .filter(|i| i.item != "Core")
            .all(|i| i.status == RestoreStatus::Restored));
        let h = c.platform().host.borrow();
        assert_eq!(
            (h.timezone.windows_id.as_str(), h.proxy.proxy_server.clone()),
            ("China Standard Time", None)
        );
    }

    fn system_scope() -> AgentProfile {
        let mut prof = profile();
        prof.proxy_scope.mode = ProxyScopeMode::SystemAndExplicit;
        prof
    }

    fn g6(r: &CheckReport) -> &CheckResult {
        r.results.iter().find(|r| r.id == "G6").expect("G6 present")
    }

    #[test]
    fn desktop_system_scope_needs_no_entry_and_restores_without_closing_foreign_apps() {
        let d = tmp("desktop-scope");
        let p = FakePlatform::new();
        p.entries.borrow_mut().push(999); // Represents a user's already-running desktop app.
        let before = p.host.borrow().proxy.clone();
        let mut c = open(p, &d, true);
        let prof = system_scope();
        let out = c.enter(&prof, EnterOptions::default()).unwrap();
        assert_eq!(g6(out.report.as_ref().unwrap()).status, Status::Pass);
        assert!(c
            .state()
            .journal
            .iter()
            .all(|j| !matches!(j, JournalEntry::Entry { .. })));
        assert_eq!(
            c.platform().host.borrow().proxy.proxy_server.as_deref(),
            Some("127.0.0.1:17893")
        );
        let restored = c.exit().unwrap();
        assert_eq!(restored.mode_after, Mode::Daily);
        assert_eq!(c.platform().host.borrow().proxy, before);
        assert_eq!(*c.platform().entries.borrow(), vec![999]);
    }

    #[test]
    fn an_external_proxy_change_is_reported_without_competing_writes() {
        let d = tmp("keep");
        let mut c = open(FakePlatform::new(), &d, true);
        let prof = system_scope();
        let out = c.enter(&prof, EnterOptions::default()).unwrap();
        assert_eq!(g6(out.report.as_ref().unwrap()).status, Status::Pass);
        assert_eq!(
            c.keep_system_proxy(),
            None,
            "nothing to do while it is ours"
        );
        let ours = c.platform().host.borrow().proxy.clone();
        // A Clash-style client turns its own "system proxy" switch on. We report the conflict,
        // invalidate G6, and leave its value untouched so neither application fights for ownership.
        let clash = ProxySettings {
            flags: proxy_flags::DIRECT | proxy_flags::PROXY,
            proxy_server: Some("127.0.0.1:7890".into()),
            bypass: None,
            auto_config_url: None,
        };
        let writes = c.platform().writes.borrow().len();
        c.platform().host.borrow_mut().proxy = clash.clone();
        assert_eq!(
            c.keep_system_proxy(),
            Some(KeepOutcome::Changed {
                found: "127.0.0.1:7890".into()
            })
        );
        assert_eq!(c.platform().host.borrow().proxy, clash);
        assert_eq!(
            c.platform().writes.borrow().len(),
            writes,
            "no competing write"
        );
        assert_eq!(c.state().mode, Mode::AgentUnverified);
        assert_eq!(
            g6(c.state().last_report.as_ref().unwrap()).status,
            Status::Fail
        );
        // The unchanged conflict is not repeatedly announced or rewritten.
        assert_eq!(c.keep_system_proxy(), None);
        assert_eq!(c.platform().host.borrow().proxy, clash);
        assert_eq!(c.platform().writes.borrow().len(), writes, "still no write");
        // Once the user turns the other client's system proxy switch off, G6 returns.
        c.platform().host.borrow_mut().proxy = ours;
        assert_eq!(c.keep_system_proxy(), None);
        assert_eq!(c.state().mode, Mode::AgentReady);
        assert_eq!(
            g6(c.state().last_report.as_ref().unwrap()).status,
            Status::Pass
        );
    }

    #[test]
    fn unreadable_proxy_invalidates_previous_pass_without_writing() {
        let d = tmp("keep-read-failed");
        let mut c = open(FakePlatform::new(), &d, true);
        c.enter(&system_scope(), EnterOptions::default()).unwrap();
        assert_eq!(c.state().mode, Mode::AgentReady);
        let writes = c.platform().writes.borrow().len();
        c.platform().fail_read_global.set(true);
        assert!(matches!(
            c.keep_system_proxy(),
            Some(KeepOutcome::Failed(_))
        ));
        assert_eq!(c.state().mode, Mode::AgentUnverified);
        assert_eq!(
            g6(c.state().last_report.as_ref().unwrap()).status,
            Status::Unknown
        );
        assert_eq!(c.platform().writes.borrow().len(), writes);
        let saved: ControllerState =
            serde_json::from_slice(&std::fs::read(d.join("state.json")).unwrap()).unwrap();
        assert_eq!(saved.mode, Mode::AgentUnverified);
    }

    #[test]
    fn without_system_scope_there_is_no_g6_and_nothing_to_keep() {
        let d = tmp("nokeep");
        let mut c = open(FakePlatform::new(), &d, true);
        let out = c.enter(&profile(), EnterOptions::default()).unwrap();
        assert!(out.report.unwrap().results.iter().all(|r| r.id != "G6"));
        assert_eq!(c.keep_system_proxy(), None);
    }

    #[test]
    fn system_level_routing_switched_off_reads_as_unknown_not_pass() {
        let d = tmp("g6off");
        // System scope in the profile, but the snapshot could not be persisted for this item: use
        // a legacy mutex, which still holds both items back.
        let p = FakePlatform::new();
        p.host
            .borrow_mut()
            .mutexes
            .insert("Local\\AIEgressGuard".into());
        let mut c = open(p, &d, true);
        let out = c.enter(&system_scope(), EnterOptions::default()).unwrap();
        let r = out.report.unwrap();
        assert_eq!(
            (g6(&r).status, g6(&r).reason.as_deref()),
            (Status::Unknown, Some("global_state_not_taken_over"))
        );
        assert!(g6(&r)
            .evidence
            .iter()
            .any(|e| e.detail.contains("AIEgressGuard")));
        assert_eq!(c.keep_system_proxy(), None, "never written, so never kept");
    }

    #[test]
    fn foreign_writer_keeps_the_gate_closed() {
        let d = tmp("writer");
        let p = FakePlatform::new();
        p.host
            .borrow_mut()
            .mutexes
            .insert("Local\\AIEgressGuard".into());
        let mut c = open(p, &d, true);
        let out = c.enter(&profile(), EnterOptions::default()).unwrap();
        assert_eq!(out.gate.state, GateState::Closed);
        assert!(out.gate.reasons.iter().any(|r| r.contains("AIEgressGuard")));
        assert!(c.platform().writes.borrow().is_empty());
    }

    #[test]
    fn changed_proxy_settings_that_still_reference_aec_do_not_complete_restoration() {
        for server in [
            "127.0.0.1:17893",
            "http=127.0.0.1:7897;https=127.0.0.1:17893",
        ] {
            let d = tmp("proxy-still-owned");
            let mut c = open(FakePlatform::new(), &d, true);
            c.enter(&system_scope(), EnterOptions::default()).unwrap();
            // FakePlatform uses port 17893. Changing bypass or just one protocol does not
            // hand the whole system proxy back to an independent client.
            c.platform().host.borrow_mut().proxy.proxy_server = Some(server.into());
            c.platform().host.borrow_mut().proxy.bypass = Some("external-change".into());
            let report = c.exit().unwrap();
            assert_eq!(report.mode_after, Mode::RestoreIncomplete, "{server}");
            assert!(report
                .items
                .iter()
                .any(|i| i.status == RestoreStatus::Conflict));
        }
    }

    #[test]
    fn exit_to_daily_keeps_the_users_new_proxy_without_an_extra_acknowledgement() {
        let d = tmp("proxy-exit-daily");
        let mut c = open(FakePlatform::new(), &d, true);
        c.enter(&system_scope(), EnterOptions::default()).unwrap();
        let proxy = ProxySettings {
            flags: proxy_flags::DIRECT | proxy_flags::PROXY,
            proxy_server: Some("127.0.0.1:7897".into()),
            bypass: None,
            auto_config_url: None,
        };
        c.platform().host.borrow_mut().proxy = proxy.clone();
        c.keep_system_proxy();
        let report = c.exit().unwrap();
        assert_eq!(
            report.mode_after,
            Mode::Daily,
            "An external proxy choice is not a failed restoration"
        );
        assert_eq!(c.platform().host.borrow().proxy, proxy);
        assert!(c.state().journal.is_empty());
        assert!(c.state().unacknowledged_restore_failures.is_empty());
        assert!(!c.platform().core_running.get());
    }

    #[test]
    fn external_change_is_a_conflict_not_overwritten() {
        let d = tmp("conflict");
        let mut c = open(FakePlatform::new(), &d, true);
        c.enter(&profile(), EnterOptions::default()).unwrap();
        c.platform().host.borrow_mut().timezone.windows_id = "Tokyo Standard Time".into();
        let r = c.exit().unwrap();
        assert_eq!(r.mode_after, Mode::RestoreIncomplete);
        assert!(r.items.iter().any(|i| i.status == RestoreStatus::Conflict));
        assert_eq!(
            c.platform().host.borrow().timezone.windows_id,
            "Tokyo Standard Time",
            "never overwritten"
        );
        assert!(!c.platform().core_running.get(), "the core still stops");
        c.acknowledge().unwrap();
        assert_eq!(c.state().mode, Mode::Daily);
        assert!(c.state().journal.is_empty());
    }

    #[test]
    fn failed_restore_can_be_retried() {
        let d = tmp("retry");
        let mut c = open(FakePlatform::new(), &d, true);
        c.enter(&profile(), EnterOptions::default()).unwrap();
        c.platform().fail_restore_write.set(true);
        let r = c.exit().unwrap();
        assert_eq!(r.mode_after, Mode::RestoreIncomplete);
        assert_eq!(c.state().unacknowledged_restore_failures.len(), 1);
        assert_eq!(
            c.state().journal.len(),
            1,
            "only the failed item stays for the retry"
        );
        c.platform().fail_restore_write.set(false);
        let r = c.exit().unwrap();
        assert_eq!(r.mode_after, Mode::Daily, "{r:?}");
        assert_eq!(
            c.platform().host.borrow().timezone.windows_id,
            "China Standard Time"
        );
    }

    #[test]
    fn interrupted_session_is_reported_not_acted_on() {
        let d = tmp("crash");
        {
            let mut c = open(FakePlatform::new(), &d, true);
            c.enter(&profile(), EnterOptions::default()).unwrap();
            // The process "dies" here: no exit.
        }
        let p = FakePlatform::new();
        p.host.borrow_mut().timezone.windows_id = "Eastern Standard Time".into();
        let mut c = open(p, &d, true);
        assert!(c.recovery_pending());
        assert_eq!(c.state().mode, Mode::AgentReady);
        assert!(
            c.platform().writes.borrow().is_empty(),
            "opening must not write anything"
        );
        let r = c.exit().unwrap();
        assert_eq!(r.mode_after, Mode::Daily);
        assert_eq!(
            c.platform().host.borrow().timezone.windows_id,
            "China Standard Time"
        );
        assert!(!c.recovery_pending());
    }

    fn with_entries(ids: &[&str]) -> EnterOptions {
        EnterOptions {
            entries: ids.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn result<'a>(r: &'a CheckReport, id: &str) -> &'a CheckResult {
        r.results
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no {id}"))
    }

    #[test]
    fn browser_entry_is_launched_checked_and_closed() {
        let d = tmp("entry");
        let mut c = open(FakePlatform::new(), &d, true);
        let out = c.enter(&profile(), with_entries(&["edge"])).unwrap();
        let rep = out.report.unwrap();
        assert_eq!(result(&rep, "G5:edge").status, Status::Pass);
        assert!(rep.results.iter().any(|r| r.id == "R3:edge"));
        assert_eq!(out.mode, Mode::AgentReady);
        assert_eq!(c.platform().entries.borrow().len(), 1);
        let r = c.exit().unwrap();
        assert_eq!(r.items[0].item, "Entry:edge", "entries close first");
        assert_eq!(r.mode_after, Mode::Daily);
        assert!(c.platform().entries.borrow().is_empty());
    }

    #[test]
    fn entries_opened_in_agent_mode_are_checked_and_closed_on_exit() {
        let d = tmp("open-entry");
        let mut c = open(FakePlatform::new(), &d, true);
        assert_eq!(
            c.open_entry(&profile(), "edge").unwrap_err().code,
            "STATE_INVALID",
            "only in Agent mode"
        );
        c.enter(&profile(), EnterOptions::default()).unwrap();
        let rep = c.open_entry(&profile(), "edge").unwrap();
        assert_eq!(result(&rep, "G5:edge").status, Status::Pass);
        assert_eq!(c.state().mode, Mode::AgentReady);
        assert_eq!(
            c.open_entry(&profile(), "nope").unwrap_err().code,
            "ENTRY_UNKNOWN"
        );
        let r = c.exit().unwrap();
        assert!(r.items.iter().any(|i| i.item == "Entry:edge"));
        assert!(c.platform().entries.borrow().is_empty());
    }

    #[test]
    fn missing_or_wrong_browser_report_blocks_ready() {
        let d = tmp("entry-none");
        let p = FakePlatform::new();
        *p.signals.borrow_mut() = None;
        let mut c = open(p, &d, true);
        let out = c.enter(&profile(), with_entries(&["edge"])).unwrap();
        assert_eq!(out.mode, Mode::AgentUnverified);
        assert_eq!(
            result(out.report.as_ref().unwrap(), "G5:edge")
                .reason
                .as_deref(),
            Some("no_report")
        );

        let d = tmp("entry-tz");
        let p = FakePlatform::new();
        p.signals.borrow_mut().as_mut().unwrap().timezone = Some("Asia/Shanghai".into());
        let mut c = open(p, &d, true);
        let out = c.enter(&profile(), with_entries(&["edge"])).unwrap();
        assert_eq!(
            result(out.report.as_ref().unwrap(), "G4").reason.as_deref(),
            Some("browser_timezone_mismatch")
        );
        assert_eq!(out.mode, Mode::AgentUnverified);
    }

    #[test]
    fn failed_launch_closes_the_entries_already_started() {
        let d = tmp("entry-fail");
        let mut c = open(FakePlatform::failing("launch:chrome"), &d, true);
        let out = c
            .enter(&profile(), with_entries(&["edge", "chrome"]))
            .unwrap();
        assert_eq!(out.fault.unwrap().code, "TEST_FAULT");
        assert_eq!(out.mode, Mode::Daily);
        assert!(
            c.platform().entries.borrow().is_empty(),
            "edge was closed by the rollback"
        );
        let d = tmp("entry-unknown");
        let mut c = open(FakePlatform::new(), &d, true);
        assert_eq!(
            c.enter(&profile(), with_entries(&["opera"]))
                .unwrap()
                .fault
                .unwrap()
                .code,
            "ENTRY_UNKNOWN"
        );
    }

    #[test]
    fn entry_that_will_not_close_is_kept_for_a_retry() {
        let d = tmp("entry-stubborn");
        let mut c = open(FakePlatform::new(), &d, true);
        c.enter(&profile(), with_entries(&["edge"])).unwrap();
        c.platform().stubborn.set(true);
        let r = c.exit().unwrap();
        assert_eq!(r.mode_after, Mode::RestoreIncomplete);
        assert!(r
            .items
            .iter()
            .any(|i| i.item == "Entry:edge" && i.status == RestoreStatus::Failed));
        assert!(
            !c.platform().core_running.get(),
            "the rest of the restore still runs"
        );
        c.platform().stubborn.set(false);
        assert_eq!(c.exit().unwrap().mode_after, Mode::Daily);
        assert!(c.platform().entries.borrow().is_empty());
    }

    #[test]
    fn cli_entries_are_left_to_the_user_and_do_not_block_daily() {
        let d = tmp("cli");
        let mut c = open(FakePlatform::new(), &d, true);
        let out = c.enter(&profile(), with_entries(&["claude-cli"])).unwrap();
        assert!(
            !out.report
                .unwrap()
                .results
                .iter()
                .any(|r| r.id.ends_with(":claude-cli")),
            "no browser checks for a CLI entry"
        );
        let r = c.exit().unwrap();
        assert_eq!(r.mode_after, Mode::Daily);
        assert!(r
            .items
            .iter()
            .any(|i| i.item == "Entry:claude-cli" && i.status == RestoreStatus::LeftOpen));
        assert!(c.state().journal.is_empty());
    }

    #[test]
    fn enter_twice_is_refused_and_recheck_needs_agent_mode() {
        let d = tmp("twice");
        let mut c = open(FakePlatform::new(), &d, false);
        assert_eq!(c.recheck(&profile()).unwrap_err().code, "STATE_INVALID");
        c.enter(&profile(), EnterOptions::default()).unwrap();
        assert_eq!(
            c.enter(&profile(), EnterOptions::default())
                .unwrap_err()
                .code,
            "STATE_INVALID"
        );
        // G1, G3, three G2, plus G4 and R10 from the controller.
        assert_eq!(c.recheck(&profile()).unwrap().results.len(), 7);
    }
}
