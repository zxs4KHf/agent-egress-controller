//! First-run and later "exit and time zone" settings: what a user fills in, how it maps onto the
//! profile, and a loopback-only page to fill it in (the tray opens it in the default browser).
//!
//! The password goes from the page straight into Credential Manager; it is never written to the
//! profile, the log or the page that is sent back.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
pub const LANGUAGES: [(&str, &str); 8] = [
    ("en-US", "英语（美国）"),
    ("en-GB", "英语（英国）"),
    ("ja-JP", "日语"),
    ("ko-KR", "韩语"),
    ("de-DE", "德语"),
    ("fr-FR", "法语"),
    ("zh-CN", "简体中文"),
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
                defaults.tunnel + 2,
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
    fn cooperation_command(
        &self,
        _command: crate::cooperation::Command,
    ) -> Result<serde_json::Value, Vec<FieldError>> {
        Err(vec![err("cooperation", "COOP_UNAVAILABLE")])
    }
    fn browser_environment(
        &self,
        _command: crate::browser_environment::Command,
        _local_confirmation: bool,
    ) -> Result<serde_json::Value, Vec<FieldError>> {
        Err(vec![err("browserEnvironment", "BROWSER_UNAVAILABLE")])
    }
    fn upstream(
        &self,
        _command: crate::local_upstream::Command,
    ) -> Result<serde_json::Value, Vec<FieldError>> {
        Err(vec![err("upstream", "本机上游管理不可用")])
    }
    fn configure(&self, patch: serde_json::Value) -> Result<(), Vec<FieldError>> {
        self.save(crate::agent::patch_settings(self.view(), patch)?)
    }
    fn action(&self, _action: &str) -> Result<(), Vec<FieldError>> {
        Err(vec![err("action", "模式控制不可用")])
    }
    fn agent_access(&self, _enabled: bool) -> Result<(), Vec<FieldError>> {
        Err(vec![err("agent", "Agent 接口不可用")])
    }
    fn agent_info(&self) -> serde_json::Value {
        serde_json::json!({"enabled":false})
    }
    fn view(&self) -> SetupView;
    /// Validate and store. Errors are per field.
    fn save(&self, form: ExitForm) -> Result<(), Vec<FieldError>>;
    fn startup(&self, _enabled: bool) -> Result<(), Vec<FieldError>> {
        Err(vec![err("startup", "自启管理不可用")])
    }
    fn shortcuts(&self) -> Result<(), Vec<FieldError>> {
        Err(vec![err("shortcuts", "入口管理不可用")])
    }
    /// Launch the bounded updater; only its physical user confirmation may stop services.
    fn update(&self, _ui_only: bool) -> Result<(), Vec<FieldError>> {
        Err(vec![err("update", "更新入口不可用")])
    }
    fn update_status(&self) -> serde_json::Value {
        serde_json::json!({"status":"unknown","reason":"update_logs_unavailable","readOnly":true})
    }
    fn dashboard(&self) -> Option<crate::dashboard::View> {
        None
    }
    fn cooperation(&self) -> serde_json::Value {
        let Some(view) = self.dashboard().filter(|view| view.fixed_exit) else {
            return serde_json::json!({"available":false,"code":"COOP_FIXED_EXIT_REQUIRED","runtimeVerified":false});
        };
        let Some(port) = crate::fixed::ingress_port(&crate::core::CorePorts::default()) else {
            return serde_json::json!({"available":false,"code":"COOP_PORT_INVALID","runtimeVerified":false});
        };
        match crate::cooperation::client_script(&crate::cooperation::Selection {
            domains: view.form.sites.clone(),
            fixed_port: port,
        }) {
            Ok(script) => serde_json::json!({
                "available":true,"domains":view.form.sites,"fixedPort":port,"script":script,
                "runtimeVerified":false,"persistentInstalled":null,"installedByAec":false,"status":"export_only",
                "message":"仅生成局部订阅脚本，未修改或重载客户端；安装后仍需核对运行规则与固定身份。"
            }),
            Err(code) => serde_json::json!({"available":false,"code":code,"runtimeVerified":false}),
        }
    }
    fn rules(&self, _form: crate::dashboard::RulesForm) -> Result<(), Vec<FieldError>> {
        Err(vec![err("rules", "看板不可用")])
    }
    fn path(&self, _path: String) -> Result<(), Vec<FieldError>> {
        Err(vec![err("path", "看板不可用")])
    }
    fn route(&self, _form: crate::route_switch::Form) -> Result<(), Vec<FieldError>> {
        Err(vec![err("route", "快捷路由切换不可用")])
    }
}

