//! First-run and later "exit and time zone" settings: what a user fills in, how it maps onto the
//! profile, and a loopback-only page to fill it in (the tray opens it in the default browser).
//!
//! The password goes from the page straight into Credential Manager; it is never written to the
//! profile, the log or the page that is sent back.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::profile::{
    self, Candidate, FieldError, ProfileSet, ProxyScopeMode, Routing, RoutingOutbound,
    RoutingOutboundKind, RoutingRuleKind, Target, Tunnel, TunnelServer, UpstreamProto,
    TUNNEL_CIPHERS,
};

/// The one credential the settings page manages.
pub const CREDENTIAL_REF: &str = "exit";
const MAX_BODY: usize = 128 * 1024;
const MAX_PORTS: usize = 20;

/// (key, label, Windows id, IANA name). The IANA name is what Chromium reports for the Windows id,
/// so G4's browser comparison holds.
pub const TIMEZONES: [(&str, &str, &str, &str); 13] = [
    (
        "us-east",
        "美国东部（纽约）",
        "Eastern Standard Time",
        "America/New_York",
    ),
    (
        "us-central",
        "美国中部（芝加哥）",
        "Central Standard Time",
        "America/Chicago",
    ),
    (
        "us-mountain",
        "美国山地（丹佛）",
        "Mountain Standard Time",
        "America/Denver",
    ),
    (
        "us-arizona",
        "美国亚利桑那（凤凰城）",
        "US Mountain Standard Time",
        "America/Phoenix",
    ),
    (
        "us-west",
        "美国西部（洛杉矶）",
        "Pacific Standard Time",
        "America/Los_Angeles",
    ),
    ("uk", "英国（伦敦）", "GMT Standard Time", "Europe/London"),
    (
        "de",
        "德国（柏林）",
        "W. Europe Standard Time",
        "Europe/Berlin",
    ),
    (
        "fr",
        "法国（巴黎）",
        "Romance Standard Time",
        "Europe/Paris",
    ),
    ("jp", "日本（东京）", "Tokyo Standard Time", "Asia/Tokyo"),
    ("kr", "韩国（首尔）", "Korea Standard Time", "Asia/Seoul"),
    ("sg", "新加坡", "Singapore Standard Time", "Asia/Singapore"),
    ("tw", "台湾（台北）", "Taipei Standard Time", "Asia/Taipei"),
    (
        "au-east",
        "澳大利亚东部（悉尼）",
        "AUS Eastern Standard Time",
        "Australia/Sydney",
    ),
];

/// (tag, label) for the Agent browsers' language.
pub const LANGUAGES: [(&str, &str); 7] = [
    ("en-US", "英语（美国）"),
    ("en-GB", "英语（英国）"),
    ("ja-JP", "日语"),
    ("ko-KR", "韩语"),
    ("de-DE", "德语"),
    ("fr-FR", "法语"),
    ("zh-TW", "繁体中文"),
];

/// The tunnel password, in Credential Manager next to the exit's.
pub const TUNNEL_CREDENTIAL_REF: &str = "tunnel";
/// Value of `tunnelPreset` for a tunnel typed in by hand.
pub const CUSTOM_TUNNEL: &str = "custom";
/// `routingMode` values: everything through the exit, or split.
pub const ROUTING_ALL: &str = "all";
pub const ROUTING_SPLIT: &str = "split";
pub const ROUTING_CUSTOM: &str = "custom";

/// What the page shows. No password, only whether one is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupView {
    pub core_mixed_port: String,
    pub host: String,
    pub proto: UpstreamProto,
    /// Comma-separated; a port whose protocol differs from `proto` carries it, e.g. `12323/http`.
    pub ports: String,
    pub username: String,
    pub password_stored: bool,
    pub timezone: String,
    pub language: String,
    pub expected_ip: String,
    pub takeover_enabled: bool,
    pub tunnel_enabled: bool,
    pub tunnel_preset: String,
    /// One `host:port` per line (custom tunnels).
    pub tunnel_servers: String,
    pub tunnel_cipher: String,
    pub tunnel_password_stored: bool,
    pub tunnel_direct_fallback: bool,
    pub routing_mode: String,
    /// `host:port` of the user's own proxy, empty for none.
    pub local_proxy: String,
    pub local_proxy_proto: UpstreamProto,
    pub domestic_direct: bool,
    /// One domain per line.
    pub targets: String,
    pub extra_domains: String,
    pub system_proxy: bool,
    pub max_inflight: u32,
    pub connect_timeout_ms: u32,
    pub dial_budget_ms: u32,
    /// Agent mode is active: saving is refused until the user leaves it.
    pub locked: bool,
}

fn default_mode() -> String {
    ROUTING_ALL.into()
}
fn default_preset() -> String {
    CUSTOM_TUNNEL.into()
}
fn default_true() -> bool {
    true
}
fn default_socks5() -> UpstreamProto {
    UpstreamProto::Socks5
}

/// What the page sends. `password: None` (or empty) keeps the stored one. Fields added after 0.1
/// default to what 0.1 did, so an older page still saves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitForm {
    /// None (older page) preserves the value; empty or "0" resets to the default.
    #[serde(default)]
    pub core_mixed_port: Option<String>,
    pub host: String,
    pub proto: UpstreamProto,
    pub ports: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    pub timezone: String,
    pub language: String,
    #[serde(default)]
    pub expected_ip: String,
    #[serde(default)]
    pub takeover_enabled: bool,
    #[serde(default)]
    pub tunnel_enabled: bool,
    #[serde(default = "default_preset")]
    pub tunnel_preset: String,
    #[serde(default)]
    pub tunnel_servers: String,
    #[serde(default)]
    pub tunnel_cipher: String,
    #[serde(default)]
    pub tunnel_password: Option<String>,
    #[serde(default = "default_true")]
    pub tunnel_direct_fallback: bool,
    #[serde(default = "default_mode")]
    pub routing_mode: String,
    #[serde(default)]
    pub local_proxy: String,
    #[serde(default = "default_socks5")]
    pub local_proxy_proto: UpstreamProto,
    #[serde(default)]
    pub domestic_direct: bool,
    /// Empty keeps the current targets.
    #[serde(default)]
    pub targets: String,
    #[serde(default)]
    pub extra_domains: String,
    #[serde(default)]
    pub system_proxy: bool,
    /// Zero keeps the current value.
    #[serde(default)]
    pub max_inflight: u32,
    #[serde(default)]
    pub connect_timeout_ms: u32,
    #[serde(default)]
    pub dial_budget_ms: u32,
}

/// What [`apply`] asks the caller to do after the profile changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub exit: CredentialChange,
    pub tunnel: CredentialChange,
    /// The time zone switch (settings, not profile).
    pub takeover: bool,
}

impl std::fmt::Debug for CredentialChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialChange::Store { user, .. } => {
                write!(f, "Store {{ user: {user:?}, pass: *** }}")
            }
            CredentialChange::Keep => f.write_str("Keep"),
            CredentialChange::Delete => f.write_str("Delete"),
        }
    }
}

/// What to do in Credential Manager after the profile is updated.
#[derive(Clone, PartialEq, Eq)]
pub enum CredentialChange {
    Store { user: String, pass: String },
    Keep,
    Delete,
}

fn err(path: &str, message: &str) -> FieldError {
    FieldError {
        path: path.into(),
        message: message.into(),
    }
}

fn proto_name(p: UpstreamProto) -> &'static str {
    match p {
        UpstreamProto::Socks5 => "socks5",
        UpstreamProto::Http => "http",
    }
}

/// What the settings page shows for this machine. `stored_user`: the exit's user name in
/// Credential Manager; `tunnel_password_stored`: whether a tunnel password is stored.
pub struct ViewInput<'a> {
    pub core_mixed_port: Option<u16>,
    pub stored_user: Option<&'a str>,
    pub tunnel_password_stored: bool,
    pub takeover_enabled: bool,
    pub locked: bool,
}

