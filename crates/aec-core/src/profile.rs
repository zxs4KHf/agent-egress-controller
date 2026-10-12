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
    /// The values the legacy chain runs with (startup-egress.ps1).
    fn default() -> Self {
        Self {
            connect_timeout_ms: 1200,
            dial_budget_ms: 4000,
            cooldown_ms: 3000,
            max_inflight: 3,
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

/// Where everything an Agent entry sends that is not a target domain goes.
///
/// Deliberately a named upstream rather than `DIRECT`. A fallback that resolves to a direct
/// connection stops being a fallback the moment the route to a domain is blocked: the request is
/// then made from this machine with this machine's address, which is a different failure that
/// looks like a working link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FallbackUpstream {
    pub host: String,
    pub port: u16,
    pub proto: UpstreamProto,
    #[serde(default)]
    pub credential_ref: Option<String>,
}

/// `default-v1`: everything goes to the target group.
pub const TEMPLATE_DEFAULT: &str = "default-v1";
/// `split-v1`: the target domains go to the target group, everything else to the fallback.
pub const TEMPLATE_SPLIT: &str = "split-v1";
pub const RULES_TEMPLATES: [&str; 2] = [TEMPLATE_DEFAULT, TEMPLATE_SPLIT];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Routing {
    pub target_group: String,
    pub rules_template: String,
    /// Required by [`TEMPLATE_SPLIT`]; ignored by [`TEMPLATE_DEFAULT`].
    #[serde(default)]
    pub fallback: Option<FallbackUpstream>,
}

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
                },
                proxy_scope: ProxyScope {
                    mode: ProxyScopeMode::ExplicitOnly,
                    bypass: vec!["<-loopback>".into(), "127.0.0.1".into(), "localhost".into()],
                },
                routing: Routing {
                    target_group: "agent-egress".into(),
                    // Everything an Agent entry opens goes through the target group. That needs no
                    // second upstream, so the only settings gap left is the egress candidate itself
                    // and nothing here has to be guessed from the machine it was written on.
                    rules_template: TEMPLATE_DEFAULT.into(),
                    fallback: None,
                },
                locale: Locale {
                    timezone: TimezoneTarget {
                        windows_id: "Eastern Standard Time".into(),
                        iana: "America/New_York".into(),
                    },
                    browser_lang: "en-US".into(),
                    accept_language: "en-US,en".into(),
                },
                targets: ["claude.ai", "www.anthropic.com", "chatgpt.com"]
                    .iter()
                    .map(|d| Target {
                        domain: (*d).into(),
                        trace: true,
                    })
                    .collect(),
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
    if p.routing.target_group.trim().is_empty() {
        e.push(format!("{at}.routing.targetGroup"), "must not be empty");
    }
    if !RULES_TEMPLATES.contains(&p.routing.rules_template.as_str()) {
        e.push(
            format!("{at}.routing.rulesTemplate"),
            format!("must be one of {}", RULES_TEMPLATES.join(", ")),
        );
    }
    match &p.routing.fallback {
        Some(f) => {
            let fp = format!("{at}.routing.fallback");
            if f.host.trim().is_empty() {
                e.push(format!("{fp}.host"), "must not be empty");
            }
            if f.port == 0 {
                e.push(format!("{fp}.port"), "must be 1-65535");
            }
            if matches!(f.credential_ref.as_deref(), Some(r) if r.trim().is_empty()) {
                e.push(
                    format!("{fp}.credentialRef"),
                    "must be omitted or non-empty",
                );
            }
        }
        None => {
            if p.routing.rules_template == TEMPLATE_SPLIT {
                e.push(
                    format!("{at}.routing.fallback"),
                    format!("is required by '{TEMPLATE_SPLIT}': without it, non-target traffic has no upstream"),
                );
            }
        }
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

    fn fallback() -> FallbackUpstream {
        FallbackUpstream {
            host: "203.0.113.10".into(),
            port: 1080,
            proto: UpstreamProto::Socks5,
            credential_ref: None,
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
        assert_eq!(s.agent[0].routing.rules_template, TEMPLATE_DEFAULT);
        assert!(s.agent[0].routing.fallback.is_none());
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
                "agent[0].routing.rulesTemplate",
                Box::new(|s| s.agent[0].routing.rules_template = "v9".into()),
            ),
            (
                "agent[0].routing.fallback",
                Box::new(|s| {
                    s.agent[0].routing.rules_template = TEMPLATE_SPLIT.into();
                    s.agent[0].routing.fallback = None;
                }),
            ),
            (
                "agent[0].routing.fallback.host",
                Box::new(|s| {
                    s.agent[0].routing.fallback = Some(fallback());
                    s.agent[0].routing.fallback.as_mut().unwrap().host = " ".into();
                }),
            ),
            (
                "agent[0].routing.fallback.port",
                Box::new(|s| {
                    s.agent[0].routing.fallback = Some(fallback());
                    s.agent[0].routing.fallback.as_mut().unwrap().port = 0;
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
    fn only_the_split_template_needs_a_fallback() {
        let mut s = valid();
        s.agent[0].routing.rules_template = TEMPLATE_DEFAULT.into();
        s.agent[0].routing.fallback = None;
        assert!(validate(&s).is_empty());
    }

    #[test]
    fn accept_language_with_quality_values_is_valid() {
        let mut s = valid();
        s.agent[0].locale.accept_language = "en-US,en;q=0.9".into();
        assert!(validate(&s).is_empty());
    }
}
