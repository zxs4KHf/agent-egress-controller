//! Foreign writer detection: programs other than this one that rewrite the same global state.
//!
//! A probe that fails is reported as an error next to the findings, never as "nothing found": an
//! unknown must not open the takeover gate (SPEC 4.5, 4.7).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{ProxySettings, StartupItem};
use crate::probe::WriterEvidence;

/// Mutexes held by the legacy chain's guard and tray.
pub const LEGACY_MUTEXES: [(&str, WriterKind); 2] = [
    ("Local\\AIEgressGuard", WriterKind::LegacyGuardMutex),
    ("Local\\AIEgressModeTray", WriterKind::LegacyTrayMutex),
];

/// Text that identifies a startup item belonging to an egress controller other than this one.
/// Matched case-insensitively against the item name and content.
pub const STARTUP_SIGNATURES: [&str; 4] = [
    "ai-egress.vbs",
    "ai-mode-tray.vbs",
    "startup-egress.ps1",
    "mode-tray.ps1",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriterKind {
    LegacyGuardMutex,
    LegacyTrayMutex,
    StartupItem,
    /// The system proxy points at a loopback port held by a process that is not ours.
    ProxyListener,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignWriter {
    pub kind: WriterKind,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WriterScan {
    pub writers: Vec<ForeignWriter>,
    /// Probes that could not run. Non-empty means the scan is incomplete.
    pub errors: Vec<String>,
}

impl WriterScan {
    /// True only when every probe ran and found nothing.
    pub fn is_clear(&self) -> bool {
        self.writers.is_empty() && self.errors.is_empty()
    }
}

/// Loopback port from a WinINet proxy string. Accepts `host:port` and the per-scheme form
/// `http=host:port;https=host:port`, preferring `https`, then `http`, then the first entry.
pub fn loopback_proxy_port(proxy_server: &str) -> Option<u16> {
    let entries: Vec<(&str, &str)> = proxy_server
        .split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| match e.split_once('=') {
            Some((scheme, addr)) => (scheme.trim(), addr.trim()),
            None => ("", e),
        })
        .collect();
    let pick = ["https", "http", ""]
        .iter()
        .find_map(|want| entries.iter().find(|(s, _)| s.eq_ignore_ascii_case(want)))
        .or_else(|| entries.first())?;
    let addr = pick
        .1
        .trim_start_matches("http://")
        .trim_start_matches("socks=");
    let (host, port) = addr.rsplit_once(':')?;
    let host = host.trim_matches(|c| c == '[' || c == ']');
    let loopback = host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost") || host == "::1";
    if !loopback {
        return None;
    }
    port.trim_end_matches('/').parse().ok()
}

fn matches_signature(item: &StartupItem) -> Option<&'static str> {
    let name = item.name.to_ascii_lowercase();
    let content = item.content.as_deref().unwrap_or("").to_ascii_lowercase();
    STARTUP_SIGNATURES
        .iter()
        .copied()
        .find(|sig| name.contains(sig) || content.contains(sig))
}

fn is_own_exe(exe: &str, own_root: Option<&Path>) -> bool {
    match own_root {
        Some(root) => {
            let root = root.to_string_lossy().to_ascii_lowercase();
            let root = root.trim_end_matches('\\');
            exe.to_ascii_lowercase().starts_with(&format!("{root}\\"))
        }
        None => false,
    }
}

/// Scan for foreign writers. `own_root` is the directory this application's own processes run
/// from; a proxy listener under it is not foreign.
pub fn scan(
    evidence: &dyn WriterEvidence,
    proxy: Option<&ProxySettings>,
    own_root: Option<&Path>,
) -> WriterScan {
    let mut scan = WriterScan::default();

    for (name, kind) in LEGACY_MUTEXES {
        match evidence.mutex_exists(name) {
            Ok(true) => scan.writers.push(ForeignWriter {
                kind,
                evidence: format!("{name} is held"),
            }),
            Ok(false) => {}
            Err(e) => scan.errors.push(format!("mutex {name}: {e}")),
        }
    }

    match evidence.startup_items() {
        Ok(items) => {
            for item in &items {
                if let Some(sig) = matches_signature(item) {
                    let place = match item.location {
                        crate::model::StartupLocation::StartupFolder => "startup folder",
                        crate::model::StartupLocation::HkcuRun => "HKCU Run",
                    };
                    scan.writers.push(ForeignWriter {
                        kind: WriterKind::StartupItem,
                        evidence: format!("{place} item '{}' matches '{sig}'", item.name),
                    });
                }
            }
        }
        Err(e) => scan.errors.push(format!("startup items: {e}")),
    }

    match proxy {
        None => scan
            .errors
            .push("system proxy unreadable; proxy listener not checked".into()),
        Some(p) if p.proxy_enabled() => {
            if let Some(port) = p.proxy_server.as_deref().and_then(loopback_proxy_port) {
                match evidence.listener_owner(port) {
                    Ok(Some(owner)) => {
                        let own = owner
                            .exe
                            .as_deref()
                            .map(|e| is_own_exe(e, own_root))
                            .unwrap_or(false);
                        if !own {
                            let exe = owner
                                .exe
                                .clone()
                                .unwrap_or_else(|| "<not readable by this token>".into());
                            scan.writers.push(ForeignWriter {
                                kind: WriterKind::ProxyListener,
                                evidence: format!(
                                    "system proxy port {port} is held by pid {} ({exe})",
                                    owner.pid
                                ),
                            });
                        }
                    }
                    Ok(None) => {}
                    Err(e) => scan.errors.push(format!("listener on {port}: {e}")),
                }
            }
        }
        Some(_) => {}
    }

    scan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{proxy_flags, ListenerOwner, StartupLocation};
    use crate::probe::ProbeError;
    use crate::testing::FakeHost;

    fn proxy(server: &str) -> ProxySettings {
        ProxySettings {
            flags: proxy_flags::DIRECT | proxy_flags::PROXY,
            proxy_server: Some(server.into()),
            bypass: None,
            auto_config_url: None,
        }
    }

    #[test]
    fn parses_loopback_proxy_forms() {
        assert_eq!(loopback_proxy_port("127.0.0.1:7893"), Some(7893));
        assert_eq!(loopback_proxy_port("localhost:8080"), Some(8080));
        assert_eq!(
            loopback_proxy_port("http=127.0.0.1:1;https=127.0.0.1:2"),
            Some(2)
        );
        assert_eq!(
            loopback_proxy_port("ftp=127.0.0.1:3;http=127.0.0.1:4"),
            Some(4)
        );
        assert_eq!(loopback_proxy_port("[::1]:9"), Some(9));
        assert_eq!(loopback_proxy_port("10.0.0.1:7893"), None);
        assert_eq!(loopback_proxy_port("127.0.0.1"), None);
        assert_eq!(loopback_proxy_port(""), None);
    }

    #[test]
    fn clean_machine_is_clear() {
        let host = FakeHost::default();
        let s = scan(&host, Some(&proxy("10.0.0.1:3128")), None);
        assert!(s.is_clear(), "{s:?}");
    }

    #[test]
    fn finds_every_legacy_signal() {
        let mut host = FakeHost::default();
        host.mutexes.insert("Local\\AIEgressGuard".into());
        host.mutexes.insert("Local\\AIEgressModeTray".into());
        host.startup = vec![
            StartupItem {
                location: StartupLocation::StartupFolder,
                name: "AI-Egress.vbs".into(),
                content: Some("…".into()),
            },
            StartupItem {
                location: StartupLocation::HkcuRun,
                name: "x".into(),
                content: Some("powershell -File C:\\a\\scripts\\startup-egress.ps1".into()),
            },
            StartupItem {
                location: StartupLocation::StartupFolder,
                name: "ShareX.lnk".into(),
                content: None,
            },
        ];
        host.listeners.insert(
            7893,
            ListenerOwner {
                port: 7893,
                pid: 42,
                exe: None,
            },
        );
        let s = scan(&host, Some(&proxy("127.0.0.1:7893")), None);
        let kinds: Vec<WriterKind> = s.writers.iter().map(|w| w.kind).collect();
        assert_eq!(
            kinds,
            vec![
                WriterKind::LegacyGuardMutex,
                WriterKind::LegacyTrayMutex,
                WriterKind::StartupItem,
                WriterKind::StartupItem,
                WriterKind::ProxyListener
            ]
        );
        assert!(s.errors.is_empty());
    }

    #[test]
    fn own_listener_is_not_foreign() {
        let mut host = FakeHost::default();
        host.listeners.insert(
            17893,
            ListenerOwner {
                port: 17893,
                pid: 7,
                exe: Some("C:\\App\\Data\\aec-mihomo.exe".into()),
            },
        );
        let s = scan(
            &host,
            Some(&proxy("127.0.0.1:17893")),
            Some(Path::new("C:\\App\\Data")),
        );
        assert!(s.is_clear(), "{s:?}");
        let s = scan(
            &host,
            Some(&proxy("127.0.0.1:17893")),
            Some(Path::new("C:\\App\\Dat")),
        );
        assert_eq!(
            s.writers.len(),
            1,
            "prefix match must stop at a path boundary"
        );
    }

    #[test]
    fn disabled_proxy_does_not_check_listener() {
        let mut host = FakeHost::default();
        host.listeners.insert(
            7893,
            ListenerOwner {
                port: 7893,
                pid: 1,
                exe: None,
            },
        );
        let mut p = proxy("127.0.0.1:7893");
        p.flags = proxy_flags::DIRECT;
        assert!(scan(&host, Some(&p), None).is_clear());
    }

    #[test]
    fn probe_failures_make_the_scan_incomplete() {
        let host = FakeHost {
            fail_mutex: Some(ProbeError::new("OpenMutexW", Some(1314))),
            fail_startup: Some(ProbeError::new("read startup folder", None)),
            ..Default::default()
        };
        let s = scan(&host, None, None);
        assert!(s.writers.is_empty());
        assert_eq!(s.errors.len(), 4, "{s:?}");
        assert!(!s.is_clear());
    }
}
