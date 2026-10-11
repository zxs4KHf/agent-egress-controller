//! `aec-dev`: development command line for the core library. It never writes system state: the only
//! things it changes are files under the development data directory and its own core process.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aec_core::model::ProxySettings;
use aec_core::paths::RunMode;
use aec_core::profile;
use aec_core::snapshot::EnvironmentSnapshot;
use aec_core::status::{self, EnvironmentStatus, Reading};
use aec_core::store::{ProfileStore, SnapshotStore};

/// Repository root for development builds: two levels above this crate's manifest.
fn repo_root() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));
    aec_core::paths::strip_verbatim(&p)
}

fn show<T>(r: &Reading<T>, f: impl Fn(&T) -> String) -> String {
    match r {
        Reading::Ok { value } => f(value),
        Reading::Unreadable { error } => format!("UNREADABLE ({error})"),
    }
}

fn describe_proxy(p: &ProxySettings) -> String {
    format!(
        "flags=0x{:x} proxy={} pac={} autodetect={} server={} bypass={} pac_url={}",
        p.flags,
        p.proxy_enabled(),
        p.pac_enabled(),
        p.auto_detect(),
        p.proxy_server.as_deref().unwrap_or("-"),
        p.bypass.as_deref().unwrap_or("-"),
        p.auto_config_url.as_deref().unwrap_or("-"),
    )
}

fn print_human(s: &EnvironmentStatus, data_dir: &Path) {
    println!("data dir (dev):        {}", data_dir.display());
    println!(
        "system proxy (WinINet): {}",
        show(&s.system_proxy, describe_proxy)
    );
    println!(
        "system proxy (registry): {}",
        show(&s.system_proxy_registry, |r| format!(
            "ProxyEnable={} ProxyServer={} ProxyOverride={} AutoConfigURL={}",
            r.proxy_enable
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
            r.proxy_server.as_deref().unwrap_or("-"),
            r.proxy_override.as_deref().unwrap_or("-"),
            r.auto_config_url.as_deref().unwrap_or("-"),
        ))
    );
    println!(
        "timezone:              {}",
        show(&s.timezone, |t| format!(
            "{} (dynamic DST disabled: {})",
            t.windows_id, t.dynamic_daylight_disabled
        ))
    );
    let w = &s.foreign_writers;
    println!(
        "foreign writers:       {} found, {} probe error(s){}",
        w.writers.len(),
        w.errors.len(),
        if w.is_clear() { " - clear" } else { "" }
    );
    for f in &w.writers {
        println!("  - {:?}: {}", f.kind, f.evidence);
    }
    for e in &w.errors {
        println!("  ! {e}");
    }
}

const USAGE: &str = "usage:
  aec-dev status [--json]          read system proxy, time zone and foreign writers
  aec-dev profile init             write default profiles if none exist
  aec-dev profile check            validate the profile file field by field
  aec-dev snapshot [--current]     record an environment snapshot (reads the system, writes only the data dir)
  aec-dev snapshots                list stored snapshots
  aec-dev core install <exe>       copy a mihomo executable into runtime\\bin after checking the pinned sha256
  aec-dev core config --direct     print the generated direct test configuration (secrets redacted)
  aec-dev core up --direct         start this application's core on 17xxx with the direct test configuration
  aec-dev core status              show the core binary, process and port state
  aec-dev core down                stop this application's core and delete its generated configuration
  aec-dev check [--local-only] [--proxy-port N] [--json]
                                   run G1, G2, R1, R2, R9 through this application's core, or through
                                   another proxy port (observation only; nothing is reconfigured)
  aec-dev enter [--direct] [--entry ID]...
                                   enter Agent mode (global changes are dry-run in this version);
                                   --entry starts a profile entry, e.g. edge or chrome
  aec-dev recheck                  re-run the checks in Agent mode
  aec-dev exit                     leave Agent mode, or recover an interrupted session, or retry a failed restore
  aec-dev ack                      accept remaining restore failures and return to daily mode
  aec-dev state                    show the persisted controller state
  aec-dev logs [N]                 show the last N structured log lines (default 30)
  aec-dev which COMMAND...         resolve CLI entries on the registry PATH, skipping egress shims
  aec-dev prompt [ID|--all] [--mask-ip]
                                   print troubleshooting prompts for the last report, restore
                                   failures, profile gaps and the takeover gate (nothing is sent)";

