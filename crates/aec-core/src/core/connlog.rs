//! Connection records from the core's own log (log-level info). Evidence for G3: which host name
//! the client handed to the core, and which group the core routed it through.
//!
//! Shapes observed with mihomo v1.19.31 on 2026-10-11:
//!   [TCP] 127.0.0.1:53503 --> claude.ai:443 match Match using agent-egress[DIRECT]
//!   [TCP] dial agent-egress (match Match/) 127.0.0.1:52525 --> chatgpt.com:443 error: dial tcp …: i/o timeout

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnRecord {
    pub network: String,
    /// What the client asked for: a host name, or an address when the client resolved it itself.
    pub host: String,
    pub port: u16,
    pub rule: String,
    /// Top-level group (or proxy) the rule chose.
    pub group: String,
    /// The member the group selected, when the log shows one (`group[member]`).
    pub via: Option<String>,
    /// Set when the core could not dial the upstream.
    pub error: Option<String>,
}

impl ConnRecord {
    pub fn host_is_address(&self) -> bool {
        self.host
            .trim_matches(|c| c == '[' || c == ']')
            .parse::<std::net::IpAddr>()
            .is_ok()
    }
}

/// The `msg="…"` payload of a logfmt line, unescaped.
fn msg(line: &str) -> Option<String> {
    let start = line.find("msg=\"")? + 5;
    let mut out = String::new();
    let mut chars = line[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            c => out.push(c),
        }
    }
    None
}

fn split_target(t: &str) -> Option<(String, u16)> {
    let (h, p) = t.rsplit_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

fn split_chain(c: &str) -> (String, Option<String>) {
    match c.split_once('[') {
        Some((g, rest)) => (g.to_string(), Some(rest.trim_end_matches(']').to_string())),
        None => (c.to_string(), None),
    }
}

pub fn parse_line(line: &str) -> Option<ConnRecord> {
    let m = msg(line)?;
    let (network, rest) = m.strip_prefix('[')?.split_once("] ")?;
    if let Some(rest) = rest.strip_prefix("dial ") {
        // dial GROUP (match RULE/PAYLOAD) SRC --> HOST:PORT error: …
        let (group, rest) = rest.split_once(" (match ")?;
        let (rule, rest) = rest.split_once(") ")?;
        let (_, rest) = rest.split_once(" --> ")?;
        let (target, error) = rest.split_once(" error: ")?;
        let (host, port) = split_target(target)?;
        let (group, via) = split_chain(group);
        return Some(ConnRecord {
            network: network.into(),
            host,
            port,
            rule: rule.trim_end_matches('/').into(),
            group,
            via,
            error: Some(error.into()),
        });
    }
    // SRC --> HOST:PORT match RULE using CHAIN
    let (_, rest) = rest.split_once(" --> ")?;
    let (target, rest) = rest.split_once(" match ")?;
    let (rule, chain) = rest.split_once(" using ")?;
    let (host, port) = split_target(target)?;
    let (group, via) = split_chain(chain.trim());
    Some(ConnRecord {
        network: network.into(),
        host,
        port,
        rule: rule.into(),
        group,
        via,
        error: None,
    })
}

pub fn parse(text: &str) -> Vec<ConnRecord> {
    text.lines().filter_map(parse_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"time="2026-10-11T04:36:30.157638900-04:00" level=info msg="[TCP] 127.0.0.1:53503 --> claude.ai:443 match Match using agent-egress[DIRECT]""#;
    const DIAL: &str = r#"time="2026-10-11T04:36:36.461063000-04:00" level=warning msg="[TCP] dial agent-egress (match Match/) 127.0.0.1:52525 --> chatgpt.com:443 error: dial tcp 192.0.2.10:443: i/o timeout""#;

    #[test]
    fn parses_routed_and_failed_dials() {
        let a = parse_line(OK).unwrap();
        assert_eq!(
            (
                a.network.as_str(),
                a.host.as_str(),
                a.port,
                a.rule.as_str(),
                a.group.as_str(),
                a.via.as_deref(),
                a.error.is_none()
            ),
            (
                "TCP",
                "claude.ai",
                443,
                "Match",
                "agent-egress",
                Some("DIRECT"),
                true
            )
        );
        let b = parse_line(DIAL).unwrap();
        assert_eq!(
            (b.host.as_str(), b.group.as_str(), b.rule.as_str()),
            ("chatgpt.com", "agent-egress", "Match")
        );
        assert!(b.error.unwrap().contains("i/o timeout"));
    }

    #[test]
    fn handles_other_shapes() {
        let rule_payload = r#"level=info msg="[TCP] 127.0.0.1:1 --> www.anthropic.com:443 match DomainSuffix(anthropic.com) using fixed-exit[exit-a]""#;
        let r = parse_line(rule_payload).unwrap();
        assert_eq!(
            (r.rule.as_str(), r.group.as_str(), r.via.as_deref()),
            ("DomainSuffix(anthropic.com)", "fixed-exit", Some("exit-a"))
        );
        let ip =
            r#"level=info msg="[TCP] 127.0.0.1:2 --> 104.18.32.47:443 match Match using DIRECT""#;
        let r = parse_line(ip).unwrap();
        assert!(r.host_is_address());
        assert_eq!((r.group.as_str(), r.via), ("DIRECT", None));
        let v6 = r#"level=info msg="[TCP] [::1]:3 --> [2001:db8::1]:443 match Match using DIRECT""#;
        assert!(parse_line(v6).unwrap().host_is_address());
        assert_eq!(
            parse_line(r#"level=info msg="DNS server(UDP) listening at: 127.0.0.1:17553""#),
            None
        );
        assert_eq!(parse_line("garbage"), None);
        assert_eq!(parse(&format!("{OK}\n{DIAL}\nnoise\n")).len(), 2);
    }
}