pub struct SetupServer {
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct ClientLease(Arc<AtomicUsize>);
impl Drop for ClientLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn respond(s: &mut TcpStream, status: &str, ctype: &str, body: &str) {
    let _ = write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: SAMEORIGIN\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
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
    crate::ui::template("setup.html", PAGE)
        .replace("__TOKEN__", token)
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

fn handle(
    mut s: TcpStream,
    port: u16,
    token: &str,
    backend: &dyn SetupBackend,
    agent: Option<&crate::agent::Auth>,
) {
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
    if let Some(auth) = agent {
        if header(&head, "authorization") != Some(format!("Bearer {}", auth.token).as_str())
            || header(&head, "origin").is_some()
            || header(&head, "transfer-encoding").is_some()
            || !header(&head, "content-type").is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
            })
            || !auth.enabled()
        {
            return respond(
                &mut s,
                "403 Forbidden",
                "application/json",
                "{\"error\":\"access denied\"}",
            );
        }
        let len = header(&head, "content-length").and_then(|v| v.parse::<usize>().ok());
        if method != "POST" || target != "/api/v1/call" || len.is_none() || len.unwrap() > MAX_BODY
        {
            return respond(
                &mut s,
                "400 Bad Request",
                "application/json",
                "{\"error\":\"expected POST /api/v1/call with Content-Length\"}",
            );
        }
        let len = len.unwrap();
        while buf.len() < head_end + len {
            match s.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let result = serde_json::from_slice::<crate::agent::Call>(&buf[head_end..head_end + len])
            .map_err(|_| "invalid request".to_string())
            .and_then(|call| crate::agent::call(backend, call));
        buf.fill(0);
        let body = match result {
            Ok(value) => serde_json::json!({"ok":true,"result":value}),
            Err(error) => serde_json::json!({"ok":false,"error":error}),
        };
        return respond(
            &mut s,
            "200 OK",
            "application/json; charset=utf-8",
            &body.to_string(),
        );
    }
    match (method, path) {
        ("GET", "/cooperation") if token_ok => respond(
            &mut s,
            "200 OK",
            "application/json; charset=utf-8",
            &backend.cooperation().to_string(),
        ),
        ("GET", "/app") if token_ok => respond(
            &mut s,
            "200 OK",
            "text/html; charset=utf-8",
            &control_page(port, token, query),
        ),
        ("GET", "/agent") if token_ok => respond(
            &mut s,
            "200 OK",
            "text/html; charset=utf-8",
            &crate::agent::page(token, &backend.agent_info()),
        ),
        ("GET", "/agent-info") if token_ok => respond(
            &mut s,
            "200 OK",
            "application/json; charset=utf-8",
            &backend.agent_info().to_string(),
        ),
        ("GET", asset) if token_ok && asset.starts_with("/ui/") => {
            match crate::ui::asset(&asset[4..]) {
                Some((ctype, body)) => respond(&mut s, "200 OK", ctype, &body),
                None => respond(&mut s, "404 Not Found", "text/plain", "asset unavailable"),
            }
        }
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
        ("GET", "/proxy-help") if token_ok => respond(
            &mut s,
            "200 OK",
            "text/html; charset=utf-8",
            &proxy_help_page(token),
        ),
        (
            "POST",
            "/save"
            | "/rules"
            | "/path"
            | "/startup"
            | "/shortcuts"
            | "/action"
            | "/agent-access"
            | "/update"
            | "/route"
            | "/upstream"
            | "/cooperation"
            | "/browser-environment",
        ) if token_ok => {
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
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct UpdateForm {
                ui_only: bool,
            }
            let malformed = || vec![err("form", "表单格式不对")];
            if path == "/upstream" || path == "/browser-environment" || path == "/cooperation" {
                let result = if path == "/cooperation" {
                    serde_json::from_slice(body)
                        .map_err(|_| malformed())
                        .and_then(|command| backend.cooperation_command(command))
                } else if path == "/browser-environment" {
                    serde_json::from_slice(body)
                        .map_err(|_| malformed())
                        .and_then(|command| backend.browser_environment(command, true))
                } else {
                    serde_json::from_slice(body)
                        .map_err(|_| malformed())
                        .and_then(|command| backend.upstream(command))
                };
                buf.iter_mut().for_each(|b| *b = 0);
                let reply = match result {
                    Ok(result) => serde_json::json!({"ok":true,"result":result}),
                    Err(errors) => serde_json::json!({"ok":false,"errors":errors}),
                };
                return respond(
                    &mut s,
                    "200 OK",
                    "application/json; charset=utf-8",
                    &reply.to_string(),
                );
            }
            let result = match path {
                "/update" => serde_json::from_slice::<UpdateForm>(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.update(form.ui_only)),
                "/agent-access" => serde_json::from_slice::<StartupForm>(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.agent_access(form.enabled)),
                "/action" => serde_json::from_slice::<PathForm>(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.action(&form.path)),
                "/startup" => serde_json::from_slice::<StartupForm>(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.startup(form.enabled)),
                "/shortcuts" => serde_json::from_slice::<EmptyForm>(body)
                    .map_err(|_| malformed())
                    .and_then(|_| backend.shortcuts()),
                "/rules" => serde_json::from_slice(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.rules(form)),
                "/route" => serde_json::from_slice(body)
                    .map_err(|_| malformed())
                    .and_then(|form| backend.route(form)),
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
            "/app"
            | "/setup"
            | "/proxy-help"
            | "/save"
            | "/dashboard"
            | "/state"
            | "/rules"
            | "/path"
            | "/startup"
            | "/shortcuts"
            | "/agent"
            | "/agent-access"
            | "/action"
            | "/agent-info"
            | "/cooperation"
            | "/update"
            | "/route"
            | "/upstream"
            | "/browser-environment",
        ) => respond(&mut s, "403 Forbidden", "text/plain", "token rejected"),
        _ => respond(&mut s, "404 Not Found", "text/plain", "not found"),
    }
}

impl SetupServer {
    pub fn control_url(&self) -> String {
        format!("http://127.0.0.1:{}/app?t={}", self.port, self.token)
    }