#[cfg(windows)]
fn prompt_cmd(args: &[String], data_dir: &Path) -> ExitCode {
    use aec_core::controller::{Controller, Settings};
    use aec_core::live::LivePlatform;
    use aec_core::prompts::{self, PromptContext};
    use aec_core::snapshot::TakeoverGate;

    let set = ProfileStore::new(data_dir).load().unwrap_or_default();
    let profile = set
        .agent
        .first()
        .cloned()
        .expect("profile sets have an agent profile");
    let settings_gaps = profile::validate(&set);
    let c = match Controller::open(
        LivePlatform::dev(&repo_root(), data_dir),
        data_dir,
        Settings::default(),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let st = status::collect(&host(), Some(data_dir));
    // Takeover is not configurable before ticket 13, so the gate is evaluated with it off.
    let gate = TakeoverGate::evaluate(false, &st.foreign_writers, true);
    let data_dir_text = data_dir.display().to_string();
    let ctx = PromptContext {
        mode: c.state().mode.as_str(),
        checked_at: c
            .state()
            .last_report
            .as_ref()
            .map(|r| r.generated_at.as_str()),
        profile: &profile,
        data_dir: &data_dir_text,
        mask_ip: args.iter().any(|a| a == "--mask-ip"),
    };
    let report = c.state().last_report.as_ref();
    let text = match args.iter().find(|a| !a.starts_with("--")) {
        Some(id) => match report.and_then(|r| r.results.iter().find(|c| &c.id == id)) {
            Some(check) => prompts::for_check(check, &ctx)
                .unwrap_or_else(|| format!("{id} passed; nothing to troubleshoot.")),
            None => {
                eprintln!("no check {id} in the last report");
                return ExitCode::from(2);
            }
        },
        None => prompts::all(
            report,
            &c.state().unacknowledged_restore_failures,
            &settings_gaps,
            Some(&gate),
            &ctx,
        ),
    };
    println!("{text}");
    ExitCode::SUCCESS
}

#[cfg(windows)]
fn mode_cmd(cmd: &str, args: &[String], data_dir: &Path) -> ExitCode {
    use aec_core::controller::{Controller, EnterOptions, Settings};
    use aec_core::live::LivePlatform;

    let profile = match ProfileStore::new(data_dir).load() {
        Ok(set) => set
            .agent
            .into_iter()
            .next()
            .expect("profile sets have an agent profile"),
        Err(_) => profile::ProfileSet::default().agent.remove(0),
    };
    // No settings store yet (ticket 13): takeover stays off, so every global change is a dry-run.
    let mut c = match Controller::open(
        LivePlatform::dev(&repo_root(), data_dir),
        data_dir,
        Settings {
            takeover_enabled: false,
        },
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if c.recovery_pending() && cmd != "exit" && cmd != "state" && cmd != "ack" {
        println!(
            "an earlier Agent session did not finish (mode {}); run `aec-dev exit` to restore",
            c.state().mode.as_str()
        );
    }
    let print = |v: &dyn erased::Json| println!("{}", v.json());
    let code = match cmd {
        "enter" => match c.enter(
            &profile,
            EnterOptions {
                direct_test: args.iter().any(|a| a == "--direct"),
                // --entry <id>, repeatable
                entries: args
                    .windows(2)
                    .filter(|w| w[0] == "--entry")
                    .map(|w| w[1].clone())
                    .collect(),
            },
        ) {
            Ok(out) => {
                print(&out);
                if out.fault.is_some() {
                    5
                } else {
                    0
                }
            }
            Err(f) => {
                eprintln!("error: {f}");
                5
            }
        },
        "recheck" => match c.recheck(&profile) {
            Ok(r) => {
                print(&r);
                0
            }
            Err(f) => {
                eprintln!("error: {f}");
                5
            }
        },
        "exit" => match c.exit() {
            Ok(r) => {
                print(&r);
                0
            }
            Err(f) => {
                eprintln!("error: {f}");
                5
            }
        },
        "ack" => match c.acknowledge() {
            Ok(()) => {
                println!("acknowledged; back in daily mode");
                0
            }
            Err(f) => {
                eprintln!("error: {f}");
                5
            }
        },
        _ => {
            print(c.state());
            0
        }
    };
    ExitCode::from(code)
}

/// Pretty JSON for any serializable value behind a trait object.
mod erased {
    pub trait Json {
        fn json(&self) -> String;
    }
    impl<T: serde::Serialize> Json for T {
        fn json(&self) -> String {
            serde_json::to_string_pretty(self).expect("serializes")
        }
    }
}

#[cfg(windows)]
fn check_cmd(args: &[String], data_dir: &Path) -> ExitCode {
    use aec_core::checks::{self, CheckInput, Status};
    use aec_core::core::process::{http_get_local, CoreManager};
    use aec_core::providers::ProviderStore;

    let flag_value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
    };
    let mut profile = match ProfileStore::new(data_dir).load() {
        Ok(set) => set
            .agent
            .into_iter()
            .next()
            .expect("validated sets have an agent profile"),
        Err(_) => profile::ProfileSet::default().agent.remove(0),
    };
    if args.iter().any(|a| a == "--local-only") {
        profile.detection.mode = profile::DetectionMode::LocalOnly;
    }
    let providers = match ProviderStore::new(data_dir).load() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (port, core) = match flag_value("--proxy-port").and_then(|v| v.parse::<u16>().ok()) {
        Some(port) => {
            let up = std::net::TcpStream::connect_timeout(
                &([127, 0, 0, 1], port).into(),
                std::time::Duration::from_secs(2),
            );
            (
                port,
                up.map(|_| ())
                    .map_err(|e| format!("nothing answers on 127.0.0.1:{port}: {e}")),
            )
        }
        None => {
            let m = CoreManager::dev(&repo_root(), data_dir);
            let secret = std::fs::read_to_string(m.config_path()).ok().and_then(|t| {
                t.lines().find_map(|l| {
                    l.strip_prefix("secret: ")
                        .map(|v| v.trim_matches('"').to_string())
                })
            });
            let core = match (m.status(&host()).alive, secret) {
                (true, Some(s)) => match http_get_local(m.ports.controller, "/version", Some(&s)) {
                    Ok((200, _)) => Ok(()),
                    other => Err(format!("controller did not answer: {other:?}")),
                },
                _ => {
                    Err("this application's core is not running (aec-dev core up --direct)".into())
                }
            };
            (m.ports.mixed, core)
        }
    };

    // Only this application's own core has a log to read; an observed proxy leaves G3 unknown.
    let observer = flag_value("--proxy-port").is_none().then(|| {
        aec_core::live::CoreLogObserver::for_core(&CoreManager::dev(&repo_root(), data_dir))
    });
    let input = CheckInput {
        profile: &profile,
        providers: &providers,
        proxy_port: port,
        core,
        observer: observer.as_ref().map(|o| o as &dyn checks::CoreObserver),
        system_dns: aec_core::windows::system_dns_servers().map_err(|e| e.to_string()),
        generated_at: aec_core::snapshot::rfc3339_utc(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        ),
    };
    let report = checks::run(&input, &aec_core::http::WinHttpClient);
    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serializes")
        );
    } else {
        println!(
            "checks via 127.0.0.1:{} mode={:?} at {}",
            report.proxy_port, report.mode, report.generated_at
        );
        for r in &report.results {
            let s = match r.status {
                Status::Pass => "PASS",
                Status::Warn => "WARN",
                Status::Fail => "FAIL",
                Status::Unknown => "UNKNOWN",
            };
            println!(
                "  {:<22} {:<7} {}",
                r.id,
                s,
                r.reason.as_deref().unwrap_or("")
            );
            for e in &r.evidence {
                println!("      {}: {}", e.source, e.detail);
            }
        }
        println!(
            "hard checks in this report pass: {}",
            report.hard_checks_pass()
        );
    }
    if report.hard_checks_pass() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(4)
    }
}

