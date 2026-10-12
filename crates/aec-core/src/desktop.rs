//! Read-only Claude Desktop routing evidence. Core probes and system-proxy settings alone
//! cannot identify the application that originated a request.

use std::net::SocketAddr;

use serde::Deserialize;

use crate::checks::{CheckResult, Kind, Status};
use crate::profile::{AgentProfile, Routing, RoutingGroupKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopFlow {
    pub pid: u32,
    pub created: u64,
    pub exe: String,
    pub local: SocketAddr,
    pub remote: SocketAddr,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionMetadata {
    pub network: String,
    #[serde(rename = "sourceIP")]
    pub source_ip: std::net::IpAddr,
    pub source_port: String,
    pub inbound_port: String,
    pub host: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoreConnection {
    pub metadata: ConnectionMetadata,
    pub chains: Vec<String>,
}

#[cfg(any(windows, test))]
pub(crate) fn parse_connections(body: &str) -> Result<Vec<CoreConnection>, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "invalid connection snapshot")?;
    let connections = value
        .get_mut("connections")
        .ok_or("missing connection snapshot")?
        .take();
    if connections.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(connections).map_err(|_| "invalid connection metadata".into())
}

pub trait DesktopProbe {
    fn flows(&self) -> Result<Vec<DesktopFlow>, String>;
    /// Must read only the controller of a core whose process identity and ports are verified.
    fn connections(&self) -> Result<Vec<CoreConnection>, String>;
}

fn unknown(reason: &str) -> CheckResult {
    CheckResult::new("R11", Kind::ReadOnly, Status::Unknown, Some(reason))
}

/// The supported per-user Squirrel installation, not Claude Code or the short-lived launcher.
pub fn is_claude_desktop(exe: &str) -> bool {
    let normalized = exe.replace('/', "\\").to_ascii_lowercase();
    let parts: Vec<_> = normalized.rsplit('\\').take(3).collect();
    matches!(parts.as_slice(), ["claude.exe", version, "anthropicclaude"]
    if version.strip_prefix("app-").is_some_and(|v| {
        !v.is_empty() && v.split('.').all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    }))
}

fn claude_service(host: &str) -> bool {
    [
        "claude.ai",
        "anthropic.com",
        "claude.com",
        "claudeusercontent.com",
    ]
    .iter()
    .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// The API reports chains from the leaf outbound to the top-level group. Check the actual path,
/// including an explicit select member, rather than just finding a familiar group name in it.
fn chain_matches(routing: &Routing, target: &str, chain: &[String]) -> bool {
    let mut target = target;
    let mut rest = chain;
    loop {
        if rest.last().map(String::as_str) != Some(target) {
            return false;
        }
        rest = &rest[..rest.len() - 1];
        let Some(group) = routing.groups.iter().find(|g| g.name == target) else {
            return rest.is_empty()
                && (matches!(target, "relay" | "DIRECT" | "REJECT")
                    || routing.outbounds.iter().any(|o| o.name == target));
        };
        let Some(member) = rest.last() else {
            return false;
        };
        if !group.members.contains(member)
            || (group.kind == RoutingGroupKind::Select
                && group.selected.as_ref().is_some_and(|s| s != member))
        {
            return false;
        }
        target = member;
    }
}

pub fn observe(profile: &AgentProfile, mixed: u16, probe: &dyn DesktopProbe) -> CheckResult {
    let Ok(before) = probe.flows() else {
        return unknown("desktop_probe_failed");
    };
    let Ok(records) = probe.connections() else {
        return unknown("owned_core_unavailable");
    };
    let Ok(after) = probe.flows() else {
        return unknown("desktop_probe_failed");
    };
    let stable: Vec<_> = before
        .iter()
        .filter(|f| {
            is_claude_desktop(&f.exe)
                && f.created != 0
                && f.local.ip().is_loopback()
                && f.remote == SocketAddr::from(([127, 0, 0, 1], mixed))
                && after.contains(f)
        })
        .collect();
    let mut result = unknown("no_desktop_traffic");
    let mut observed = 0;
    let mut service = false;
    let mut unproven = false;
    let mut warning = None;
    for c in records {
        let m = &c.metadata;
        let Ok(port) = m.source_port.parse::<u16>() else {
            continue;
        };
        if m.network != "tcp" || m.inbound_port.parse::<u16>().ok() != Some(mixed) {
            continue;
        }
        let source = SocketAddr::new(m.source_ip, port);
        let Some(f) = stable.iter().find(|f| f.local == source) else {
            continue;
        };
        observed += 1;
        let host = m.host.trim_end_matches('.').to_ascii_lowercase();
        // An IP-only CONNECT or an unsupported host does not prove a domain rule match.
        if host.is_empty()
            || host.parse::<std::net::IpAddr>().is_ok()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        {
            unproven = true;
            continue;
        }
        service |= claude_service(&host);
        let expected = profile.routing.target_for_domain(&host);
        let leaf = c.chains.first().map(String::as_str);
        if claude_service(&host) && matches!(leaf, Some("DIRECT" | "REJECT")) {
            warning = Some("claude_target_not_proxied");
        } else if let Some(target) = expected {
            if !chain_matches(&profile.routing, target, &c.chains) {
                warning.get_or_insert("routing_mismatch");
            }
        } else {
            unproven = true;
        }
        result = result.with(
            "desktop.connection",
            crate::prompts::redact(
                &format!(
                    "pid={} source={} host={} expected={} actual={}",
                    f.pid,
                    f.local,
                    host,
                    expected.unwrap_or("unknown (Geo/IP rule needs more evidence)"),
                    c.chains.join(" -> "),
                ),
                false,
            ),
        );
    }
    let (status, reason) = if let Some(reason) = warning {
        (Status::Warn, Some(reason))
    } else if unproven {
        (Status::Unknown, Some("desktop_route_unproven"))
    } else if !service {
        (Status::Unknown, Some("no_desktop_traffic"))
    } else {
        (Status::Pass, None)
    };
    result.status = status;
    result.reason = reason.map(str::to_string);
    result.with("desktop.scope", format!(
        "{observed} stable active connection(s) attributed to Claude Desktop; only this sample is checked. \
         ISP identity/exit IP require G1/G2. No application content is read; idle/closed or bypass traffic is not proven."
    ))
}

#[cfg(windows)]
pub struct WindowsDesktopProbe<'a>(pub &'a crate::core::process::CoreManager);

#[cfg(windows)]
impl DesktopProbe for WindowsDesktopProbe<'_> {
    fn flows(&self) -> Result<Vec<DesktopFlow>, String> {
        crate::windows::desktop_proxy_flows(self.0.ports.mixed)
            .map_err(|_| "TCP probe failed".into())
    }

    fn connections(&self) -> Result<Vec<CoreConnection>, String> {
        let body = self
            .0
            .connections(&crate::windows::WindowsHost)
            .map_err(|_| "core unavailable")?;
        parse_connections(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ProfileSet, Routing, RoutingGroupKind, SUGGESTED_EXTRA_DOMAINS};
    use std::cell::Cell;

    const MIXED: u16 = 17893;
    const APP: &str = r"C:\Users\test\AppData\Local\AnthropicClaude\app-2.31226.1\claude.exe";

    fn profile() -> AgentProfile {
        let mut p = ProfileSet::default().agent.remove(0);
        p.routing = Routing::preset(
            "agent-egress",
            &p.targets,
            true,
            &SUGGESTED_EXTRA_DOMAINS.map(str::to_string),
            false,
            None,
        );
        p
    }

    fn flow(exe: &str, port: u16) -> DesktopFlow {
        DesktopFlow {
            pid: 10,
            created: 123,
            exe: exe.into(),
            local: format!("127.0.0.1:{port}").parse().unwrap(),
            remote: format!("127.0.0.1:{MIXED}").parse().unwrap(),
        }
    }

    fn connection(port: u16, host: &str, chains: &[&str]) -> CoreConnection {
        CoreConnection {
            metadata: ConnectionMetadata {
                network: "tcp".into(),
                source_ip: "127.0.0.1".parse().unwrap(),
                source_port: port.to_string(),
                inbound_port: MIXED.to_string(),
                host: host.into(),
            },
            chains: chains.iter().map(|s| s.to_string()).collect(),
        }
    }

    struct FakeProbe {
        before: Vec<DesktopFlow>,
        after: Vec<DesktopFlow>,
        records: Vec<CoreConnection>,
        calls: Cell<usize>,
        fail: Option<&'static str>,
    }

    impl DesktopProbe for FakeProbe {
        fn flows(&self) -> Result<Vec<DesktopFlow>, String> {
            let calls = self.calls.get();
            self.calls.set(calls + 1);
            if self.fail == Some("flows") {
                return Err("private credential password=secret".into());
            }
            Ok(if calls == 0 {
                &self.before
            } else {
                &self.after
            }
            .clone())
        }
        fn connections(&self) -> Result<Vec<CoreConnection>, String> {
            if self.fail == Some("core") {
                return Err("private credential password=secret".into());
            }
            Ok(self.records.clone())
        }
    }

    fn probe(flows: Vec<DesktopFlow>, records: Vec<CoreConnection>) -> FakeProbe {
        FakeProbe {
            after: flows.clone(),
            before: flows,
            records,
            calls: Cell::new(0),
            fail: None,
        }
    }

    #[test]
    fn desktop_api_uses_fixed_exit_and_other_traffic_follows_split_rules() {
        let f = probe(
            vec![flow(APP, 50000), flow(APP, 50001)],
            vec![
                connection(50000, "api.anthropic.com", &["relay", "agent-egress"]),
                connection(50001, "example.org", &["DIRECT"]),
            ],
        );
        let r = observe(&profile(), MIXED, &f);
        assert_eq!(r.status, Status::Pass, "{r:?}");
        assert_eq!(r.kind, Kind::ReadOnly);
        assert!(r
            .evidence
            .iter()
            .any(|e| e.detail.contains("api.anthropic.com")));
    }

    #[test]
    fn a_healthy_core_probe_or_claude_cli_is_not_desktop_evidence() {
        for exe in [
            r"C:\aec\aec-dev.exe",
            r"C:\tools\claude.exe",
            r"C:\Users\test\AppData\Local\AnthropicClaude\claude.exe",
        ] {
            let f = probe(
                vec![flow(exe, 50000)],
                vec![connection(50000, "claude.ai", &["relay", "agent-egress"])],
            );
            assert_eq!(observe(&profile(), MIXED, &f).status, Status::Unknown);
        }
        let f = probe(
            vec![flow(APP, 50000)],
            vec![connection(50001, "claude.ai", &["relay", "agent-egress"])],
        );
        assert_eq!(observe(&profile(), MIXED, &f).status, Status::Unknown);
    }

    #[test]
    fn closed_connections_reused_pids_and_other_proxy_ports_do_not_pass() {
        for change in ["closed", "pid", "created", "port", "exe"] {
            let mut f = probe(
                vec![flow(APP, 50000)],
                vec![connection(50000, "claude.ai", &["relay", "agent-egress"])],
            );
            match change {
                "closed" => f.after.clear(),
                "pid" => f.after[0].pid += 1,
                "created" => f.after[0].created += 1,
                "exe" => f.after[0].exe = r"C:\tools\claude.exe".into(),
                "port" => {
                    f.before[0].remote.set_port(7893);
                    f.after = f.before.clone();
                }
                _ => unreachable!(),
            }
            assert_eq!(
                observe(&profile(), MIXED, &f).status,
                Status::Unknown,
                "{change}"
            );
        }
    }

    #[test]
    fn direct_claude_and_wrong_route_warn_even_when_a_connection_exists() {
        for chains in [vec!["DIRECT"], vec!["other-exit"]] {
            let f = probe(
                vec![flow(APP, 50000)],
                vec![connection(50000, "claude.ai", &chains)],
            );
            assert_eq!(observe(&profile(), MIXED, &f).status, Status::Warn);
        }
        let mut p = profile();
        p.routing.rules.clear(); // Intentional custom DIRECT: preserve data, but explain the ISP gap.
        let f = probe(
            vec![flow(APP, 50000)],
            vec![connection(50000, "api.anthropic.com", &["DIRECT"])],
        );
        assert_eq!(
            observe(&p, MIXED, &f).reason.as_deref(),
            Some("claude_target_not_proxied")
        );
        assert!(p.routing.rules.is_empty());
    }

    #[test]
    fn a_long_connection_using_an_old_select_member_warns() {
        let mut p = profile();
        p.routing.groups[0].members.push("DIRECT".into());
        p.routing.groups[0].kind = RoutingGroupKind::Select;
        p.routing.groups[0].selected = Some("relay".into());
        let f = probe(
            vec![flow(APP, 50000)],
            vec![connection(50000, "claude.ai", &["DIRECT", "agent-egress"])],
        );
        assert_eq!(observe(&p, MIXED, &f).status, Status::Warn);
    }

    #[test]
    fn nested_group_paths_are_checked_in_order() {
        let mut p = profile();
        p.routing.groups[0].members = vec!["nested".into()];
        p.routing.groups.push(crate::profile::RoutingGroup {
            name: "nested".into(),
            kind: RoutingGroupKind::Select,
            members: vec!["relay".into()],
            selected: Some("relay".into()),
            switchable: false,
            check: None,
        });
        for (chain, expected) in [
            (vec!["relay", "nested", "agent-egress"], Status::Pass),
            (vec!["nested", "relay", "agent-egress"], Status::Warn),
            (vec!["relay", "unknown", "agent-egress"], Status::Warn),
        ] {
            let f = probe(
                vec![flow(APP, 50000)],
                vec![connection(50000, "claude.ai", &chain)],
            );
            assert_eq!(observe(&p, MIXED, &f).status, expected);
        }
    }

    #[test]
    fn idle_or_unresolvable_traffic_and_probe_errors_are_unknown() {
        for host in ["", "192.0.2.10", "example.org"] {
            let f = probe(
                vec![flow(APP, 50000)],
                vec![connection(50000, host, &["DIRECT"])],
            );
            assert_eq!(
                observe(&profile(), MIXED, &f).status,
                Status::Unknown,
                "{host}"
            );
        }
        for fail in ["flows", "core"] {
            let mut f = probe(vec![flow(APP, 50000)], vec![]);
            f.fail = Some(fail);
            let r = observe(&profile(), MIXED, &f);
            assert_eq!(r.status, Status::Unknown);
            assert!(!serde_json::to_string(&r).unwrap().contains("secret"));
        }
    }

    #[test]
    fn connection_api_shape_requires_string_ports_and_rejects_missing_metadata() {
        let wire = r#"{"connections":[{"metadata":{"network":"tcp","sourceIP":"127.0.0.1","sourcePort":"50000","inboundPort":"17893","host":"claude.ai"},"chains":["relay","agent-egress"]}]}"#;
        let records = parse_connections(wire).unwrap();
        let f = probe(vec![flow(APP, 50000)], records);
        assert_eq!(observe(&profile(), MIXED, &f).status, Status::Pass);
        assert!(parse_connections(r#"{"connections":null}"#)
            .unwrap()
            .is_empty());
        for wire in [
            "{}",
            r#"{"connections":[{}]}"#,
            r#"{"connections":[{"metadata":{"password":"secret"}}]}"#,
        ] {
            let error = parse_connections(wire).unwrap_err();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn another_inbound_source_ip_and_ambiguous_geo_rules_cannot_pass() {
        for change in ["inbound", "source", "udp", "geo"] {
            let mut p = profile();
            let mut f = probe(
                vec![flow(APP, 50000)],
                vec![connection(50000, "claude.ai", &["relay", "agent-egress"])],
            );
            match change {
                "inbound" => f.records[0].metadata.inbound_port = "7893".into(),
                "source" => f.records[0].metadata.source_ip = "127.0.0.2".parse().unwrap(),
                "udp" => f.records[0].metadata.network = "udp".into(),
                "geo" => p.routing.rules.insert(
                    0,
                    crate::profile::RoutingRule {
                        kind: crate::profile::RoutingRuleKind::Geosite,
                        value: "cn".into(),
                        target: "DIRECT".into(),
                        note: None,
                        no_resolve: false,
                    },
                ),
                _ => unreachable!(),
            }
            assert_eq!(observe(&p, MIXED, &f).status, Status::Unknown, "{change}");
        }
    }
}