/// The settings page's view of a profile set.
pub fn view(set: &ProfileSet, input: &ViewInput) -> SetupView {
    let p = &set.agent[0];
    let c = &p.egress.candidates;
    let tz = TIMEZONES
        .iter()
        .find(|t| t.2 == p.locale.timezone.windows_id && t.3 == p.locale.timezone.iana)
        .map_or("us-east", |t| t.0);
    let proto = c.first().map_or(UpstreamProto::Socks5, |c| c.proto);
    let t = p.egress.tunnel.as_ref();
    let r = &p.routing;
    let local = r.outbounds.iter().find(|o| o.name == "daily-proxy");
    let extra: Vec<String> = r
        .rules
        .iter()
        .filter(|rule| {
            rule.kind == RoutingRuleKind::DomainSuffix
                && rule.target == "agent-egress"
                && !p.targets.iter().any(|t| t.domain == rule.value)
        })
        .map(|rule| rule.value.clone())
        .collect();
    let split = r.final_target != "agent-egress";
    let generated = Routing::preset(
        "agent-egress",
        &p.targets,
        split,
        &extra,
        r.uses_geodata(),
        local.cloned(),
    );
    let is_preset = serde_json::to_value(&generated).ok() == serde_json::to_value(r).ok();
    let lines = |v: Vec<String>| v.join("\n");
    SetupView {
        core_mixed_port: input
            .core_mixed_port
            .filter(|p| *p != 0)
            .map(|p| p.to_string())
            .unwrap_or_default(),
        host: c.first().map(|c| c.host.clone()).unwrap_or_default(),
        proto,
        ports: c
            .iter()
            .map(|c| {
                if c.proto == proto {
                    c.port.to_string()
                } else {
                    format!("{}/{}", c.port, proto_name(c.proto))
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
        username: input.stored_user.unwrap_or_default().to_string(),
        password_stored: input.stored_user.is_some(),
        timezone: tz.into(),
        language: p.locale.browser_lang.clone(),
        expected_ip: p.egress.expected.ip.clone().unwrap_or_default(),
        takeover_enabled: input.takeover_enabled,
        tunnel_enabled: t.is_some_and(|t| t.enabled),
        tunnel_preset: t
            .and_then(|t| t.preset.clone())
            .unwrap_or_else(|| CUSTOM_TUNNEL.into()),
        tunnel_servers: t.map_or_else(String::new, |t| {
            lines(
                t.servers
                    .iter()
                    .map(|s| format!("{}:{}", s.host, s.port))
                    .collect(),
            )
        }),
        tunnel_cipher: t.map_or_else(|| TUNNEL_CIPHERS[0].to_string(), |t| t.cipher.clone()),
        tunnel_password_stored: input.tunnel_password_stored,
        tunnel_direct_fallback: t.is_none_or(|t| t.direct_fallback),
        routing_mode: if !is_preset {
            ROUTING_CUSTOM.into()
        } else if split {
            ROUTING_SPLIT.into()
        } else {
            ROUTING_ALL.into()
        },
        local_proxy: local
            .map(|f| format!("{}:{}", f.server, f.port))
            .unwrap_or_default(),
        local_proxy_proto: local.map_or(UpstreamProto::Socks5, |f| {
            if f.kind == RoutingOutboundKind::Http {
                UpstreamProto::Http
            } else {
                UpstreamProto::Socks5
            }
        }),
        domestic_direct: r.uses_geodata(),
        targets: lines(p.targets.iter().map(|t| t.domain.clone()).collect()),
        extra_domains: lines(extra),
        system_proxy: p.proxy_scope.mode == ProxyScopeMode::SystemAndExplicit,
        max_inflight: p.egress.relay.max_inflight,
        connect_timeout_ms: p.egress.relay.connect_timeout_ms,
        dial_budget_ms: p.egress.relay.dial_budget_ms,
        locked: input.locked,
    }
}

/// `12324, 12323/http`: each port with its own protocol when given, else `default`.
fn parse_ports(s: &str, default: UpstreamProto) -> Result<Vec<(u16, UpstreamProto)>, FieldError> {
    let mut out: Vec<(u16, UpstreamProto)> = Vec::new();
    for part in s
        .split([',', ' ', '，', ';', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let (num, proto) = match part.split_once('/') {
            None => (part, default),
            Some((n, pr)) => match pr.trim().to_ascii_lowercase().as_str() {
                "http" => (n.trim(), UpstreamProto::Http),
                "socks5" | "socks" => (n.trim(), UpstreamProto::Socks5),
                _ => {
                    return Err(err(
                        "ports",
                        &format!("「{part}」的协议只能是 http 或 socks5"),
                    ))
                }
            },
        };
        let port: u16 = num
            .parse()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| err("ports", &format!("「{part}」不是 1–65535 的端口号")))?;
        if !out.contains(&(port, proto)) {
            out.push((port, proto));
        }
    }
    if out.is_empty() {
        return Err(err("ports", "至少填一个端口"));
    }
    if out.len() > MAX_PORTS {
        return Err(err("ports", &format!("最多 {MAX_PORTS} 个端口")));
    }
    Ok(out)
}

/// `host:port`, IPv6 in brackets. Empty means none.
fn parse_host_port(s: &str, field: &str) -> Result<Option<(String, u16)>, FieldError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let bad = || {
        err(
            field,
            &format!("「{s}」要写成 地址:端口，例如 127.0.0.1:7890"),
        )
    };
    if s.contains("://") || s.contains('/') {
        return Err(bad());
    }
    let (host, port) = s.rsplit_once(':').ok_or_else(bad)?;
    let host = host.trim_start_matches('[').trim_end_matches(']').trim();
    let port: u16 = port
        .trim()
        .parse()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(bad)?;
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(bad());
    }
    Ok(Some((host.to_string(), port)))
}

/// One domain per line (commas and spaces also separate); duplicates dropped, order kept.
fn parse_domains(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in s
        .split(['\n', ',', ' ', '，', ';'])
        .map(|d| d.trim().trim_start_matches("*.").to_ascii_lowercase())
        .filter(|d| !d.is_empty())
    {
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

/// Apply a form to the first AgentProfile. On success the set is changed and the credential changes
/// are returned for the caller to carry out; on failure the set is left as it was.
/// `tunnel_password_stored`: a tunnel password is in Credential Manager already.
pub fn apply(
    set: &mut ProfileSet,
    form: &ExitForm,
    stored_user: Option<&str>,
    tunnel_password_stored: bool,
) -> Result<Applied, Vec<FieldError>> {
    let mut errors = Vec::new();
    let host = form.host.trim();
    if host.is_empty() {
        errors.push(err("host", "必须填写出口地址"));
    } else if host.contains("://") || host.contains('/') || host.contains(char::is_whitespace) {
        errors.push(err("host", "只填主机名或 IP，不要带 http:// 或路径"));
    } else if host.contains(':') && host.parse::<std::net::Ipv6Addr>().is_err() {
        errors.push(err("host", "端口填在下面的「端口」里，这里不要带冒号"));
    }
    let ports = parse_ports(&form.ports, form.proto)
        .map_err(|e| errors.push(e))
        .ok();
    let tz = TIMEZONES.iter().find(|t| t.0 == form.timezone);
    if tz.is_none() {
        errors.push(err("timezone", "请选择一个时区"));
    }
    if !LANGUAGES.iter().any(|l| l.0 == form.language) {
        errors.push(err("language", "请选择一种语言"));
    }
    let user = form.username.trim();
    let pass = form.password.as_deref().filter(|p| !p.is_empty());
    let change = match (user.is_empty(), pass) {
        (true, Some(_)) => {
            errors.push(err("username", "填了密码就要填用户名"));
            CredentialChange::Keep
        }
        (true, None) => CredentialChange::Delete,
        (false, Some(p)) => CredentialChange::Store {
            user: user.into(),
            pass: p.into(),
        },
        (false, None) if stored_user == Some(user) => CredentialChange::Keep,
        (false, None) => {
            errors.push(err("password", "新填或改了用户名时需要输入密码"));
            CredentialChange::Keep
        }
    };
    // Tunnel: a preset carries its own servers, cipher and published password; a custom tunnel
    // needs all three from the form.
    let mut tunnel_change = CredentialChange::Keep;
    let previous_tunnel = set.agent[0].egress.tunnel.clone();
    let tunnel = if !form.tunnel_enabled {
        previous_tunnel.clone().map(|mut t| {
            t.enabled = false;
            t
        })
    } else if form.tunnel_preset == CUSTOM_TUNNEL {
        let mut servers = Vec::new();
        for line in form
            .tunnel_servers
            .split(['\n', ',', '，', ';'])
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            match parse_host_port(line, "tunnelServers") {
                Ok(Some((host, port))) => servers.push(TunnelServer { host, port }),
                Ok(None) => {}
                Err(e) => errors.push(e),
            }
        }
        if servers.is_empty() {
            errors.push(err("tunnelServers", "至少填一个隧道服务器（地址:端口）"));
        }
        if !TUNNEL_CIPHERS.contains(&form.tunnel_cipher.as_str()) {
            errors.push(err("tunnelCipher", "请选择一种加密方式"));
        }
        let was_custom = previous_tunnel.as_ref().is_some_and(|t| t.preset.is_none());
        match form.tunnel_password.as_deref().filter(|p| !p.is_empty()) {
            Some(p) => {
                tunnel_change = CredentialChange::Store {
                    user: "shadowsocks".into(),
                    pass: p.into(),
                }
            }
            // A stored password belongs to the tunnel it was entered for; switching from a preset
            // to a custom tunnel needs the custom one's password.
            None if tunnel_password_stored && was_custom => {}
            None => errors.push(err("tunnelPassword", "填隧道的密码")),
        }
        Some(Tunnel {
            enabled: true,
            preset: None,
            servers,
            cipher: form.tunnel_cipher.clone(),
            credential_ref: TUNNEL_CREDENTIAL_REF.into(),
            direct_fallback: form.tunnel_direct_fallback,
            step_timeout_ms: previous_tunnel.as_ref().map_or(2500, |t| t.step_timeout_ms),
        })
    } else {
        match crate::tunnel::preset(&form.tunnel_preset) {
            Some(pre) => {
                if user.is_empty() {
                    errors.push(err(
                        "username",
                        "经隧道连接时出口看不到你的 IP 白名单，必须填出口的用户名和密码",
                    ));
                }
                tunnel_change = CredentialChange::Store {
                    user: "shadowsocks".into(),
                    pass: pre.password.into(),
                };
                Some(Tunnel {
                    enabled: true,
                    preset: Some(pre.key.into()),
                    servers: pre.servers(),
                    cipher: pre.cipher.into(),
                    credential_ref: TUNNEL_CREDENTIAL_REF.into(),
                    direct_fallback: form.tunnel_direct_fallback,
                    step_timeout_ms: previous_tunnel.as_ref().map_or(2500, |t| t.step_timeout_ms),
                })
            }
            None => {
                errors.push(err("tunnelPreset", "请选择一个隧道渠道"));
                None
            }
        }
    };

    let split = match form.routing_mode.as_str() {
        ROUTING_ALL => false,
        ROUTING_SPLIT => true,
        ROUTING_CUSTOM => false,
        _ => {
            errors.push(err("routingMode", "请选择分流方式"));
            false
        }
    };
    let local_proxy = parse_host_port(&form.local_proxy, "localProxy")
        .map_err(|e| errors.push(e))
        .ok()
        .flatten();
    let targets = parse_domains(&form.targets);
    let extra_domains = parse_domains(&form.extra_domains);

    if !errors.is_empty() {
        return Err(errors);
    }

    let mut next = set.clone();
    let p = &mut next.agent[0];
    let credential_ref = (!user.is_empty()).then(|| CREDENTIAL_REF.to_string());
    p.egress.candidates = ports
        .expect("checked")
        .into_iter()
        .map(|(port, proto)| Candidate {
            id: if proto == form.proto {
                format!("exit-{port}")
            } else {
                format!("exit-{port}-{}", proto_name(proto))
            },
            host: host.to_string(),
            port,
            proto,
            credential_ref: credential_ref.clone(),
        })
        .collect();
    p.egress.tunnel = tunnel;
    let old_targets = p.targets.clone();
    if form.max_inflight > 0 {
        p.egress.relay.max_inflight = form.max_inflight;
    }
    if form.connect_timeout_ms > 0 {
        p.egress.relay.connect_timeout_ms = form.connect_timeout_ms;
    }
    if form.dial_budget_ms > 0 {
        p.egress.relay.dial_budget_ms = form.dial_budget_ms;
    }
    if !targets.is_empty() {
        p.targets = targets
            .into_iter()
            .map(|domain| Target {
                domain,
                trace: true,
            })
            .collect();
    }
    if form.routing_mode != ROUTING_CUSTOM {
        p.routing = Routing::preset(
            "agent-egress",
            &p.targets,
            split,
            &extra_domains,
            form.domestic_direct,
            local_proxy.map(|(server, port)| RoutingOutbound {
                name: "daily-proxy".into(),
                server,
                port,
                credential_ref: None,
                cipher: None,
                dialer_proxy: None,
                kind: match form.local_proxy_proto {
                    UpstreamProto::Http => RoutingOutboundKind::Http,
                    UpstreamProto::Socks5 => RoutingOutboundKind::Socks5,
                },
            }),
        );
    } else {
        p.targets = old_targets;
    }
    p.proxy_scope.mode = if form.system_proxy {
        ProxyScopeMode::SystemAndExplicit
    } else {
        ProxyScopeMode::ExplicitOnly
    };
    let ip = form.expected_ip.trim();
    p.egress.expected.ip = (!ip.is_empty()).then(|| ip.to_string());
    let (_, _, windows_id, iana) = tz.expect("checked");
    p.locale.timezone.windows_id = (*windows_id).into();
    p.locale.timezone.iana = (*iana).into();
    p.locale.browser_lang = form.language.clone();
    let primary = form.language.split('-').next().unwrap_or("en");
    p.locale.accept_language = format!("{},{primary}", form.language);

    let problems: Vec<FieldError> = profile::validate(&next)
        .into_iter()
        .map(|e| {
            let path = e.path.as_str();
            match path {
                "agent[0].egress.expected.ip" => err("expectedIp", "需要是一个完整的 IP 地址"),
                "agent[0].egress.relay.maxInflight" => err("maxInflight", "至少 1"),
                "agent[0].egress.relay.dialBudgetMs" => err(
                    "dialBudgetMs",
                    "要小于 5000 毫秒：内核等一个连接最多 5 秒，超过它就白等",
                ),
                "agent[0].egress.relay.connectTimeoutMs" => {
                    err("connectTimeoutMs", "要大于 0，并且不超过拨号总时长")
                }
                _ if path.starts_with("agent[0].targets") => {
                    err("targets", "每行一个域名，不带 http:// 和路径，至少一个")
                }
                _ if path.starts_with("agent[0].routing.rules") => {
                    err("extraDomains", "每行一个域名，不带 http:// 和路径")
                }
                _ if path.starts_with("agent[0].egress.tunnel") => {
                    err("tunnelServers", &format!("隧道设置不对：{}", e.message))
                }
                _ if path.starts_with("agent[0].routing.outbounds") => {
                    err("localProxy", &format!("本机代理设置不对：{}", e.message))
                }
                _ => e,
            }
        })
        .collect();
    if !problems.is_empty() {
        return Err(problems);
    }
    *set = next;
    Ok(Applied {
        exit: change,
        tunnel: tunnel_change,
        takeover: form.takeover_enabled,
    })
}

/// Validate installation settings separately, before saving credentials or the profile.
pub fn apply_settings(
    form: &ExitForm,
    previous: crate::controller::Settings,
) -> Result<crate::controller::Settings, Vec<FieldError>> {
    let port = match form.core_mixed_port.as_deref().map(str::trim) {
        None => previous.core_mixed_port,
        Some("" | "0") => None,
        Some(value) => {
            let port = value
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| {
                    vec![err(
                        "coreMixedPort",
                        "端口必须是 1–65535；留空或 0 使用默认值",
                    )]
                })?;
            let defaults = crate::core::CorePorts::default();
            if [
                defaults.controller,
                defaults.dns,
                defaults.relay,
                defaults.tunnel,
                defaults.tunnel + 1,
            ]
            .contains(&port)
            {
                return Err(vec![err(
                    "coreMixedPort",
                    "入口端口不能与控制、DNS、relay、隧道或检测端口相同",
                )]);
            }
            Some(port)
        }
    };
    Ok(crate::controller::Settings {
        takeover_enabled: form.takeover_enabled,
        core_mixed_port: port,
    })
}

/// What the page server needs from its owner.
pub trait SetupBackend: Send + Sync {
    fn view(&self) -> SetupView;
    /// Validate and store. Errors are per field.
    fn save(&self, form: ExitForm) -> Result<(), Vec<FieldError>>;
    fn startup(&self, _enabled: bool) -> Result<(), Vec<FieldError>> {
        Err(vec![err("startup", "自启管理不可用")])
    }
    fn shortcuts(&self) -> Result<(), Vec<FieldError>> {
        Err(vec![err("shortcuts", "入口管理不可用")])
    }
    fn dashboard(&self) -> Option<crate::dashboard::View> {
        None
    }
    fn rules(&self, _form: crate::dashboard::RulesForm) -> Result<(), Vec<FieldError>> {
        Err(vec![err("rules", "看板不可用")])
    }
    fn path(&self, _path: String) -> Result<(), Vec<FieldError>> {
        Err(vec![err("path", "看板不可用")])
    }
}

pub struct SetupServer {
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn respond(s: &mut TcpStream, status: &str, ctype: &str, body: &str) {
    let _ = write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// The page with the view and the option lists embedded as JSON.
pub fn page(token: &str, view: &SetupView) -> String {
    let js = |v: serde_json::Value| v.to_string().replace("</", "<\\/");
    let tz: Vec<_> = TIMEZONES.iter().map(|t| (t.0, t.1)).collect();
    let presets: Vec<_> = crate::tunnel::PRESETS
        .iter()
        .map(|p| {
            serde_json::json!({
                "key": p.key,
                "label": p.label,
                "source": p.source,
                "checked": p.checked,
                "note": p.note,
                "servers": p.servers.iter().map(|(h, port)| format!("{h}:{port}")).collect::<Vec<_>>(),
                "cipher": p.cipher,
            })
        })
        .collect();
    PAGE.replace("__TOKEN__", token)
        .replace(
            "__VIEW__",
            &js(serde_json::to_value(view).expect("serializes")),
        )
        .replace("__TZ__", &js(serde_json::json!(tz)))
        .replace("__LANG__", &js(serde_json::json!(LANGUAGES)))
        .replace("__PRESETS__", &js(serde_json::json!(presets)))
        .replace("__CIPHERS__", &js(serde_json::json!(TUNNEL_CIPHERS)))
        .replace(
            "__SUGGESTED__",
            &js(serde_json::json!(profile::SUGGESTED_EXTRA_DOMAINS)),
        )
}

fn handle(mut s: TcpStream, port: u16, token: &str, backend: &dyn SetupBackend) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_BODY {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let token_ok = query.split('&').any(|kv| kv == format!("t={token}"));
    let me = format!("127.0.0.1:{port}");
    // A page served under another name (DNS rebinding) never reaches the handlers.
    if header(&head, "host") != Some(me.as_str()) {
        return respond(&mut s, "403 Forbidden", "text/plain", "host rejected");
    }
    match (method, path) {
        ("GET", "/dashboard" | "/state") if token_ok => match backend.dashboard() {
            Some(view) => {
                let (ctype, body) = if path == "/state" {
                    (
                        "application/json; charset=utf-8",
                        serde_json::to_string(&view).expect("serializes"),
                    )
                } else {
                    (
                        "text/html; charset=utf-8",
                        crate::dashboard::page(token, &view),
                    )
                };
                respond(&mut s, "200 OK", ctype, &body)
            }
            None => respond(
                &mut s,
                "404 Not Found",
                "text/plain",
                "dashboard unavailable",
            ),
        },
        ("GET", "/setup") if token_ok => respond(
            &mut s,
            "200 OK",
            "text/html; charset=utf-8",
            &page(token, &backend.view()),
        ),
        ("POST", "/save" | "/rules" | "/path" | "/startup" | "/shortcuts") if token_ok => {
            if header(&head, "origin") != Some(format!("http://{me}").as_str()) {
                return respond(&mut s, "403 Forbidden", "text/plain", "origin rejected");
            }
            let len: usize = header(&head, "content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if len > MAX_BODY {
                return respond(&mut s, "413 Payload Too Large", "text/plain", "too large");
            }
            while buf.len() < head_end + len {
                match s.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let body = &buf[head_end..buf.len().min(head_end + len)];
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct PathForm {
                path: String,
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct StartupForm {
                enabled: bool,
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct EmptyForm {}
            let malformed = || vec![err("form", "表单格式不对")];
            let result = match path {
                "/startup" => serde_json::from_slice::<StartupForm>(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.startup(form.enabled)),
                "/shortcuts" => serde_json::from_slice::<EmptyForm>(body)
                    .map_err(|_| malformed())
                    .and_then(|_| backend.shortcuts()),
                "/rules" => serde_json::from_slice(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.rules(form)),
                "/path" => serde_json::from_slice::<PathForm>(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.path(form.path)),
                _ => serde_json::from_slice::<ExitForm>(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.save(form)),
            };
            buf.iter_mut().for_each(|b| *b = 0);
            let reply = match result {
                Ok(()) => serde_json::json!({ "ok": true }),
                Err(errors) => serde_json::json!({ "ok": false, "errors": errors }),
            };
            respond(
                &mut s,
                "200 OK",
                "application/json; charset=utf-8",
                &reply.to_string(),
            )
        }
        (
            _,
            "/setup" | "/save" | "/dashboard" | "/state" | "/rules" | "/path" | "/startup"
            | "/shortcuts",
        ) => respond(&mut s, "403 Forbidden", "text/plain", "token rejected"),
        _ => respond(&mut s, "404 Not Found", "text/plain", "not found"),
    }
}

impl SetupServer {
    pub fn dashboard_url(&self) -> String {
        format!("http://127.0.0.1:{}/dashboard?t={}", self.port, self.token)
    }
    /// Listen on 127.0.0.1:`port` (0 picks a free port).
    pub fn start(
        port: u16,
        token: String,
        backend: Arc<dyn SetupBackend>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (stop2, token2) = (stop.clone(), token.clone());
        let thread = std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, _)) => {
                        let _ = s.set_nonblocking(false);
                        handle(s, port, &token2, backend.as_ref());
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        });
        Ok(Self {
            port,
            token,
            stop,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/setup?t={}", self.port, self.token)
    }
}

impl Drop for SetupServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

const PAGE: &str = r##"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>出口与分流设置</title>
<style>
:root{--bg:#f6f7f5;--card:#fff;--fg:#1d2420;--muted:#5d6862;--line:#d8ddd9;--accent:#28664b;--err:#b3261e;--okbg:#e5f2ea;--errbg:#fbeaea;--soft:#eef2ef}
@media (prefers-color-scheme:dark){:root{--bg:#141816;--card:#1d2320;--fg:#e6ebe8;--muted:#9aa5a0;--line:#333b37;--accent:#5fb98d;--err:#f2867e;--okbg:#1f3329;--errbg:#3a2120;--soft:#242b27}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 system-ui,"Microsoft YaHei UI",sans-serif}
main{max-width:680px;margin:0 auto;padding:24px 16px 48px}
h1{font-size:22px;margin:0 0 4px}p.lead{color:var(--muted);margin:0 0 20px}
section,details{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:16px 18px;margin-bottom:16px}
h2{font-size:16px;margin:0 0 12px}summary{font-weight:600;cursor:pointer}
label{display:block;font-weight:600;margin:12px 0 4px}label:first-of-type{margin-top:0}
.hint{color:var(--muted);font-size:13px;font-weight:400;margin-top:3px}
input[type=text],input[type=password],input[type=number],select,textarea{width:100%;padding:8px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--fg);font:inherit}
textarea{min-height:84px;resize:vertical;font-family:ui-monospace,Consolas,monospace;font-size:13px}
.row{display:flex;gap:12px}.row>div{flex:1;min-width:0}.row>.narrow{flex:0 0 140px}
.check{display:flex;gap:10px;align-items:flex-start;font-weight:400;margin-top:12px}.check input{margin-top:4px}
.radio{display:flex;gap:10px;align-items:flex-start;font-weight:400;margin:8px 0}.radio input{margin-top:4px}
.box{background:var(--soft);border-radius:8px;padding:10px 12px;margin-top:10px;font-size:13px;color:var(--muted)}
.sub{border-left:3px solid var(--line);padding-left:12px;margin-top:8px}
.fe{color:var(--err);font-size:13px;margin-top:3px}
button{background:var(--accent);color:#fff;border:0;border-radius:6px;padding:10px 22px;font:inherit;font-weight:600;cursor:pointer}
button.link{background:none;color:var(--accent);padding:0;font-weight:400;font-size:13px;text-decoration:underline}
button:disabled{opacity:.5;cursor:default}
#msg{margin-top:14px;padding:10px 12px;border-radius:6px;display:none}
#msg.ok{display:block;background:var(--okbg)}#msg.bad{display:block;background:var(--errbg);color:var(--err)}
[hidden]{display:none!important}
@media (max-width:480px){.row{flex-direction:column;gap:0}.row>.narrow{flex:none}}
</style></head><body><main>
<h1>出口与分流设置</h1>
<p class="lead">填你自己的出口代理，再选哪些流量走它。默认只有从托盘打开的浏览器和 CLI 会走这个出口；打开「系统级分流」后，整台电脑跟随系统代理的程序也会进入下面的规则。</p>
<form id="f" autocomplete="off">

<section><h2>出口代理</h2>
<div class="row"><div><label for="host">地址</label><input id="host" type="text" placeholder="例如 203.0.113.10 或 proxy.example.com"><div class="fe" data-for="host"></div></div>
<div class="narrow"><label for="proto">协议</label><select id="proto"><option value="socks5">SOCKS5</option><option value="http">HTTP</option></select></div></div>
<label for="ports">端口</label><input id="ports" type="text" placeholder="例如 12324, 12323/http">
<div class="hint">多个端口用逗号分隔，必须是同一个出口（同一台机器）；某个端口协议不同，就写成 <code>12323/http</code>。连不上一个会自动换下一个。</div><div class="fe" data-for="ports"></div>
<div class="row"><div><label for="username">用户名</label><input id="username" type="text" placeholder="没有就留空"><div class="fe" data-for="username"></div></div>
<div><label for="password">密码</label><input id="password" type="password" placeholder=""><div class="fe" data-for="password"></div></div></div>
<div class="hint">密码保存在 Windows 凭据管理器，档案只保存引用；页面不会回显已保存的密码。</div>
<label for="expectedIp">出口 IP（可选）</label><input id="expectedIp" type="text" placeholder="填了就严格比对，不一致算检测不通过">
<div class="fe" data-for="expectedIp"></div>
</section>

<section><h2>加密隧道（可选）</h2>
<label class="check"><input id="tunnelEnabled" type="checkbox"><span>经加密隧道连接出口
<div class="hint">在国内直连某些出口（例如 IPRoyal 的 ISP / 静态住宅代理）会被重置、时通时断时打开。每个连接先走隧道，不通再按下面的设置直连；两条路均连接配置的出口，最终 IP 由检测确认。</div></span></label>
<div id="tunnelBox" class="sub">
<label for="tunnelPreset">隧道渠道</label><select id="tunnelPreset"></select><div class="fe" data-for="tunnelPreset"></div>
<div id="presetBox" class="box"></div>
<div id="customBox">
<label for="tunnelServers">隧道服务器（Shadowsocks）</label><textarea id="tunnelServers" placeholder="每行一个，地址:端口&#10;例如 ss.example.com:8388"></textarea><div class="fe" data-for="tunnelServers"></div>
<div class="row"><div><label for="tunnelCipher">加密方式</label><select id="tunnelCipher"></select><div class="fe" data-for="tunnelCipher"></div></div>
<div><label for="tunnelPassword">隧道密码</label><input id="tunnelPassword" type="password"><div class="fe" data-for="tunnelPassword"></div></div></div>
</div>
<label class="check"><input id="tunnelDirectFallback" type="checkbox"><span>隧道不通时直连出口
<div class="hint">建议打开。两条路时好时坏会轮换，留着直连这条路断线最少。</div></span></label>
</div>
</section>

<section><h2>分流</h2>
<label class="radio"><input type="radio" name="routingMode" value="custom"><span><b>保留现有路由数据</b>
<div class="hint">不改动已导入的出站、分组、规则和顺序。下面两个选项会重新套用预设并替换现有路由表。</div></span></label>
<label class="radio"><input type="radio" name="routingMode" value="all"><span><b>全部走出口</b>
<div class="hint">进入规则的所有网站都走出口。简单，但出口流量最多。</div></span></label>
<label class="radio"><input type="radio" name="routingMode" value="split"><span><b>分流</b>
<div class="hint">只有下面列出的站点走出口；其他网站交给你自己的本机代理（比如 Clash 的端口），本机代理没开就直连。</div></span></label>
<div class="fe" data-for="routingMode"></div>
<div id="splitBox" class="sub">
<div class="row"><div><label for="localProxy">本机代理（可选）</label><input id="localProxy" type="text" placeholder="例如 127.0.0.1:7890，没有就留空"><div class="fe" data-for="localProxy"></div></div>
<div class="narrow"><label for="localProxyProto">协议</label><select id="localProxyProto"><option value="socks5">SOCKS5</option><option value="http">HTTP</option></select></div></div>
<div class="hint">留空：其他网站直连。本机代理不可用时可回退到直连；每 15 秒进行一次健康检查，恢复可用后重新选择。</div>
<label for="extraDomains">也走出口的其他域名</label><textarea id="extraDomains" placeholder="每行一个，例如 openai.com"></textarea>
<div class="hint">按后缀匹配，<code>openai.com</code> 也包括 <code>api.openai.com</code>。想让某个站点改走本机代理（比如 ChatGPT），就把它从这里和下面的「检测站点」里都删掉。 <button type="button" class="link" id="suggest">填入常用 AI 域名</button></div>
<div class="fe" data-for="extraDomains"></div>
</div>
<label class="check"><input id="domesticDirect" type="checkbox"><span>国内网站直连
<div class="hint">需要自行把可信的 GeoSite.dat、GeoIP.dat 放到 bin 目录；程序不会下载。没有这些文件时请关闭此项。</div></span></label>
<label for="targets">检测站点</label><textarea id="targets"></textarea>
<div class="hint">每行一个，每次逐项检测（G2）。预设会为这些站点生成出口规则；导入表保持原规则。要能读到 <code>/cdn-cgi/trace</code> 才能比对出口。</div>
<div class="fe" data-for="targets"></div>
</section>

<section><h2>系统级分流</h2>
<label for="coreMixedPort">本机代理入口端口</label><input id="coreMixedPort" type="number" min="0" max="65535" placeholder="默认 17893"><div class="fe" data-for="coreMixedPort"></div>
<div class="hint">留空或 0 使用默认端口。接替已有代理时，填写客户端当前使用的入口端口；不会改写用户级代理环境变量。保存后下次进入生效。若本机程序依赖这个端口，先核对，避免断网。</div>
<label class="check"><input id="systemProxy" type="checkbox"><span>进入 Agent 模式时接管 Windows 系统代理，退出时恢复原样
<div class="hint">电脑上跟随 Windows 系统代理的程序也使用上面的规则；是否遵循系统代理由各程序决定。发现其他程序改写时会撤销就绪并告警，请关闭冲突程序的系统代理开关后重新进入。默认关闭，真实写入需要接管门放行。</div></span></label>
</section>

<section><h2>时区与语言</h2>
<div class="row"><div><label for="timezone">目标时区</label><select id="timezone"></select><div class="fe" data-for="timezone"></div></div>
<div><label for="language">浏览器语言</label><select id="language"></select><div class="fe" data-for="language"></div></div></div>
<label class="check"><input id="takeoverEnabled" type="checkbox"><span>进入 Agent 模式时把系统时区改成目标时区，退出时改回原样
<div class="hint">不勾选就不修改系统时区。仍会核对系统与浏览器时区；目标不一致或缺少证据时无法就绪。</div></span></label>
</section>

<details><summary>高级：转发层</summary>
<div class="hint" style="margin-top:8px">转发层替你连出口：一个端口或一条路不通，就换下一个，出口 IP 不变。</div>
<div class="row"><div><label for="maxInflight">同时建立的连接数</label><input id="maxInflight" type="number" min="1" max="64"><div class="fe" data-for="maxInflight"></div>
<div class="hint">只限制「正在建立」的连接，已经连上的不占名额。Claude、ChatGPT 客户端会一次新建很多连接，太小会排队失败。</div></div>
<div><label for="connectTimeoutMs">每一步等待（毫秒）</label><input id="connectTimeoutMs" type="number" min="100" max="4900"><div class="fe" data-for="connectTimeoutMs"></div>
<div class="hint">直连出口时每一步最多等多久；隧道自己会多等一些。</div></div></div>
<label for="dialBudgetMs">一个连接的总时长（毫秒）</label><input id="dialBudgetMs" type="number" min="500" max="4999"><div class="fe" data-for="dialBudgetMs"></div>
<div class="hint">在这段时间里把所有路轮流试一遍。必须小于 5000：内核等一个连接最多 5 秒。</div>
</details>

<button id="save" type="submit">保存</button>
<div id="msg"></div>
<div class="fe" data-for="form"></div><div id="other" class="fe"></div>
</form></main>
<script>
const T="__TOKEN__",V=__VIEW__,TZ=__TZ__,LANG=__LANG__,PRESETS=__PRESETS__,CIPHERS=__CIPHERS__,SUGGESTED=__SUGGESTED__;
const $=id=>document.getElementById(id);
const opt=(sel,v,t)=>{const o=document.createElement("option");o.value=v;o.textContent=t;$(sel).append(o)};
for(const [k,l] of TZ)opt("timezone",k,l);
for(const [k,l] of LANG)opt("language",k,l+"  "+k);
for(const p of PRESETS)opt("tunnelPreset",p.key,p.label);
opt("tunnelPreset","custom","自定义 Shadowsocks 隧道");
for(const c of CIPHERS)opt("tunnelCipher",c,c);
for(const k of ["host","ports","username","expectedIp","tunnelServers","localProxy","targets","extraDomains","coreMixedPort"])$(k).value=V[k]||"";
for(const k of ["maxInflight","connectTimeoutMs","dialBudgetMs"])$(k).value=V[k];
$("proto").value=V.proto;$("timezone").value=V.timezone;$("language").value=V.language;
$("tunnelPreset").value=V.tunnelPreset;$("tunnelCipher").value=V.tunnelCipher;$("localProxyProto").value=V.localProxyProto;
for(const k of ["takeoverEnabled","tunnelEnabled","tunnelDirectFallback","domesticDirect","systemProxy"])$(k).checked=V[k];
document.querySelector('input[name=routingMode][value="'+V.routingMode+'"]').checked=true;
$("password").placeholder=V.passwordStored?"已保存，不改就留空":"没有就留空";
function tunnelPwHint(){$("tunnelPassword").placeholder=V.tunnelPasswordStored&&V.tunnelPreset==="custom"?"已保存，不改就留空":""}
tunnelPwHint();
function sync(){
 $("tunnelBox").hidden=!$("tunnelEnabled").checked;
 const pre=PRESETS.find(p=>p.key===$("tunnelPreset").value);
 $("customBox").hidden=!!pre;$("presetBox").hidden=!pre;
 if(pre){$("presetBox").textContent="";const b=$("presetBox");
  const line=t=>{const d=document.createElement("div");d.textContent=t;b.append(d)};
  line("来源："+pre.source+"；文档核对日期："+pre.checked+"（节点连通性需在你的网络检测）。");
  line("服务器："+pre.servers.join("、")+"，加密 "+pre.cipher+"，密码按官方公开值自动填写。");
  line(pre.note)}
 $("splitBox").hidden=document.querySelector("input[name=routingMode]:checked").value!=="split";
}
for(const id of ["tunnelEnabled","tunnelPreset"])$(id).addEventListener("change",sync);
document.querySelectorAll("input[name=routingMode]").forEach(r=>r.addEventListener("change",()=>{sync();
 if(r.value==="split"&&r.checked&&!$("extraDomains").value.trim())$("extraDomains").value=SUGGESTED.join("\n")}));
$("suggest").addEventListener("click",()=>{const have=$("extraDomains").value.split(/\s+/).filter(Boolean);
 $("extraDomains").value=[...new Set(have.concat(SUGGESTED))].join("\n")});
sync();
if(V.locked){$("save").disabled=true;show("bad","现在处于 Agent 模式。先从托盘「退出 Agent 模式」，再修改设置。")}
function show(c,t){const m=$("msg");m.className=c;m.textContent=t}
$("f").addEventListener("submit",async e=>{e.preventDefault();
 document.querySelectorAll(".fe").forEach(x=>x.textContent="");$("msg").className="";
 const num=id=>parseInt($(id).value,10)||0;
 const form={host:$("host").value,proto:$("proto").value,ports:$("ports").value,username:$("username").value,
  coreMixedPort:$("coreMixedPort").value,
  password:$("password").value||null,timezone:$("timezone").value,language:$("language").value,
  expectedIp:$("expectedIp").value,takeoverEnabled:$("takeoverEnabled").checked,
  tunnelEnabled:$("tunnelEnabled").checked,tunnelPreset:$("tunnelPreset").value,tunnelServers:$("tunnelServers").value,
  tunnelCipher:$("tunnelCipher").value,tunnelPassword:$("tunnelPassword").value||null,tunnelDirectFallback:$("tunnelDirectFallback").checked,
  routingMode:document.querySelector("input[name=routingMode]:checked").value,localProxy:$("localProxy").value,
  localProxyProto:$("localProxyProto").value,domesticDirect:$("domesticDirect").checked,
  targets:$("targets").value,extraDomains:$("extraDomains").value,systemProxy:$("systemProxy").checked,
  maxInflight:num("maxInflight"),connectTimeoutMs:num("connectTimeoutMs"),dialBudgetMs:num("dialBudgetMs")};
 $("save").disabled=true;
 try{const r=await fetch("/save?t="+T,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(form)});
  const j=await r.json();
  if(j.ok){$("password").value="";$("password").placeholder=form.username?"已保存，不改就留空":"没有就留空";
   $("tunnelPassword").value="";V.tunnelPreset=form.tunnelPreset;V.tunnelPasswordStored=form.tunnelEnabled||V.tunnelPasswordStored;tunnelPwHint();
   show("ok","已保存。回到托盘点「进入 Agent 模式」；已经在 Agent 模式里的，要退出再进入才生效。这个页面可以关掉了。")}
  else{let other=[];for(const x of j.errors){const el=document.querySelector('.fe[data-for="'+x.path+'"]');
    if(el)el.textContent=x.message;else other.push(x.path+"："+x.message)}
   if(j.errors.some(x=>["maxInflight","connectTimeoutMs","dialBudgetMs"].includes(x.path)))document.querySelector("details").open=true;
   $("other").textContent=other.join("；");show("bad","有几处需要改，见标红的说明。")}
 }catch(err){show("bad","保存失败："+err+"。托盘程序可能已经退出。")}
 finally{$("save").disabled=V.locked}
});
</script></body></html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> ExitForm {
        ExitForm {
            core_mixed_port: None,
            host: " 203.0.113.10 ".into(),
            proto: UpstreamProto::Socks5,
            ports: "12324, 12323，12324".into(),
            username: "alice".into(),
            password: Some("s3cret".into()),
            timezone: "us-west".into(),
            language: "en-GB".into(),
            expected_ip: "".into(),
            takeover_enabled: true,
            tunnel_enabled: false,
            tunnel_preset: CUSTOM_TUNNEL.into(),
            tunnel_servers: String::new(),
            tunnel_cipher: String::new(),
            tunnel_password: None,
            tunnel_direct_fallback: true,
            routing_mode: ROUTING_ALL.into(),
            local_proxy: String::new(),
            local_proxy_proto: UpstreamProto::Socks5,
            domestic_direct: false,
            targets: String::new(),
            extra_domains: String::new(),
            system_proxy: false,
            max_inflight: 0,
            connect_timeout_ms: 0,
            dial_budget_ms: 0,
        }
    }

    fn input<'a>(user: Option<&'a str>) -> ViewInput<'a> {
        ViewInput {
            core_mixed_port: None,
            stored_user: user,
            tunnel_password_stored: false,
            takeover_enabled: true,
            locked: false,
        }
    }

    #[test]
    fn settings_form_keeps_legacy_port_and_validates_explicit_changes() {
        let previous = crate::controller::Settings {
            takeover_enabled: false,
            core_mixed_port: Some(27893),
        };
        let mut json = serde_json::to_value(form()).unwrap();
        for (value, expected) in [
            (None, Some(27893)),
            (Some(""), None),
            (Some("0"), None),
            (Some(" 27894 "), Some(27894)),
        ] {
            json.as_object_mut().unwrap().remove("coreMixedPort");
            if let Some(value) = value {
                json["coreMixedPort"] = value.into();
            }
            let f: ExitForm = serde_json::from_value(json.clone()).unwrap();
            let next = apply_settings(&f, previous).unwrap();
            assert_eq!(next.core_mixed_port, expected);
            assert!(next.takeover_enabled);
        }
        for value in [
            "-1", "65536", "no-port", "17909", "17553", "17325", "17326", "17327",
        ] {
            json["coreMixedPort"] = value.into();
            let f: ExitForm = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(
                apply_settings(&f, previous).unwrap_err()[0].path,
                "coreMixedPort"
            );
        }
        let mut i = input(None);
        i.core_mixed_port = previous.core_mixed_port;
        assert_eq!(view(&ProfileSet::default(), &i).core_mixed_port, "27893");
    }
    #[test]
    fn a_form_becomes_one_candidate_per_port_with_one_credential() {
        let mut set = ProfileSet::default();
        let a = apply(&mut set, &form(), None, false).unwrap();
        assert!(a.takeover);
        assert_eq!(
            a.exit,
            CredentialChange::Store {
                user: "alice".into(),
                pass: "s3cret".into()
            }
        );
        assert_eq!(a.tunnel, CredentialChange::Keep);
        let p = &set.agent[0];
        assert_eq!(
            p.egress
                .candidates
                .iter()
                .map(|c| (
                    c.id.as_str(),
                    c.host.as_str(),
                    c.port,
                    c.credential_ref.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("exit-12324", "203.0.113.10", 12324, Some("exit")),
                ("exit-12323", "203.0.113.10", 12323, Some("exit"))
            ]
        );
        assert_eq!(p.locale.timezone.windows_id, "Pacific Standard Time");
        assert_eq!(p.locale.timezone.iana, "America/Los_Angeles");
        assert_eq!(p.locale.accept_language, "en-GB,en");
        assert!(profile::validate(&set).is_empty());

        let v = view(&set, &input(Some("alice")));
        assert_eq!(
            (v.ports.as_str(), v.timezone.as_str(), v.password_stored),
            ("12324, 12323", "us-west", true)
        );
        let json = serde_json::to_string(&v).unwrap();
        assert!(
            !json.contains("s3cret"),
            "the view never carries a password"
        );
    }

    #[test]
    fn a_port_can_carry_its_own_protocol() {
        let mut set = ProfileSet::default();
        let mut f = form();
        f.ports = "12324, 12323/http".into();
        apply(&mut set, &f, None, false).unwrap();
        let c = &set.agent[0].egress.candidates;
        assert_eq!(
            c.iter().map(|c| (c.port, c.proto)).collect::<Vec<_>>(),
            vec![(12324, UpstreamProto::Socks5), (12323, UpstreamProto::Http)]
        );
        assert_eq!(view(&set, &input(None)).ports, "12324, 12323/http");
        f.ports = "12323/quic".into();
        assert_eq!(
            apply(&mut set.clone(), &f, None, false).unwrap_err()[0].path,
            "ports"
        );
    }

    #[test]
    fn credential_rules() {
        let mut set = ProfileSet::default();
        let mut f = form();
        f.password = None;
        assert_eq!(
            apply(&mut set.clone(), &f, Some("alice"), false)
                .unwrap()
                .exit,
            CredentialChange::Keep
        );
        let e = apply(&mut set.clone(), &f, Some("bob"), false).unwrap_err();
        assert_eq!(e[0].path, "password", "a new user name needs a password");
        f.username = " ".into();
        let a = apply(&mut set, &f, Some("alice"), false).unwrap();
        assert_eq!(a.exit, CredentialChange::Delete);
        assert!(set.agent[0].egress.candidates[0].credential_ref.is_none());
        f.password = Some("x".into());
        assert_eq!(
            apply(&mut set, &f, None, false).unwrap_err()[0].path,
            "username"
        );
    }

    #[test]
    fn the_preset_tunnel_fills_itself_in_and_needs_exit_credentials() {
        let mut set = ProfileSet::default();
        let mut f = form();
        f.tunnel_enabled = true;
        f.tunnel_preset = "iproyal-cn".into();
        let a = apply(&mut set, &f, None, false).unwrap();
        let pre = crate::tunnel::preset("iproyal-cn").unwrap();
        assert_eq!(
            a.tunnel,
            CredentialChange::Store {
                user: "shadowsocks".into(),
                pass: pre.password.into()
            }
        );
        let t = set.agent[0].egress.tunnel.as_ref().unwrap();
        assert!(t.enabled && t.direct_fallback);
        assert_eq!(t.preset.as_deref(), Some("iproyal-cn"));
        assert_eq!(t.servers, pre.servers());
        let v = view(&set, &input(Some("alice")));
        assert!(v.tunnel_enabled && v.tunnel_preset == "iproyal-cn");

        // Through the tunnel the exit cannot see the user's allow-list, so a user name is required.
        f.username = String::new();
        f.password = None;
        let e = apply(&mut ProfileSet::default(), &f, None, false).unwrap_err();
        assert!(e.iter().any(|e| e.path == "username"), "{e:?}");
    }

    #[test]
    fn a_custom_tunnel_needs_servers_cipher_and_password() {
        let mut f = form();
        f.tunnel_enabled = true;
        f.tunnel_preset = CUSTOM_TUNNEL.into();
        let e = apply(&mut ProfileSet::default(), &f, None, false).unwrap_err();
        let paths: Vec<&str> = e.iter().map(|e| e.path.as_str()).collect();
        for p in ["tunnelServers", "tunnelCipher", "tunnelPassword"] {
            assert!(paths.contains(&p), "{p}: {paths:?}");
        }
        f.tunnel_servers = "ss.example.com:8388\n[2001:db8::1]:8389".into();
        f.tunnel_cipher = "aes-256-gcm".into();
        f.tunnel_password = Some("pw".into());
        f.tunnel_direct_fallback = false;
        let mut set = ProfileSet::default();
        let a = apply(&mut set, &f, None, false).unwrap();
        assert!(matches!(a.tunnel, CredentialChange::Store { ref pass, .. } if pass == "pw"));
        let t = set.agent[0].egress.tunnel.clone().unwrap();
        assert_eq!(t.preset, None);
        assert!(!t.direct_fallback);
        assert_eq!(t.servers[1].host, "2001:db8::1");

        // Saved again without retyping: the stored password is kept.
        f.tunnel_password = None;
        assert_eq!(
            apply(&mut set.clone(), &f, None, true).unwrap().tunnel,
            CredentialChange::Keep
        );
        // Switching it off keeps the servers for next time.
        f.tunnel_enabled = false;
        apply(&mut set, &f, None, true).unwrap();
        let t = set.agent[0].egress.tunnel.as_ref().unwrap();
        assert!(!t.enabled && t.servers.len() == 2);
    }

    #[test]
    fn split_routing_with_a_local_proxy_and_domestic_direct() {
        let mut set = ProfileSet::default();
        let mut f = form();
        f.routing_mode = ROUTING_SPLIT.into();
        f.local_proxy = "127.0.0.1:7890".into();
        f.local_proxy_proto = UpstreamProto::Http;
        f.domestic_direct = true;
        f.extra_domains = "openai.com\n*.grok.com, claude.ai\nopenai.com".into();
        f.targets = "claude.ai\nwww.anthropic.com".into();
        f.system_proxy = true;
        apply(&mut set, &f, None, false).unwrap();
        let p = &set.agent[0];
        assert_eq!(p.routing.final_target, "aec-other");
        let fb = p.routing.outbounds.first().unwrap();
        assert_eq!(
            (fb.server.as_str(), fb.port, fb.kind),
            ("127.0.0.1", 7890, RoutingOutboundKind::Http)
        );
        assert!(p.routing.uses_geodata());
        assert_eq!(
            view(&set, &input(None)).extra_domains,
            "openai.com\ngrok.com"
        );
        assert_eq!(
            p.targets
                .iter()
                .map(|t| t.domain.as_str())
                .collect::<Vec<_>>(),
            vec!["claude.ai", "www.anthropic.com"]
        );
        assert_eq!(p.proxy_scope.mode, ProxyScopeMode::SystemAndExplicit);
        let v = view(&set, &input(None));
        assert_eq!(
            (
                v.routing_mode.as_str(),
                v.local_proxy.as_str(),
                v.system_proxy
            ),
            ("split", "127.0.0.1:7890", true)
        );

        // No local proxy is fine (the rest goes direct); a malformed one is not.
        f.local_proxy = " ".into();
        apply(&mut set.clone(), &f, None, false).unwrap();
        f.local_proxy = "http://127.0.0.1:7890".into();
        assert_eq!(
            apply(&mut set.clone(), &f, None, false).unwrap_err()[0].path,
            "localProxy"
        );
        // Empty targets keep the current ones; a bad one is reported on the field.
        f.local_proxy = String::new();
        f.targets = "https://claude.ai/".into();
        assert!(apply(&mut set, &f, None, false)
            .unwrap_err()
            .iter()
            .any(|e| e.path == "targets"));
    }

    #[test]
    fn relay_settings_are_validated_on_their_own_fields() {
        let mut set = ProfileSet::default();
        let mut f = form();
        f.max_inflight = 20;
        f.connect_timeout_ms = 1500;
        f.dial_budget_ms = 4500;
        apply(&mut set, &f, None, false).unwrap();
        let r = &set.agent[0].egress.relay;
        assert_eq!(
            (r.max_inflight, r.connect_timeout_ms, r.dial_budget_ms),
            (20, 1500, 4500)
        );
        f.dial_budget_ms = 6000;
        let e = apply(&mut set, &f, None, false).unwrap_err();
        assert!(e.iter().any(|e| e.path == "dialBudgetMs"), "{e:?}");
    }

    #[test]
    fn bad_fields_are_reported_and_leave_the_set_unchanged() {
        let before = ProfileSet::default();
        type Mutation = Box<dyn Fn(&mut ExitForm)>;
        let cases: Vec<(&str, Mutation)> = vec![
            ("host", Box::new(|f| f.host = "".into())),
            ("host", Box::new(|f| f.host = "socks5://x.example".into())),
            ("host", Box::new(|f| f.host = "x.example:1080".into())),
            ("ports", Box::new(|f| f.ports = "0".into())),
            ("ports", Box::new(|f| f.ports = "abc".into())),
            ("ports", Box::new(|f| f.ports = " ".into())),
            ("timezone", Box::new(|f| f.timezone = "mars".into())),
            ("language", Box::new(|f| f.language = "xx".into())),
            ("routingMode", Box::new(|f| f.routing_mode = "some".into())),
            (
                "expectedIp",
                Box::new(|f| f.expected_ip = "1.2.3.0/24".into()),
            ),
        ];
        for (path, mutate) in cases {
            let mut f = form();
            mutate(&mut f);
            let mut set = before.clone();
            let errors = apply(&mut set, &f, None, false).unwrap_err();
            assert!(errors.iter().any(|e| e.path == path), "{path}: {errors:?}");
            assert_eq!(set, before);
        }
    }

    #[test]
    fn an_older_page_without_the_new_fields_still_saves() {
        let old = r#"{"host":"203.0.113.10","proto":"socks5","ports":"12324","username":"","password":null,
            "timezone":"us-east","language":"en-US","expectedIp":"","takeoverEnabled":false}"#;
        let f: ExitForm = serde_json::from_str(old).unwrap();
        let mut set = ProfileSet::default();
        apply(&mut set, &f, None, false).unwrap();
        let p = &set.agent[0];
        assert_eq!(p.routing.final_target, "agent-egress");
        assert_eq!(p.proxy_scope.mode, ProxyScopeMode::ExplicitOnly);
        assert_eq!(p.egress.relay, crate::profile::RelaySettings::default());
        assert_eq!(p.targets.len(), 3, "default targets kept");
    }

    #[test]
    fn credential_change_debug_hides_the_password() {
        let c = CredentialChange::Store {
            user: "u".into(),
            pass: "hunter2".into(),
        };
        assert!(!format!("{c:?}").contains("hunter2"));
    }

    #[test]
    fn the_page_embeds_presets_and_never_a_password() {
        let mut set = ProfileSet::default();
        let mut f = form();
        f.tunnel_enabled = true;
        f.tunnel_preset = "iproyal-cn".into();
        apply(&mut set, &f, None, false).unwrap();
        let html = page("tok", &view(&set, &input(Some("alice"))));
        assert!(html.contains("出口与分流设置") && html.contains("iproyal-cn"));
        assert!(!html.contains("s3cret"));
        for placeholder in [
            "__VIEW__",
            "__PRESETS__",
            "__CIPHERS__",
            "__SUGGESTED__",
            "__TZ__",
        ] {
            assert!(
                !html.contains(placeholder),
                "{placeholder} left in the page"
            );
        }
    }

    struct Fake(std::sync::Mutex<Vec<ExitForm>>);
    impl SetupBackend for Fake {
        fn dashboard(&self) -> Option<crate::dashboard::View> {
            Some(crate::dashboard::view(
                &ProfileSet::default(),
                "daily",
                false,
                false,
                None,
            ))
        }
        fn view(&self) -> SetupView {
            view(&ProfileSet::default(), &input(None))
        }
        fn save(&self, form: ExitForm) -> Result<(), Vec<FieldError>> {
            self.0.lock().unwrap().push(form);
            Ok(())
        }
    }

    fn send(port: u16, req: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    #[test]
    fn startup_routes_dispatch_only_valid_authenticated_changes() {
        struct Entrances(std::sync::Mutex<(bool, usize)>);
        impl SetupBackend for Entrances {
            fn view(&self) -> SetupView {
                view(&ProfileSet::default(), &input(None))
            }
            fn save(&self, _form: ExitForm) -> Result<(), Vec<FieldError>> {
                unreachable!()
            }
            fn startup(&self, enabled: bool) -> Result<(), Vec<FieldError>> {
                self.0.lock().unwrap().0 = enabled;
                Ok(())
            }
            fn shortcuts(&self) -> Result<(), Vec<FieldError>> {
                self.0.lock().unwrap().1 += 1;
                Ok(())
            }
            fn dashboard(&self) -> Option<crate::dashboard::View> {
                let mut v =
                    crate::dashboard::view(&ProfileSet::default(), "daily", false, false, None);
                v.autostart = self.0.lock().unwrap().0;
                Some(v)
            }
        }
        let backend = Arc::new(Entrances(std::sync::Mutex::new((false, 0))));
        let srv = SetupServer::start(0, "tok".into(), backend.clone()).unwrap();
        let port = srv.port;
        let post = |path: &str, body: &str| {
            send(port, &format!("POST /{path}?t=tok HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
        };
        assert!(post("startup", r#"{"enabled":true}"#).contains("\"ok\":true"));
        assert!(send(
            port,
            &format!("GET /state?t=tok HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n")
        )
        .contains("\"autostart\":true"));
        assert!(post("startup", r#"{"enabled":"false"}"#).contains("\"ok\":false"));
        assert!(backend.0.lock().unwrap().0);
        assert!(post("startup", r#"{"enabled":false}"#).contains("\"ok\":true"));
        assert!(post("shortcuts", "{}").contains("\"ok\":true"));
        assert!(post("shortcuts", r#"{"command":"must-not-run"}"#).contains("\"ok\":false"));
        assert_eq!(*backend.0.lock().unwrap(), (false, 1));
    }
    #[test]
    fn server_requires_token_host_and_origin() {
        let backend = Arc::new(Fake(Default::default()));
        let srv = SetupServer::start(0, "tok".into(), backend.clone()).unwrap();
        let port = srv.port;
        let h = format!("Host: 127.0.0.1:{port}\r\n");
        assert!(
            send(port, &format!("GET /setup?t=tok HTTP/1.1\r\n{h}\r\n")).contains("出口与分流设置")
        );
        assert!(send(port, &format!("GET /setup?t=bad HTTP/1.1\r\n{h}\r\n"))
            .starts_with("HTTP/1.1 403"));
        assert!(send(
            port,
            "GET /setup?t=tok HTTP/1.1\r\nHost: evil.example\r\n\r\n"
        )
        .starts_with("HTTP/1.1 403"));
        let body = serde_json::to_string(&form()).unwrap();
        let post = |origin: &str| {
            send(
                port,
                &format!(
                    "POST /save?t=tok HTTP/1.1\r\n{h}Origin: {origin}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                ),
            )
        };
        assert!(post("http://evil.example").starts_with("HTTP/1.1 403"));
        assert!(backend.0.lock().unwrap().is_empty());
        assert!(post(&format!("http://127.0.0.1:{port}")).contains("\"ok\":true"));
        assert_eq!(backend.0.lock().unwrap()[0].username, "alice");
        for path in ["dashboard", "state"] {
            assert!(
                send(port, &format!("GET /{path}?t=bad HTTP/1.1\r\n{h}\r\n"))
                    .starts_with("HTTP/1.1 403")
            );
            assert!(send(
                port,
                &format!("GET /{path}?t=tok HTTP/1.1\r\nHost: evil.example\r\n\r\n")
            )
            .starts_with("HTTP/1.1 403"));
            assert!(
                send(port, &format!("GET /{path}?t=tok HTTP/1.1\r\n{h}\r\n"))
                    .starts_with("HTTP/1.1 200")
            );
        }
        for path in ["rules", "path", "startup", "shortcuts"] {
            let body = r#"{"path":"direct","password":"must-not-echo"}"#;
            let post = |token: &str, origin: &str| {
                send(port,&format!("POST /{path}?t={token} HTTP/1.1\r\n{h}Origin: {origin}\r\nContent-Length: {}\r\n\r\n{body}",body.len()))
            };
            assert!(post("bad", &format!("http://127.0.0.1:{port}")).starts_with("HTTP/1.1 403"));
            assert!(post("tok", "http://evil.example").starts_with("HTTP/1.1 403"));
            assert!(send(port, &format!("POST /{path}?t=tok HTTP/1.1\r\nHost: evil.example\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}", body.len())).starts_with("HTTP/1.1 403"));
            let response = post("tok", &format!("http://127.0.0.1:{port}"));
            assert!(response.contains("表单格式不对"));
            assert!(!response.contains("must-not-echo"));
        }
    }
}