    pub fn control_tab_url(&self, tab: &str) -> String {
        format!("{}&tab={}", self.control_url(), tab)
    }

    pub fn dashboard_url(&self) -> String {
        format!("http://127.0.0.1:{}/dashboard?t={}", self.port, self.token)
    }
    pub fn proxy_help_url(&self) -> String {
        format!("http://127.0.0.1:{}/proxy-help?t={}", self.port, self.token)
    }
    /// Listen on 127.0.0.1:`port` (0 picks a free port).
    pub fn start(
        port: u16,
        token: String,
        backend: Arc<dyn SetupBackend>,
    ) -> std::io::Result<Self> {
        Self::start_inner(port, token, backend, None)
    }

    pub(crate) fn start_agent(
        backend: Arc<dyn SetupBackend>,
        auth: crate::agent::Auth,
    ) -> std::io::Result<Self> {
        Self::start_inner(0, String::new(), backend, Some(auth))
    }

    fn start_inner(
        port: u16,
        token: String,
        backend: Arc<dyn SetupBackend>,
        agent: Option<crate::agent::Auth>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (stop2, token2) = (stop.clone(), Arc::new(token.clone()));
        let agent = Arc::new(agent);
        let clients = Arc::new(AtomicUsize::new(0));
        let thread = std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, _)) => {
                        let _ = s.set_nonblocking(false);
                        let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
                        if clients.fetch_add(1, Ordering::Relaxed) >= 32 {
                            clients.fetch_sub(1, Ordering::Relaxed);
                            let mut s = s;
                            respond(
                                &mut s,
                                "503 Service Unavailable",
                                "text/plain",
                                "too many local requests",
                            );
                            continue;
                        }
                        let lease = ClientLease(clients.clone());
                        let (token, backend, agent) =
                            (token2.clone(), backend.clone(), agent.clone());
                        std::thread::spawn(move || {
                            let _lease = lease;
                            handle(s, port, &token, backend.as_ref(), agent.as_ref().as_ref());
                        });
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
    pub fn port(&self) -> u16 {
        self.port
    }
}

fn control_page(port: u16, token: &str, query: &str) -> String {
    let tab = query
        .split('&')
        .find_map(|part| part.strip_prefix("tab="))
        .unwrap_or("setup");
    let tabs = ["setup", "dashboard", "agent", "help"];
    let config = serde_json::json!({
        "pages": {
            "setup": format!("http://127.0.0.1:{port}/setup?t={token}"),
            "dashboard": format!("http://127.0.0.1:{port}/dashboard?t={token}"),
            "agent": format!("http://127.0.0.1:{port}/agent?t={token}"),
            "help": format!("http://127.0.0.1:{port}/proxy-help?t={token}")
        },
        "stateUrl": format!("http://127.0.0.1:{port}/state?t={token}"),
        "agentInfoUrl": format!("http://127.0.0.1:{port}/agent-info?t={token}"),
        "tab": if tabs.contains(&tab) { tab } else { "setup" }
    });
    crate::ui::template(
        "control.html",
        include_str!("../../../ui/revisions/fluent-2/control.html"),
    )
    .replace("__TOKEN__", token)
    .replace("__CONFIG__", &config.to_string().replace("</", "<\\/"))
}

impl Drop for SetupServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn proxy_help_page(token: &str) -> String {
    crate::ui::template("help.html", PROXY_HELP_PAGE).replace("__TOKEN__", token)
}

const PROXY_HELP_PAGE: &str = include_str!("../../../ui/revisions/fluent-2/help.html");

const PAGE: &str = include_str!("../../../ui/revisions/fluent-2/setup.html");

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
            "-1", "65536", "no-port", "17909", "17553", "17325", "17326", "17327", "17328",
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
    fn slow_mutation_does_not_block_page_navigation_or_status_reads() {
        struct Slow {
            entered: std::sync::mpsc::Sender<()>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl SetupBackend for Slow {
            fn view(&self) -> SetupView {
                view(&ProfileSet::default(), &input(None))
            }
            fn save(&self, _form: ExitForm) -> Result<(), Vec<FieldError>> {
                unreachable!()
            }
            fn dashboard(&self) -> Option<crate::dashboard::View> {
                Some(crate::dashboard::view(
                    &ProfileSet::default(),
                    "daily",
                    true,
                    false,
                    None,
                ))
            }
            fn path(&self, _path: String) -> Result<(), Vec<FieldError>> {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                Ok(())
            }
        }
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = SetupServer::start(
            0,
            "tok".into(),
            Arc::new(Slow {
                entered: entered_tx,
                release: std::sync::Mutex::new(release_rx),
            }),
        )
        .unwrap();
        let port = server.port();
        let mutation = std::thread::spawn(move || {
            let body = r#"{"path":"tunnel-2"}"#;
            send(port, &format!("POST /path?t=tok HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut reader = TcpStream::connect(("127.0.0.1", port)).unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(700)))
            .unwrap();
        write!(
            reader,
            "GET /state?t=tok HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        let _ = reader.read_to_string(&mut response);
        release_tx.send(()).unwrap();
        assert!(mutation.join().unwrap().contains("\"ok\":true"));
        assert!(
            response.contains("200 OK") && response.contains("\"busy\":true"),
            "read requests were blocked behind the mutation: {response}"
        );
    }

    #[test]
    fn agent_toggle_and_update_require_authentication_and_strict_arguments() {
        struct Controls(std::sync::Mutex<(bool, Vec<bool>)>);
        impl SetupBackend for Controls {
            fn view(&self) -> SetupView {
                view(&ProfileSet::default(), &input(None))
            }
            fn save(&self, _form: ExitForm) -> Result<(), Vec<FieldError>> {
                unreachable!()
            }
            fn agent_info(&self) -> serde_json::Value {
                serde_json::json!({"enabled":self.0.lock().unwrap().0})
            }
            fn agent_access(&self, enabled: bool) -> Result<(), Vec<FieldError>> {
                self.0.lock().unwrap().0 = enabled;
                Ok(())
            }
            fn update(&self, ui_only: bool) -> Result<(), Vec<FieldError>> {
                self.0.lock().unwrap().1.push(ui_only);
                Ok(())
            }
        }
        let backend = Arc::new(Controls(std::sync::Mutex::new((false, Vec::new()))));
        let server = SetupServer::start(0, "tok".into(), backend.clone()).unwrap();
        let port = server.port();
        let post = |path: &str, token: &str, origin: &str, body: &str| {
            send(port,&format!("POST /{path}?t={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {origin}\r\nContent-Length: {}\r\n\r\n{body}",body.len()))
        };
        let origin = format!("http://127.0.0.1:{port}");
        for enabled in [true, false, true, false] {
            assert!(post(
                "agent-access",
                "tok",
                &origin,
                &format!("{{\"enabled\":{enabled}}}")
            )
            .contains("\"ok\":true"));
            assert!(send(
                port,
                &format!("GET /agent-info?t=tok HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n")
            )
            .contains(&format!("\"enabled\":{enabled}")));
        }
        assert!(post("update", "bad", &origin, r#"{"uiOnly":false}"#).contains("403 Forbidden"));
        assert!(post(
            "update",
            "tok",
            "http://foreign.example",
            r#"{"uiOnly":false}"#
        )
        .contains("403 Forbidden"));
        assert!(post(
            "update",
            "tok",
            &origin,
            r#"{"uiOnly":false,"confirmCore":true}"#
        )
        .contains("\"ok\":false"));
        assert!(send(
            port,
            &format!("GET /agent-info?t=bad HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n")
        )
        .contains("403 Forbidden"));
        assert!(backend.0.lock().unwrap().1.is_empty());
        for ui_only in [true, false] {
            assert!(post(
                "update",
                "tok",
                &origin,
                &format!("{{\"uiOnly\":{ui_only}}}")
            )
            .contains("\"ok\":true"));
        }
        assert_eq!(backend.0.lock().unwrap().1, vec![true, false]);
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
        let app = send(
            port,
            &format!("GET /app?t=tok&tab=agent HTTP/1.1\r\n{h}\r\n"),
        );
        assert!(app.starts_with("HTTP/1.1 200"));
        assert!(app.contains("X-Frame-Options: SAMEORIGIN"));
        assert!(app.contains("Agent Egress Controller"));
        assert!(app.contains("http://127.0.0.1:") && app.contains("agent?t=tok"));
        assert!(!app.contains("__CONFIG__"));
        assert!(
            send(port, &format!("GET /app?t=bad HTTP/1.1\r\n{h}\r\n")).starts_with("HTTP/1.1 403")
        );
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
        for path in ["dashboard", "state", "proxy-help"] {
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
        for path in ["rules", "route", "path", "startup", "shortcuts"] {
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

    #[test]
    fn agent_http_requires_credential_and_never_accepts_browser_origin() {
        let data = std::env::temp_dir().join(format!("aec-agent-auth-{}", std::process::id()));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join(crate::agent::ACCESS), b"true").unwrap();
        let backend: Arc<dyn SetupBackend> = Arc::new(Fake(Default::default()));
        let server = SetupServer::start_inner(
            0,
            String::new(),
            backend,
            Some(crate::agent::Auth {
                token: "private-token".into(),
                data: data.clone(),
            }),
        )
        .unwrap();
        let port = server.port;
        let body = r#"{"name":"aec_state","arguments":{}}"#;
        let request = |auth: &str, origin: Option<&str>, content_type: &str| {
            let origin = origin.map_or(String::new(), |v| format!("Origin: {v}\r\n"));
            send(port,&format!("POST /api/v1/call HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: {auth}\r\n{origin}Content-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",body.len()))
        };
        assert!(request("Bearer wrong", None, "application/json").starts_with("HTTP/1.1 403"));
        assert!(request(
            "Bearer private-token",
            Some("http://127.0.0.1"),
            "application/json"
        )
        .starts_with("HTTP/1.1 403"));
        assert!(request("Bearer private-token", None, "text/plain").starts_with("HTTP/1.1 403"));
        assert!(request("Bearer private-token", None, "application/json").contains("\"ok\":true"));
        let update_query = serde_json::json!({"name":"aec_update_status","arguments":{}});
        assert_eq!(
            crate::agent::client(port, "private-token", &update_query).unwrap()["readOnly"],
            true
        );
        assert!(crate::agent::client(port, "wrong", &update_query).is_err());
        std::fs::write(data.join(crate::agent::ACCESS), b"false").unwrap();
        assert!(crate::agent::client(port, "private-token", &update_query).is_err());
        assert!(
            request("Bearer private-token", None, "application/json").starts_with("HTTP/1.1 403")
        );
        drop(server);
        let _ = std::fs::remove_dir_all(data);
    }
}
