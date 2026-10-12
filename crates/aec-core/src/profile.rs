//! AgentProfile and DailyProfile, schema version 1 (SPEC 4.3).
//!
//! Unknown fields are rejected on load, so a profile can never carry a plaintext password: the only
//! way to refer to a secret is `credentialRef`, resolved from Windows Credential Manager later.

use std::collections::HashSet;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
/// mihomo gives up dialing after 5 s and this is not configurable (legacy measurement), so the
/// relay's per-connection budget must finish before that.
pub const CORE_DIAL_TIMEOUT_MS: u32 = 5000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub proto: UpstreamProto,
    #[serde(default)]
    pub credential_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamProto {
    Socks5,
    Http,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelaySettings {
    pub connect_timeout_ms: u32,
    pub dial_budget_ms: u32,
    pub cooldown_ms: u32,
    pub max_inflight: u32,
    pub max_rounds: u32,
}

impl Default for RelaySettings {
    /// The legacy chain's timings. Concurrency is higher than its original 3: Claude and ChatGPT
    /// clients open many connections at once, and with 3 slots they queued until they failed
    /// (2026-10-12, mostly api.anthropic.com). It limits connections being set up, not open ones.
    fn default() -> Self {
        Self {
            connect_timeout_ms: 1200,
            dial_budget_ms: 4000,
            cooldown_ms: 3000,
            max_inflight: 12,
            max_rounds: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedEgress {
    /// Exact IP (user decision 2026-10-10); ranges are not accepted.
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub country_code: Option<String>,
    #[serde(default)]
    pub asn: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Egress {
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub relay: RelaySettings,
    #[serde(default)]
    pub expected: ExpectedEgress,
    /// Optional encrypted first hop to the exit (ADR-0009).
    #[serde(default)]
    pub tunnel: Option<Tunnel>,
}

/// Shadowsocks ciphers offered for a tunnel. AEAD only; the stream ciphers are broken.
pub const TUNNEL_CIPHERS: [&str; 4] = [
    "chacha20-ietf-poly1305",
    "aes-128-gcm",
    "aes-256-gcm",
    "xchacha20-ietf-poly1305",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TunnelServer {
    pub host: String,
    pub port: u16,
}

/// A Shadowsocks tunnel the relay reaches the exit through (ADR-0009).
///
/// Some exits cannot be dialled reliably in plain text from where the user sits: the first hop is
/// recognised and reset. The core runs the tunnel (it already speaks Shadowsocks) and offers it on
/// a loopback port; the relay dials each candidate through that port. The exit's own credentials
/// therefore stay in the relay, and the core only ever holds the tunnel's password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tunnel {
    pub enabled: bool,
    /// The preset the servers came from (`tunnel::PRESETS`), or none when typed in by hand.
    #[serde(default)]
    pub preset: Option<String>,
    pub servers: Vec<TunnelServer>,
    pub cipher: String,
    /// The tunnel password, in Credential Manager like every other secret.
    pub credential_ref: String,
    /// After the tunnelled attempts, also dial the exit directly. Both paths leave from the same
    /// exit address, so the account sees no second address either way.
    #[serde(default = "default_true")]
    pub direct_fallback: bool,
    /// Per-step wait for a tunnelled attempt. The tunnel adds a round trip to every step of the
    /// handshake with the exit, so the relay's plain connect timeout is too short for it.
    #[serde(default = "default_tunnel_step_ms")]
    pub step_timeout_ms: u32,
}

fn default_tunnel_step_ms() -> u32 {
    2500
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyScopeMode {
    ExplicitOnly,
    SystemAndExplicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxyScope {
    pub mode: ProxyScopeMode,
    pub bypass: Vec<String>,
}

/// An explicitly named mihomo outbound. `relay`, `DIRECT` and `REJECT` are reserved names and do
/// not need entries here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingOutbound {
    pub name: String,
    pub kind: RoutingOutboundKind,
    pub server: String,
    pub port: u16,
    #[serde(default)]
    pub credential_ref: Option<String>,
    /// Encrypted transport nodes in imported tables (ADR-0009).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cipher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialer_proxy: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutingOutboundKind {
    Http,
    Socks5,
    #[serde(rename = "ss")]
    Shadowsocks,
}

/// Legacy settings-page compatibility type. Imported profiles use `RoutingOutbound` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FallbackUpstream {
    pub host: String,
    pub port: u16,
    pub proto: UpstreamProto,
    #[serde(default)]
    pub credential_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutingGroupKind {
    Select,
    Fallback,
    #[serde(rename = "url-test")]
    UrlTest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingCheck {
    pub url: String,
    pub interval_sec: u32,
    pub tolerance_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingGroup {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: RoutingGroupKind,
    #[serde(default)]
    pub check: Option<RoutingCheck>,
    pub members: Vec<String>,
    #[serde(default)]
    pub switchable: bool,
    #[serde(default)]
    pub selected: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingRuleKind {
    Domain,
    DomainSuffix,
    DomainKeyword,
    IpCidr,
    /// Optional geodata-based domestic bypass in the settings-page preset.
    Geosite,
    Geoip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingRule {
    pub kind: RoutingRuleKind,
    pub value: String,
    pub target: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub no_resolve: bool,
}

/// Routing is data, not a code-selected template. Presets below only create initial values in the
/// setup page; the running core consumes these four fields directly (ADR-0008).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Routing {
    #[serde(default)]
    pub outbounds: Vec<RoutingOutbound>,
    #[serde(default)]
    pub groups: Vec<RoutingGroup>,
    #[serde(default)]
    pub rules: Vec<RoutingRule>,
    #[serde(rename = "final")]
    pub final_target: String,
}

impl Default for Routing {
    fn default() -> Self {
        Self::preset("agent-egress", &[], false, &[], false, None)
    }
}

impl Routing {
    pub fn preset(
        group_name: &str,
        targets: &[Target],
        split: bool,
        extra: &[String],
        domestic: bool,
        local_proxy: Option<RoutingOutbound>,
    ) -> Self {
        let group = group_name.to_string();
        let mut rules: Vec<_> = [
            "127.0.0.0/8",
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "::1/128",
            "fc00::/7",
            "fe80::/10",
        ]
        .into_iter()
        .map(|value| RoutingRule {
            kind: RoutingRuleKind::IpCidr,
            value: value.into(),
            target: "DIRECT".into(),
            note: None,
            no_resolve: true,
        })
        .collect();
        {
            for domain in targets.iter().map(|t| &t.domain).chain(extra.iter()) {
                if !rules.iter().any(|r: &RoutingRule| r.value == *domain) {
                    rules.push(RoutingRule {
                        kind: RoutingRuleKind::DomainSuffix,
                        value: domain.clone(),
                        target: group.clone(),
                        note: None,
                        no_resolve: false,
                    });
                }
            }
        }
        if domestic {
            rules.extend([
                RoutingRule {
                    kind: RoutingRuleKind::Geosite,
                    value: "cn".into(),
                    target: "DIRECT".into(),
                    note: None,
                    no_resolve: false,
                },
                RoutingRule {
                    kind: RoutingRuleKind::Geoip,
                    value: "CN".into(),
                    target: "DIRECT".into(),
                    note: None,
                    no_resolve: true,
                },
            ]);
        }
        let mut groups = vec![RoutingGroup {
            name: group.clone(),
            kind: RoutingGroupKind::Select,
            check: None,
            members: vec!["relay".into()],
            switchable: false,
            selected: None,
        }];
        let mut outbounds = Vec::new();
        let final_target = if split {
            if let Some(proxy) = local_proxy {
                groups.push(RoutingGroup {
                    name: "aec-other".into(),
                    kind: RoutingGroupKind::Fallback,
                    check: Some(RoutingCheck {
                        url: "https://www.gstatic.com/generate_204".into(),
                        interval_sec: 15,
                        tolerance_ms: 0,
                    }),
                    members: vec![proxy.name.clone(), "DIRECT".into()],
                    switchable: false,
                    selected: None,
                });
                outbounds.push(proxy);
                "aec-other".to_string()
            } else {
                "DIRECT".to_string()
            }
        } else {
            group
        };
        Self {
            outbounds,
            groups,
            rules,
            final_target,
        }
    }

    pub fn primary_group(&self) -> Option<&RoutingGroup> {
        self.groups.first()
    }

    pub fn primary_group_name(&self) -> Option<&str> {
        self.primary_group().map(|g| g.name.as_str())
    }

    pub fn target_for_domain(&self, host: &str) -> Option<&str> {
        for rule in &self.rules {
            let hit = match rule.kind {
                RoutingRuleKind::Domain => host.eq_ignore_ascii_case(&rule.value),
                RoutingRuleKind::DomainSuffix => {
                    host.eq_ignore_ascii_case(&rule.value)
                        || host
                            .to_ascii_lowercase()
                            .ends_with(&format!(".{}", rule.value.to_ascii_lowercase()))
                }
                RoutingRuleKind::DomainKeyword => host
                    .to_ascii_lowercase()
                    .contains(&rule.value.to_ascii_lowercase()),
                RoutingRuleKind::Geosite | RoutingRuleKind::Geoip => return None,
                RoutingRuleKind::IpCidr if !rule.no_resolve => return None,
                RoutingRuleKind::IpCidr => false,
            };
            if hit {
                return Some(&rule.target);
            }
        }
        Some(&self.final_target)
    }

    pub fn uses_geodata(&self) -> bool {
        self.rules
            .iter()
            .any(|r| matches!(r.kind, RoutingRuleKind::Geosite | RoutingRuleKind::Geoip))
    }
}

/// `default-v1`: a preset that routes every target and the final catch-all to the fixed exit.
pub const TEMPLATE_DEFAULT: &str = "default-v1";
/// `split-v1`: a preset that routes checked targets to the fixed exit and the rest direct.
pub const TEMPLATE_SPLIT: &str = "split-v1";
pub const RULES_TEMPLATES: [&str; 2] = [TEMPLATE_DEFAULT, TEMPLATE_SPLIT];

/// Domains the settings page suggests for the fixed exit in split mode: the account and API
/// domains of common AI services. A suggestion the user edits, not a rule the code relies on.
pub const SUGGESTED_EXTRA_DOMAINS: [&str; 12] = [
    "anthropic.com",
    "claude.ai",
    "claude.com",
    "claudeusercontent.com",
    "openai.com",
    "chatgpt.com",
    "oaistatic.com",
    "oaiusercontent.com",
    "sora.com",
    "x.ai",
    "grok.com",
    "gemini.google.com",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimezoneTarget {
    pub windows_id: String,
    pub iana: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Locale {
    pub timezone: TimezoneTarget,
    pub browser_lang: String,
    pub accept_language: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub domain: String,
    #[serde(default = "default_true")]
    pub trace: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Browser {
    Edge,
    Chrome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Entry {
    #[serde(rename_all = "camelCase")]
    Browser {
        id: String,
        browser: Browser,
        #[serde(default = "default_true")]
        open_check_page: bool,
        #[serde(default)]
        extra_args: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    Cli {
        id: String,
        command: String,
        #[serde(default)]
        env: std::collections::BTreeMap<String, String>,
    },
}

impl Entry {
    pub fn id(&self) -> &str {
        match self {
            Entry::Browser { id, .. } | Entry::Cli { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionMode {
    Standard,
    LocalOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Detection {
    pub mode: DetectionMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentProfile {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub egress: Egress,
    pub proxy_scope: ProxyScope,
    pub routing: Routing,
    pub locale: Locale,
    pub targets: Vec<Target>,
    pub entries: Vec<Entry>,
    pub detection: Detection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreSource {
    Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestorePolicy {
    pub source: RestoreSource,
    pub close_launched_entries: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DailyFallback {
    #[serde(default)]
    pub timezone_windows_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DailyProfile {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub restore: RestorePolicy,
    #[serde(default)]
    pub fallback: DailyFallback,
}

/// The profile file: arrays so that more than one profile per kind needs no data migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileSet {
    pub schema_version: u32,
    pub agent: Vec<AgentProfile>,
    pub daily: Vec<DailyProfile>,
}

impl Default for ProfileSet {
    /// First-run profiles. The egress candidates are deliberately empty: they are a settings gap the
    /// user fills in, and validation reports them as such.
    fn default() -> Self {
        let targets: Vec<_> = ["claude.ai", "www.anthropic.com", "chatgpt.com"]
            .iter()
            .map(|d| Target {
                domain: (*d).into(),
                trace: true,
            })
            .collect();
        Self {
            schema_version: SCHEMA_VERSION,
            agent: vec![AgentProfile {
                schema_version: SCHEMA_VERSION,
                id: "agent-default".into(),
                name: "Agent".into(),
                egress: Egress {
                    candidates: Vec::new(),
                    relay: RelaySettings::default(),
                    expected: ExpectedEgress::default(),
                    tunnel: None,
                },
                proxy_scope: ProxyScope {
                    mode: ProxyScopeMode::ExplicitOnly,
                    bypass: vec!["<-loopback>".into(), "127.0.0.1".into(), "localhost".into()],
                },
                routing: Routing::preset("agent-egress", &targets, false, &[], false, None),
                locale: Locale {
                    timezone: TimezoneTarget {
                        windows_id: "Eastern Standard Time".into(),
                        iana: "America/New_York".into(),
                    },
                    browser_lang: "en-US".into(),
                    accept_language: "en-US,en".into(),
                },
                targets,
                entries: vec![
                    Entry::Browser {
                        id: "edge".into(),
                        browser: Browser::Edge,
                        open_check_page: true,
                        extra_args: Vec::new(),
                    },
                    Entry::Browser {
                        id: "chrome".into(),
                        browser: Browser::Chrome,
                        open_check_page: true,
                        extra_args: Vec::new(),
                    },
                    Entry::Cli {
                        id: "claude-cli".into(),
                        command: "claude".into(),
                        env: Default::default(),
                    },
                    Entry::Cli {
                        id: "codex-cli".into(),
                        command: "codex".into(),
                        env: Default::default(),
                    },
                ],
                detection: Detection {
                    mode: DetectionMode::Standard,
                },
            }],
            daily: vec![DailyProfile {
                schema_version: SCHEMA_VERSION,
                id: "daily-default".into(),
                name: "日常".into(),
                restore: RestorePolicy {
                    source: RestoreSource::Snapshot,
                    close_launched_entries: true,
                },
                fallback: DailyFallback {
                    timezone_windows_id: Some("China Standard Time".into()),
                },
            }],
        }
    }
}

/// One validation problem, addressed by a JSON-pointer-like path such as `agent[0].egress.candidates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    pub path: String,
    pub message: String,
}

struct Errors(Vec<FieldError>);

impl Errors {
    fn push(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.0.push(FieldError {
            path: path.into(),
            message: message.into(),
        });
    }
}

fn is_hostname(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.contains('.')
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        })
}

fn is_language_tag(s: &str) -> bool {
    !s.is_empty()
        && s.split('-')
            .all(|p| !p.is_empty() && p.len() <= 8 && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn check_unique<'a>(errors: &mut Errors, path: &str, ids: impl Iterator<Item = &'a str>) {
    let mut seen = HashSet::new();
    for id in ids {
        if id.trim().is_empty() {
            errors.push(path, "id must not be empty");
        } else if !seen.insert(id) {
            errors.push(path, format!("duplicate id '{id}'"));
        }
    }
}

/// Validate the data-driven routing table before the core can see it. All references share one
/// namespace, including built-in `relay`, `DIRECT`, and `REJECT`.
pub fn validate_routing(r: &Routing) -> Vec<FieldError> {
    let mut e = Errors(Vec::new());
    let mut names = HashSet::from(["relay", "DIRECT", "REJECT"]);
    for (i, outbound) in r.outbounds.iter().enumerate() {
        let p = format!("routing.outbounds[{i}]");
        let name = outbound.name.as_str();
        if name.trim().is_empty()
            || name.contains([',', '\n', '\r'])
            || name.starts_with("aec-tunnel")
            || !names.insert(name)
        {
            e.push(
                format!("{p}.name"),
                "name is empty, reserved, or already used",
            );
        }
        if outbound.server.trim().is_empty() || outbound.server.contains(char::is_whitespace) {
            e.push(format!("{p}.server"), "must be a host name or IP");
        }
        if outbound.port == 0 {
            e.push(format!("{p}.port"), "must be 1-65535");
        }
        match outbound.kind {
            RoutingOutboundKind::Shadowsocks => {
                if !outbound
                    .cipher
                    .as_deref()
                    .is_some_and(|c| TUNNEL_CIPHERS.contains(&c))
                {
                    e.push(format!("{p}.cipher"), "requires a supported AEAD cipher");
                }
                if outbound.credential_ref.is_none() {
                    e.push(
                        format!("{p}.credentialRef"),
                        "SS requires a stored password",
                    );
                }
            }
            _ if outbound.cipher.is_some() => {
                e.push(format!("{p}.cipher"), "only SS outbounds use a cipher")
            }
            _ => {}
        }
        if matches!(outbound.credential_ref.as_deref(), Some("")) {
            e.push(format!("{p}.credentialRef"), "must not be empty");
        }
    }
    for (i, group) in r.groups.iter().enumerate() {
        let p = format!("routing.groups[{i}]");
        let name = group.name.as_str();
        if name.trim().is_empty()
            || name.contains([',', '\n', '\r'])
            || name.starts_with("aec-tunnel")
            || !names.insert(name)
        {
            e.push(
                format!("{p}.name"),
                "name is empty, reserved, or already used",
            );
        }
        if group.switchable && group.kind != RoutingGroupKind::Select {
            e.push(
                format!("{p}.switchable"),
                "only select groups can be switched",
            );
        }
    }
    let exists = |n: &str| names.contains(n);
    for (i, outbound) in r.outbounds.iter().enumerate() {
        if outbound
            .dialer_proxy
            .as_deref()
            .is_some_and(|n| !exists(n) || n == outbound.name)
        {
            e.push(
                format!("routing.outbounds[{i}].dialerProxy"),
                "unknown or self-referencing dialer",
            );
        }
    }
    // The same cycle check covers nested groups and outbound dialer chains.
    fn cyclic<'a>(
        r: &'a Routing,
        name: &'a str,
        visiting: &mut HashSet<&'a str>,
        done: &mut HashSet<&'a str>,
    ) -> bool {
        if done.contains(name) {
            return false;
        }
        if !visiting.insert(name) {
            return true;
        }
        let children: Vec<&str> = if let Some(g) = r.groups.iter().find(|g| g.name == name) {
            g.members.iter().map(String::as_str).collect()
        } else {
            r.outbounds
                .iter()
                .find(|o| o.name == name)
                .and_then(|o| o.dialer_proxy.as_deref())
                .into_iter()
                .collect()
        };
        if children.into_iter().any(|n| cyclic(r, n, visiting, done)) {
            return true;
        }
        visiting.remove(name);
        done.insert(name);
        false
    }
    let mut done = HashSet::new();
    for name in r
        .groups
        .iter()
        .map(|g| g.name.as_str())
        .chain(r.outbounds.iter().map(|o| o.name.as_str()))
    {
        if cyclic(r, name, &mut HashSet::new(), &mut done) {
            e.push("routing", "group/dialer references form a cycle");
            break;
        }
    }
    if !exists(&r.final_target) {
        e.push(
            "routing.final",
            "must refer to an outbound, group, or reserved name",
        );
    }
    for (i, group) in r.groups.iter().enumerate() {
        let p = format!("routing.groups[{i}]");
        if group.members.is_empty() {
            e.push(format!("{p}.members"), "must not be empty");
        }
        for (j, member) in group.members.iter().enumerate() {
            if !exists(member) || member == &group.name {
                e.push(
                    format!("{p}.members[{j}]"),
                    "unknown or self-referencing member",
                );
            }
        }
        if let Some(selected) = &group.selected {
            if !group.members.contains(selected) || !group.switchable {
                e.push(
                    format!("{p}.selected"),
                    "must be a member of a switchable group",
                );
            }
        }
        if let Some(check) = &group.check {
            if !(check.url.starts_with("http://") || check.url.starts_with("https://"))
                || check.url.contains(char::is_whitespace)
            {
                e.push(format!("{p}.check.url"), "must be an HTTP(S) URL");
            }
            if check.interval_sec == 0 {
                e.push(format!("{p}.check.intervalSec"), "must be positive");
            }
        } else if group.kind != RoutingGroupKind::Select {
            e.push(
                format!("{p}.check"),
                "fallback and url-test groups require a check",
            );
        }
    }
    for (i, rule) in r.rules.iter().enumerate() {
        let p = format!("routing.rules[{i}]");
        if !exists(&rule.target) {
            e.push(format!("{p}.target"), "unknown outbound or group");
        }
        if rule.value.trim().is_empty() || rule.value.contains([',', '\n', '\r']) {
            e.push(
                format!("{p}.value"),
                "must be a non-empty single value without commas",
            );
        }
        if matches!(
            rule.kind,
            RoutingRuleKind::Domain | RoutingRuleKind::DomainSuffix
        ) && !is_hostname(&rule.value)
        {
            e.push(format!("{p}.value"), "must be a domain name");
        }
        if rule.kind == RoutingRuleKind::IpCidr {
            let valid = rule
                .value
                .split_once('/')
                .and_then(|(ip, bits)| Some((ip.parse::<IpAddr>().ok()?, bits.parse::<u8>().ok()?)))
                .is_some_and(|(ip, bits)| bits <= if ip.is_ipv4() { 32 } else { 128 });
            if !valid {
                e.push(format!("{p}.value"), "must be an IPv4/IPv6 CIDR");
            }
        }
        if rule.no_resolve && !matches!(rule.kind, RoutingRuleKind::IpCidr | RoutingRuleKind::Geoip)
        {
            e.push(format!("{p}.noResolve"), "only IP rules support no-resolve");
        }
    }
    e.0
}

fn validate_agent(p: &AgentProfile, at: &str, e: &mut Errors) {
    if p.schema_version != SCHEMA_VERSION {
        e.push(
            format!("{at}.schemaVersion"),
            format!("must be {SCHEMA_VERSION}"),
        );
    }
    if p.name.trim().is_empty() {
        e.push(format!("{at}.name"), "must not be empty");
    }
    for error in validate_routing(&p.routing) {
        e.push(format!("{at}.{}", error.path), error.message);
    }

    let c = format!("{at}.egress.candidates");
    if p.egress.candidates.is_empty() {
        e.push(&c, "at least one egress candidate is required");
    }
    check_unique(e, &c, p.egress.candidates.iter().map(|c| c.id.as_str()));
    for (i, cand) in p.egress.candidates.iter().enumerate() {
        if cand.host.trim().is_empty() {
            e.push(format!("{c}[{i}].host"), "must not be empty");
        }
        if cand.port == 0 {
            e.push(format!("{c}[{i}].port"), "must be 1-65535");
        }
        if matches!(cand.credential_ref.as_deref(), Some(r) if r.trim().is_empty()) {
            e.push(
                format!("{c}[{i}].credentialRef"),
                "must be omitted or non-empty",
            );
        }
    }

    let r = &p.egress.relay;
    let rp = format!("{at}.egress.relay");
    if r.dial_budget_ms >= CORE_DIAL_TIMEOUT_MS {
        e.push(
            format!("{rp}.dialBudgetMs"),
            format!("must be below the core's {CORE_DIAL_TIMEOUT_MS} ms dial timeout"),
        );
    }
    if r.connect_timeout_ms == 0 || r.connect_timeout_ms > r.dial_budget_ms {
        e.push(
            format!("{rp}.connectTimeoutMs"),
            "must be positive and not above dialBudgetMs",
        );
    }
    if r.max_inflight == 0 {
        e.push(format!("{rp}.maxInflight"), "must be at least 1");
    }
    if r.max_rounds == 0 {
        e.push(format!("{rp}.maxRounds"), "must be at least 1");
    }

    if let Some(t) = &p.egress.tunnel {
        let tp = format!("{at}.egress.tunnel");
        if t.enabled && t.servers.is_empty() {
            e.push(format!("{tp}.servers"), "at least one server is required");
        }
        for (i, s) in t.servers.iter().enumerate() {
            if s.host.trim().is_empty() || s.host.contains(char::is_whitespace) {
                e.push(
                    format!("{tp}.servers[{i}].host"),
                    "must be a host name or IP",
                );
            }
            if s.port == 0 {
                e.push(format!("{tp}.servers[{i}].port"), "must be 1-65535");
            }
        }
        if !TUNNEL_CIPHERS.contains(&t.cipher.as_str()) {
            e.push(
                format!("{tp}.cipher"),
                format!("must be one of {}", TUNNEL_CIPHERS.join(", ")),
            );
        }
        if t.credential_ref.trim().is_empty() {
            e.push(format!("{tp}.credentialRef"), "must not be empty");
        }
        if t.step_timeout_ms == 0 || t.step_timeout_ms > r.dial_budget_ms {
            e.push(
                format!("{tp}.stepTimeoutMs"),
                "must be positive and not above egress.relay.dialBudgetMs",
            );
        }
    }

    let x = &p.egress.expected;
    if let Some(ip) = &x.ip {
        if ip.parse::<IpAddr>().is_err() {
            e.push(
                format!("{at}.egress.expected.ip"),
                "must be one exact IP address",
            );
        }
    }
    if let Some(cc) = &x.country_code {
        if cc.len() != 2 || !cc.chars().all(|c| c.is_ascii_uppercase()) {
            e.push(
                format!("{at}.egress.expected.countryCode"),
                "must be two upper-case letters, e.g. US",
            );
        }
    }

    // The local check page and the core's controller live on loopback; they must never be proxied.
    if !p
        .proxy_scope
        .bypass
        .iter()
        .any(|b| b == "<-loopback>" || b == "127.0.0.1")
    {
        e.push(
            format!("{at}.proxyScope.bypass"),
            "must contain <-loopback> or 127.0.0.1",
        );
    }
    let l = &p.locale;
    if l.timezone.windows_id.trim().is_empty() {
        e.push(
            format!("{at}.locale.timezone.windowsId"),
            "must not be empty",
        );
    }
    if !l.timezone.iana.contains('/') {
        e.push(
            format!("{at}.locale.timezone.iana"),
            "must be an IANA name such as America/New_York",
        );
    }
    if !is_language_tag(&l.browser_lang) {
        e.push(
            format!("{at}.locale.browserLang"),
            "must be a language tag such as en-US",
        );
    }
    if l.accept_language
        .split(',')
        .map(str::trim)
        .any(|t| !is_language_tag(t.split(';').next().unwrap_or("")))
    {
        e.push(
            format!("{at}.locale.acceptLanguage"),
            "must be a comma-separated list of language tags",
        );
    }

    if p.targets.is_empty() {
        e.push(
            format!("{at}.targets"),
            "at least one target domain is required",
        );
    }
    for (i, t) in p.targets.iter().enumerate() {
        if !is_hostname(&t.domain) {
            e.push(
                format!("{at}.targets[{i}].domain"),
                "must be a bare host name (no scheme, port or path)",
            );
        }
    }

    check_unique(e, &format!("{at}.entries"), p.entries.iter().map(Entry::id));
    for (i, entry) in p.entries.iter().enumerate() {
        if let Entry::Cli { command, .. } = entry {
            if command.trim().is_empty() {
                e.push(format!("{at}.entries[{i}].command"), "must not be empty");
            }
        }
    }
}

/// Validate one AgentProfile on its own (paths start at `agent`).
pub fn validate_agent_profile(p: &AgentProfile) -> Vec<FieldError> {
    let mut e = Errors(Vec::new());
    validate_agent(p, "agent", &mut e);
    e.0
}

/// Validate a profile set. An empty result means valid.
pub fn validate(set: &ProfileSet) -> Vec<FieldError> {
    let mut e = Errors(Vec::new());
    if set.schema_version != SCHEMA_VERSION {
        e.push("schemaVersion", format!("must be {SCHEMA_VERSION}"));
    }
    if set.agent.is_empty() {
        e.push("agent", "at least one AgentProfile is required");
    }
    if set.daily.is_empty() {
        e.push("daily", "at least one DailyProfile is required");
    }
    check_unique(&mut e, "agent", set.agent.iter().map(|p| p.id.as_str()));
    check_unique(&mut e, "daily", set.daily.iter().map(|p| p.id.as_str()));
    for (i, p) in set.agent.iter().enumerate() {
        validate_agent(p, &format!("agent[{i}]"), &mut e);
    }
    for (i, d) in set.daily.iter().enumerate() {
        if d.schema_version != SCHEMA_VERSION {
            e.push(
                format!("daily[{i}].schemaVersion"),
                format!("must be {SCHEMA_VERSION}"),
            );
        }
        if matches!(d.fallback.timezone_windows_id.as_deref(), Some(t) if t.trim().is_empty()) {
            e.push(
                format!("daily[{i}].fallback.timezoneWindowsId"),
                "must be omitted or non-empty",
            );
        }
    }
    e.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> ProfileSet {
        let mut s = ProfileSet::default();
        s.agent[0].egress.candidates.push(Candidate {
            id: "c1".into(),
            host: "198.51.100.7".into(),
            port: 12324,
            proto: UpstreamProto::Socks5,
            credential_ref: Some("c1".into()),
        });
        s
    }

    fn paths(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|e| e.path.as_str()).collect()
    }

    fn fallback() -> RoutingOutbound {
        RoutingOutbound {
            name: "daily-proxy".into(),
            server: "203.0.113.10".into(),
            port: 1080,
            kind: RoutingOutboundKind::Socks5,
            credential_ref: None,
            cipher: None,
            dialer_proxy: None,
        }
    }

    #[test]
    fn default_reports_only_the_missing_candidate() {
        assert_eq!(
            paths(&validate(&ProfileSet::default())),
            vec!["agent[0].egress.candidates"]
        );
        assert!(validate(&valid()).is_empty());
    }

    /// The shipped default must not describe the maintainer's machine. A port that happens to be
    /// listening on one developer's box is a settings gap nobody else can diagnose, so the default
    /// pre-fills nothing: no egress candidate, no second upstream.
    #[test]
    fn default_profile_carries_nothing_machine_specific() {
        let s = ProfileSet::default();
        assert_eq!(s.agent[0].routing.final_target, "agent-egress");
        assert!(s.agent[0].routing.outbounds.is_empty());
        assert!(s.agent[0].egress.candidates.is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let s = valid();
        let json = serde_json::to_string_pretty(&s).unwrap();
        assert!(json.contains("\"credentialRef\": \"c1\""), "{json}");
        assert!(json.contains("\"type\": \"browser\""));
        let back: ProfileSet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn plaintext_password_field_is_rejected() {
        let mut v = serde_json::to_value(valid()).unwrap();
        v["agent"][0]["egress"]["candidates"][0]["password"] = "hunter2".into();
        let err = serde_json::from_value::<ProfileSet>(v)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown field `password`"), "{err}");
    }

    type Mutation = Box<dyn Fn(&mut ProfileSet)>;

    #[test]
    fn field_errors_are_addressed() {
        let cases: Vec<(&str, Mutation)> = vec![
            (
                "agent[0].egress.candidates[0].port",
                Box::new(|s| s.agent[0].egress.candidates[0].port = 0),
            ),
            (
                "agent[0].egress.candidates[0].host",
                Box::new(|s| s.agent[0].egress.candidates[0].host = " ".into()),
            ),
            (
                "agent[0].egress.candidates",
                Box::new(|s| {
                    let c = s.agent[0].egress.candidates[0].clone();
                    s.agent[0].egress.candidates.push(c)
                }),
            ),
            (
                "agent[0].egress.relay.dialBudgetMs",
                Box::new(|s| s.agent[0].egress.relay.dial_budget_ms = 5000),
            ),
            (
                "agent[0].egress.relay.connectTimeoutMs",
                Box::new(|s| s.agent[0].egress.relay.connect_timeout_ms = 0),
            ),
            (
                "agent[0].egress.relay.maxInflight",
                Box::new(|s| s.agent[0].egress.relay.max_inflight = 0),
            ),
            (
                "agent[0].egress.expected.ip",
                Box::new(|s| s.agent[0].egress.expected.ip = Some("198.51.100.0/24".into())),
            ),
            (
                "agent[0].egress.expected.countryCode",
                Box::new(|s| s.agent[0].egress.expected.country_code = Some("usa".into())),
            ),
            (
                "agent[0].proxyScope.bypass",
                Box::new(|s| s.agent[0].proxy_scope.bypass = vec!["localhost".into()]),
            ),
            (
                "agent[0].locale.timezone.iana",
                Box::new(|s| s.agent[0].locale.timezone.iana = "EST".into()),
            ),
            (
                "agent[0].locale.browserLang",
                Box::new(|s| s.agent[0].locale.browser_lang = "en_US".into()),
            ),
            (
                "agent[0].locale.acceptLanguage",
                Box::new(|s| s.agent[0].locale.accept_language = "en-US,,en".into()),
            ),
            (
                "agent[0].routing.final",
                Box::new(|s| s.agent[0].routing.final_target = "missing".into()),
            ),
            (
                "agent[0].routing.rules[0].target",
                Box::new(|s| {
                    s.agent[0].routing.rules[0].target = "missing".into();
                }),
            ),
            (
                "agent[0].routing.outbounds[0].server",
                Box::new(|s| {
                    let mut outbound = fallback();
                    outbound.server = " ".into();
                    s.agent[0].routing.outbounds.push(outbound);
                }),
            ),
            (
                "agent[0].routing.outbounds[0].port",
                Box::new(|s| {
                    let mut outbound = fallback();
                    outbound.port = 0;
                    s.agent[0].routing.outbounds.push(outbound);
                }),
            ),
            (
                "agent[0].targets[0].domain",
                Box::new(|s| s.agent[0].targets[0].domain = "https://claude.ai/".into()),
            ),
            ("agent[0].targets", Box::new(|s| s.agent[0].targets.clear())),
            (
                "agent[0].entries",
                Box::new(|s| {
                    s.agent[0].entries.push(Entry::Cli {
                        id: "edge".into(),
                        command: "x".into(),
                        env: Default::default(),
                    })
                }),
            ),
            ("agent", Box::new(|s| s.agent.clear())),
            ("daily", Box::new(|s| s.daily.clear())),
        ];
        for (expected, mutate) in cases {
            let mut s = valid();
            mutate(&mut s);
            let errs = validate(&s);
            assert!(
                paths(&errs).contains(&expected),
                "expected {expected}, got {errs:?}"
            );
        }
    }

    #[test]
    fn split_mode_does_not_need_a_proxy_of_its_own() {
        let mut s = valid();
        for split in [false, true] {
            s.agent[0].routing =
                Routing::preset("agent-egress", &s.agent[0].targets, split, &[], true, None);
            assert!(validate(&s).is_empty(), "split={split}");
        }
    }

    #[test]
    fn suggested_extra_domains_are_valid() {
        let mut s = valid();
        let extra: Vec<_> = SUGGESTED_EXTRA_DOMAINS
            .iter()
            .map(|d| d.to_string())
            .collect();
        s.agent[0].routing = Routing::preset(
            "agent-egress",
            &s.agent[0].targets,
            true,
            &extra,
            false,
            None,
        );
        assert!(validate(&s).is_empty());
    }

    #[test]
    fn accept_language_with_quality_values_is_valid() {
        let mut s = valid();
        s.agent[0].locale.accept_language = "en-US,en;q=0.9".into();
        assert!(validate(&s).is_empty());
    }
}