#[cfg(windows)]
fn core_cmd(args: &[String], data_dir: &Path) -> ExitCode {
    use aec_core::core::{config, process::CoreManager};
    let mut m = CoreManager::dev(&repo_root(), data_dir);
    // Test hook: AEC_DEV_PORTS="mixed,controller,dns" moves the core off the default ports.
    if let Ok(v) = std::env::var("AEC_DEV_PORTS") {
        let p: Vec<u16> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        match p[..] {
            [mixed, controller, dns] => {
                m.ports = aec_core::core::CorePorts {
                    mixed,
                    controller,
                    dns,
                    ..Default::default()
                }
            }
            [mixed, controller, dns, relay] => {
                m.ports = aec_core::core::CorePorts {
                    mixed,
                    controller,
                    dns,
                    relay,
                }
            }
            _ => {}
        }
    }
    let direct = args.iter().any(|a| a == "--direct");
    let group = ProfileStore::new(data_dir)
        .load()
        .ok()
        .and_then(|s| s.agent.first().map(|p| p.routing.target_group.clone()))
        .unwrap_or_else(|| "agent-egress".into());
    let fail = |e: &dyn std::fmt::Display| {
        eprintln!("error: {e}");
        ExitCode::FAILURE
    };
    match args.first().map(String::as_str) {
        Some("install") => match args.get(1) {
            Some(src) => match m.install(Path::new(src)) {
                Ok(()) => {
                    println!(
                        "installed {} ({})",
                        m.bin.display(),
                        aec_core::core::pin::VERSION
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&e),
            },
            None => fail(&"core install needs the path of a mihomo executable"),
        },
        Some("config") if direct => {
            print!(
                "{}",
                config::redact(&config::direct_test(
                    &m.ports,
                    "<generated at start>",
                    &group
                ))
            );
            ExitCode::SUCCESS
        }
        Some("up") if direct => match m.up(&host(), |p, s| Ok(config::direct_test(p, s, &group))) {
            Ok(o) => {
                println!(
                    "core running: pid {} {} (mixed {}, controller {}, dns {})",
                    o.pid, o.exe, m.ports.mixed, m.ports.controller, m.ports.dns
                );
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        },
        // Profile mode: relay in front of the profile's candidates, then the core. Until ticket 13
        // only candidates without a credentialRef can be used.
        Some(sub @ ("up" | "config")) => {
            let profile = match ProfileStore::new(data_dir).load() {
                Ok(set) => set
                    .agent
                    .into_iter()
                    .next()
                    .expect("profile sets have an agent profile"),
                Err(e) => return fail(&e),
            };
            let relay = match config::relay_config(&profile, &m.ports, &config::NoCredentials) {
                Ok(r) => r,
                Err(e) => return fail(&e),
            };
            if sub == "config" {
                print!(
                    "{}",
                    config::redact(
                        &config::from_profile(&profile, &m.ports, "<generated at start>")
                            .expect("relay_config succeeded")
                    )
                );
                println!(
                    "# relay {} -> {}",
                    relay.listen,
                    relay
                        .candidates
                        .iter()
                        .map(aec_core::relay::Upstream::label)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                return ExitCode::SUCCESS;
            }
            match m.up_with_relay(&host(), &relay, |p, s| {
                config::from_profile(&profile, p, s).map_err(|e| e.to_string())
            }) {
                Ok(o) => {
                    println!(
                        "relay on {} and core pid {} running (mixed {})",
                        relay.listen, o.pid, m.ports.mixed
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&e),
            }
        }
        Some("status") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&m.status(&host())).expect("serializes")
            );
            ExitCode::SUCCESS
        }
        Some("down") => match m.down(&host()) {
            Ok(true) => {
                println!("core stopped; generated configuration deleted");
                ExitCode::SUCCESS
            }
            Ok(false) => {
                println!("no running core of this application");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn host() -> impl status::Host {
    #[cfg(windows)]
    return aec_core::windows::WindowsHost;
    #[cfg(not(windows))]
    return aec_core::testing::FakeHost::default();
}

fn profile_cmd(sub: Option<&str>, data_dir: &Path) -> ExitCode {
    let store = ProfileStore::new(data_dir);
    match sub {
        Some("init") => match store.init_if_missing() {
            Ok(true) => {
                println!("wrote default profiles to {}", store.path().display());
                ExitCode::SUCCESS
            }
            Ok(false) => {
                println!(
                    "profiles already exist at {}; left unchanged",
                    store.path().display()
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("check") => match store.load() {
            Ok(set) => {
                let errors = profile::validate(&set);
                if errors.is_empty() {
                    println!("profiles valid: {}", store.path().display());
                    ExitCode::SUCCESS
                } else {
                    println!("{} problem(s) in {}:", errors.len(), store.path().display());
                    for e in errors {
                        println!("  {}: {}", e.path, e.message);
                    }
                    ExitCode::from(3)
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn snapshot_cmd(make_current: bool, data_dir: &Path) -> ExitCode {
    let store = SnapshotStore::new(data_dir);
    let profile_id = ProfileStore::new(data_dir)
        .load()
        .ok()
        .and_then(|s| s.agent.first().map(|p| p.id.clone()));
    let st = status::collect(&host(), Some(data_dir));
    let (id, ms) = store.next_id();
    let snap = EnvironmentSnapshot::from_status(id, ms, &st, profile_id, false);
    match store.save(&snap, make_current) {
        Ok(removed) => {
            println!(
                "snapshot {} saved{} (restorable: {}, takeover gate: {:?})",
                snap.id,
                if make_current { " as current" } else { "" },
                snap.is_restorable(),
                snap.takeover_gate.state
            );
            for r in &snap.takeover_gate.reasons {
                println!("  gate closed: {r}");
            }
            if !removed.is_empty() {
                println!("  pruned: {}", removed.join(", "));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn snapshots_cmd(data_dir: &Path) -> ExitCode {
    let store = SnapshotStore::new(data_dir);
    match store.list() {
        Ok(list) => {
            println!("{} snapshot(s) in {}", list.len(), store.dir().display());
            for e in list {
                let mark = if e.current { "*" } else { " " };
                match e.snapshot {
                    Ok(s) => println!(
                        "{mark} {}  {}  proxy={}  tz={}  writers={}",
                        s.id,
                        s.taken_at,
                        show(&s.system_proxy, |p| p
                            .proxy_server
                            .clone()
                            .unwrap_or_else(|| "-".into())),
                        show(&s.timezone, |t| t.windows_id.clone()),
                        s.foreign_writers.writers.len()
                    ),
                    Err(err) => println!("{mark} {}  UNREADABLE: {err}", e.id),
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    // Test hook: AEC_DEV_DATA_DIR points the command line at a scratch data directory.
    let data_dir = std::env::var_os("AEC_DEV_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            RunMode::Dev {
                repo_root: repo_root(),
            }
            .data_dir()
        });

    match args.first().map(String::as_str) {
        Some("status") => {
            let s = status::collect(&host(), Some(&data_dir));
            if has("--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&s).expect("status serializes")
                );
            } else {
                print_human(&s, &data_dir);
            }
            ExitCode::SUCCESS
        }
        Some("profile") => profile_cmd(args.get(1).map(String::as_str), &data_dir),
        Some("snapshot") => snapshot_cmd(has("--current"), &data_dir),
        Some("snapshots") => snapshots_cmd(&data_dir),
        #[cfg(windows)]
        Some("check") => check_cmd(&args[1..], &data_dir),
        #[cfg(windows)]
        Some(cmd @ ("enter" | "recheck" | "exit" | "ack" | "state")) => {
            mode_cmd(cmd, &args[1..], &data_dir)
        }
        #[cfg(windows)]
        Some("prompt") => prompt_cmd(&args[1..], &data_dir),
        #[cfg(windows)]
        Some("which") => {
            use aec_core::cli;
            let dirs = cli::split_path(&aec_core::windows::fresh_path().unwrap_or_default());
            let pathext: Vec<String> = std::env::var("PATHEXT")
                .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
                .split(';')
                .map(str::to_string)
                .collect();
            for c in &args[1..] {
                let hit = cli::resolve(c, &dirs, &pathext, &|p| p.is_file(), &|d| {
                    cli::is_shim_dir(d)
                });
                let shimmed = cli::resolve(c, &dirs, &pathext, &|p| p.is_file(), &|_| false);
                println!(
                    "{c}: {} (without skipping shims: {})",
                    hit.map_or("not found".into(), |p| p.display().to_string()),
                    shimmed.map_or("not found".into(), |p| p.display().to_string())
                );
            }
            ExitCode::SUCCESS
        }
        Some("logs") => {
            let n = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);
            for line in aec_core::log::Logger::new(&data_dir).tail(n) {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        #[cfg(windows)]
        Some("core") => core_cmd(&args[1..], &data_dir),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
