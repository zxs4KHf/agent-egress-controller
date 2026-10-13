//! Environment checks G1, G2, G3, R1, R2, R9 (SPEC 4.5). Pure logic over an [`HttpClient`] and a
//! [`CoreObserver`], so every rule is testable without a network. An unknown never counts as a pass.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::connlog::ConnRecord;
use crate::http::{HttpClient, HttpError, Request, Response, Route};
use crate::profile::{AgentProfile, DetectionMode, Target};
use crate::providers::{
    parse_cf_trace, parse_ipsb, parse_ipwhois, Geo, Parser, ProviderSet, Purpose,
};

pub const LATENCY_WARN: Duration = Duration::from_millis(1500);
const LATENCY_SAMPLES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Decides readiness.
    Hard,
    /// Shown only; pass, warn or unknown.
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub source: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: String,
    pub kind: Kind,
    pub status: Status,
    /// Short machine-readable cause for anything but a plain pass, e.g. `same_as_direct`.
    pub reason: Option<String>,
    pub evidence: Vec<Evidence>,
}

impl CheckResult {
    pub(crate) fn new(
        id: impl Into<String>,
        kind: Kind,
        status: Status,
        reason: Option<&str>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            status,
            reason: reason.map(str::to_string),
            evidence: Vec::new(),
        }
    }
    pub(crate) fn with(mut self, source: impl Into<String>, detail: impl Into<String>) -> Self {
        self.evidence.push(Evidence {
            source: source.into(),
            detail: detail.into(),
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub generated_at: String,
    pub mode: DetectionMode,
    pub proxy_port: u16,
    pub results: Vec<CheckResult>,
    /// The exit address G1 settled on, if any; R3 compares WebRTC's public address with it.
    #[serde(default)]
    pub exit_ip: Option<String>,
}

impl CheckReport {
    /// All hard checks in this report passed. Readiness also needs the checks that later tickets
    /// add (G3–G5); a missing check is not a pass, the caller decides which ones must be present.
    pub fn hard_checks_pass(&self) -> bool {
        let hard: Vec<_> = self
            .results
            .iter()
            .filter(|r| r.kind == Kind::Hard)
            .collect();
        !hard.is_empty() && hard.iter().all(|r| r.status == Status::Pass)
    }
}

/// What the core under test reports about itself. Only available for this application's own core;
/// a proxy observed from outside has no observer, and G3 is then unknown.
pub trait CoreObserver {
    /// Position in the connection log; records after it belong to this check run.
    fn mark(&self) -> u64;
    fn records_since(&self, mark: u64) -> Result<Vec<ConnRecord>, String>;
    fn dns_listening(&self) -> Result<bool, String>;
}

pub struct CheckInput<'a> {
    pub profile: &'a AgentProfile,
    pub providers: &'a ProviderSet,
    /// The mixed port of the core under test.
    pub proxy_port: u16,
    /// Whether the core answered; when it did not, nothing proves traffic went through it.
    pub core: Result<(), String>,
    pub observer: Option<&'a dyn CoreObserver>,
    /// DNS servers of the active network adapters, shown with G3.
    pub system_dns: Result<Vec<String>, String>,
    pub generated_at: String,
}

/// Language tags of an Accept-Language value, without quality weights, lower-cased.
fn language_tags(v: &str) -> Vec<String> {
    v.split(',')
        .map(|t| {
            t.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// `(type, address)` of WebRTC ICE candidate lines.
fn candidate_parts(c: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = c.trim_start_matches("a=").split_whitespace().collect();
    let typ = parts
        .iter()
        .position(|p| *p == "typ")
        .and_then(|i| parts.get(i + 1))?;
    Some((typ, parts.get(4)?))
}

/// G5, R3, R4, R5 for one browser entry (SPEC 4.5), from the local check page's report.
/// Returns the results and the browser's Intl time zone for G4.
pub fn browser_checks(
    profile: &AgentProfile,
    entry: &str,
    signals: Option<&crate::checkpage::BrowserSignals>,
    exit_ip: Option<&str>,
) -> (Vec<CheckResult>, Option<String>) {
    let id = |p: &str| format!("{p}:{entry}");
    let Some(s) = signals else {
        let why = Some("no_report");
        let r = |p: &str, k| {
            CheckResult::new(id(p), k, Status::Unknown, why)
                .with("check page", "the browser did not report back in time")
        };
        return (
            vec![
                r("G5", Kind::Hard),
                r("R3", Kind::ReadOnly),
                r("R4", Kind::ReadOnly),
                r("R5", Kind::ReadOnly),
            ],
            None,
        );
    };
    let local_only = profile.detection.mode == DetectionMode::LocalOnly;

    // G5: what the page sees and what the server received must both match the profile.
    let want_lang = &profile.locale.browser_lang;
    let want_tags = language_tags(&profile.locale.accept_language);
    let lang_ok = s
        .language
        .as_deref()
        .is_some_and(|l| l.eq_ignore_ascii_case(want_lang));
    let header_tags = s.accept_language_header.as_deref().map(language_tags);
    let g5 = match (&header_tags, lang_ok) {
        (None, _) => CheckResult::new(
            id("G5"),
            Kind::Hard,
            Status::Unknown,
            Some("no_accept_language_header"),
        ),
        (Some(_), false) => CheckResult::new(
            id("G5"),
            Kind::Hard,
            Status::Fail,
            Some("language_mismatch"),
        ),
        (Some(h), true) if *h != want_tags => CheckResult::new(
            id("G5"),
            Kind::Hard,
            Status::Fail,
            Some("accept_language_mismatch"),
        ),
        _ => CheckResult::new(id("G5"), Kind::Hard, Status::Pass, None),
    }
    .with(
        "page",
        format!(
            "navigator.language={} languages={}",
            s.language.as_deref().unwrap_or("?"),
            s.languages.join(",")
        ),
    )
    .with(
        "server",
        format!(
            "Accept-Language: {}",
            s.accept_language_header.as_deref().unwrap_or("(none)")
        ),
    )
    .with(
        "profile",
        format!(
            "browserLang={want_lang} acceptLanguage={}",
            profile.locale.accept_language
        ),
    );

    // R3: does WebRTC reveal a public address other than the exit?
    let parsed: Vec<(&str, &str)> = s
        .candidates
        .iter()
        .filter_map(|c| candidate_parts(c))
        .collect();
    let public: Vec<&str> = parsed
        .iter()
        .filter(|(t, _)| *t == "srflx" || *t == "prflx")
        .map(|(_, a)| *a)
        .collect();
    let hosts = parsed.iter().filter(|(t, _)| *t == "host").count();
    let r3 = if local_only {
        CheckResult::new(id("R3"), Kind::ReadOnly, Status::Unknown, Some("local_only"))
    } else if public.is_empty() {
        // No public candidate proves nothing: the STUN server may simply be unreachable from
        // here, so this is unknown, not a pass (observed 2026-10-11 with Google STUN).
        CheckResult::new(id("R3"), Kind::ReadOnly, Status::Unknown, Some("no_public_candidate")).with(
            "note",
            "WebRTC produced no public address. Either UDP to the STUN server is blocked or unreachable, or nothing is revealed; the two cannot be told apart",
        )
    } else {
        match exit_ip {
            None => CheckResult::new(id("R3"), Kind::ReadOnly, Status::Unknown, Some("no_exit_ip")),
            Some(exit) if public.iter().all(|a| *a == exit) => CheckResult::new(id("R3"), Kind::ReadOnly, Status::Pass, None),
            Some(_) => CheckResult::new(id("R3"), Kind::ReadOnly, Status::Warn, Some("webrtc_other_address")).with(
                "hint",
                "WebRTC UDP does not go through the HTTP proxy. The entry's extraArgs may add --force-webrtc-ip-handling-policy=disable_non_proxied_udp (the profile's choice; default off)",
            ),
        }
    }
    .with("webrtc", format!("{} public candidate(s): {}; {hosts} host candidate(s)", public.len(), public.join(", ")))
    .with("exit", exit_ip.unwrap_or("unknown").to_string());

    // R4: shown, never judged.
    let r4 = if s.canvas_sha256.is_none() && s.webgl_renderer.is_none() {
        CheckResult::new(
            id("R4"),
            Kind::ReadOnly,
            Status::Unknown,
            Some("not_readable"),
        )
    } else {
        CheckResult::new(id("R4"), Kind::ReadOnly, Status::Pass, None)
    }
    .with(
        "canvas",
        s.canvas_sha256
            .clone()
            .unwrap_or_else(|| "unreadable".into()),
    )
    .with(
        "webgl",
        format!(
            "{} / {}",
            s.webgl_vendor.as_deref().unwrap_or("?"),
            s.webgl_renderer.as_deref().unwrap_or("?")
        ),
    );

    let r5 = match s.cookie_enabled {
        Some(true) => CheckResult::new(id("R5"), Kind::ReadOnly, Status::Pass, None),
        Some(false) => CheckResult::new(
            id("R5"),
            Kind::ReadOnly,
            Status::Warn,
            Some("cookies_disabled"),
        ),
        None => CheckResult::new(
            id("R5"),
            Kind::ReadOnly,
            Status::Unknown,
            Some("not_readable"),
        ),
    }
    .with(
        "page",
        format!(
            "navigator.cookieEnabled={}",
            s.cookie_enabled
                .map_or("unreadable".into(), |b| b.to_string())
        ),
    );

    (vec![g5, r3, r4, r5], s.timezone.clone())
}

/// What G4 needs from the state machine rather than from the network.
pub struct TimezoneContext {
    /// Current system time zone (Windows id).
    pub current: Result<String, String>,
    /// The time zone item of the takeover gate was open when Agent mode was entered.
    pub timezone_writable: bool,
    /// This application set the time zone during this session (a real, not dry-run, change).
    pub changed_by_us: bool,
    /// Foreign writers seen when entering; non-empty means someone else may be keeping it.
    pub foreign_writers: Vec<String>,
    /// The process token holds SeTimeZonePrivilege.
    pub privilege: Result<bool, String>,
    /// `(entry, IANA zone)` as reported by each Agent browser's check page.
    pub browser_timezones: Vec<(String, String)>,
}

/// G4 (hard) and R10 time zone ownership (read-only), SPEC 4.5.
pub fn timezone_checks(profile: &AgentProfile, ctx: &TimezoneContext) -> Vec<CheckResult> {
    let target = &profile.locale.timezone.windows_id;
    let privilege = match &ctx.privilege {
        Ok(true) => "SeTimeZonePrivilege held by this process".to_string(),
        Ok(false) => {
            "SeTimeZonePrivilege NOT held: this process could not change the time zone".to_string()
        }
        Err(e) => format!("privilege unreadable: {e}"),
    };
    let current = match &ctx.current {
        Err(e) => {
            return vec![
                CheckResult::new("G4", Kind::Hard, Status::Unknown, Some("unreadable"))
                    .with("system", e.clone()),
                CheckResult::new("R10", Kind::ReadOnly, Status::Unknown, Some("unreadable")),
            ]
        }
        Ok(c) => c,
    };
    let seen = format!("system time zone {current}, profile wants {target}");
    if current != target {
        let g4 = if ctx.timezone_writable {
            CheckResult::new("G4", Kind::Hard, Status::Fail, Some("mismatch"))
        } else {
            CheckResult::new(
                "G4",
                Kind::Hard,
                Status::Unknown,
                Some("global_state_not_taken_over"),
            )
            .with(
                "gate",
                "the time zone is outside this session's takeover, so this application did not change it",
            )
        };
        return vec![
            g4.with("system", seen).with("token", privilege),
            CheckResult::new(
                "R10",
                Kind::ReadOnly,
                Status::Unknown,
                Some("not_at_target"),
            ),
        ];
    }
    let mut g4 = CheckResult::new("G4", Kind::Hard, Status::Pass, None)
        .with("system", seen)
        .with("token", privilege);
    let iana = &profile.locale.timezone.iana;
    for (entry, tz) in &ctx.browser_timezones {
        g4 = g4.with(format!("browser {entry}"), format!("Intl time zone {tz}"));
        if tz != iana && g4.status == Status::Pass {
            g4.status = Status::Fail;
            g4.reason = Some("browser_timezone_mismatch".into());
        }
    }
    let r10 = if ctx.changed_by_us {
        CheckResult::new("R10", Kind::ReadOnly, Status::Pass, None)
            .with("journal", "set by this application for this session")
    } else if ctx.foreign_writers.is_empty() {
        CheckResult::new(
            "R10",
            Kind::ReadOnly,
            Status::Pass,
            Some("already_at_target"),
        )
        .with("journal", "was already at the target before entering")
    } else {
        let mut r = CheckResult::new(
            "R10",
            Kind::ReadOnly,
            Status::Warn,
            Some("maintained_externally"),
        )
        .with(
            "journal",
            "not set by this application; another program may change it at any time",
        );
        for w in &ctx.foreign_writers {
            r = r.with("foreign writer", w.clone());
        }
        r
    };
    vec![g4, r10]
}

fn system_dns_line(r: &Result<Vec<String>, String>) -> String {
    match r {
        Ok(v) if v.is_empty() => "none configured".into(),
        Ok(v) => v.join(", "),
        Err(e) => format!("unreadable: {e}"),
    }
}

/// G3: every target reached the core by name and left through the profile's group.
fn dns_path(input: &CheckInput, mark: Option<u64>) -> CheckResult {
    let r = match (input.observer, mark) {
        (Some(o), Some(m)) => match (o.dns_listening(), o.records_since(m)) {
            (Ok(false), _) => {
                CheckResult::new("G3", Kind::Hard, Status::Fail, Some("dns_not_listening"))
            }
            (Err(e), _) => {
                CheckResult::new("G3", Kind::Hard, Status::Unknown, Some("dns_probe_failed"))
                    .with("core", e)
            }
            (Ok(true), Err(e)) => CheckResult::new(
                "G3",
                Kind::Hard,
                Status::Unknown,
                Some("core_log_unreadable"),
            )
            .with("core log", e),
            (Ok(true), Ok(records)) => {
                let mut evidence = Vec::new();
                let (mut wrong, mut missing, mut uncertain) = (Vec::new(), Vec::new(), Vec::new());
                for t in &input.profile.targets {
                    let host = t.probe_domain();
                    let group = if crate::fixed::protects(input.profile, host) {
                        Some("relay")
                    } else {
                        input.profile.routing.target_for_domain(host)
                    };
                    if group.is_none() {
                        uncertain.push(host.to_string());
                    }
                    let seen: Vec<&ConnRecord> = records
                        .iter()
                        .filter(|c| c.host.eq_ignore_ascii_case(host))
                        .collect();
                    match seen.iter().find(|c| {
                        group.is_some_and(|g| c.group != g)
                            || c.group == "DIRECT"
                            || c.group == "REJECT"
                            || c.via.as_deref() == Some("DIRECT")
                    }) {
                        _ if seen.is_empty() => missing.push(host.to_string()),
                        Some(bad) => wrong.push(format!(
                            "{} left through {} (rule {})",
                            host, bad.group, bad.rule
                        )),
                        None => {
                            let c = seen[0];
                            let dial = if seen.iter().any(|c| c.error.is_some()) {
                                "; the core could not reach it (see G2)"
                            } else {
                                ""
                            };
                            evidence.push(format!(
                                "{} handed over by name, routed via {}[{}] by rule {}{dial}",
                                host,
                                c.group,
                                c.via.as_deref().unwrap_or("-"),
                                c.rule
                            ));
                        }
                    }
                }
                let addresses = records.iter().filter(|c| c.host_is_address()).count();
                let mut r = if !wrong.is_empty() {
                    CheckResult::new("G3", Kind::Hard, Status::Fail, Some("wrong_group"))
                } else if !missing.is_empty() {
                    CheckResult::new("G3", Kind::Hard, Status::Fail, Some("no_domain_record"))
                } else if !uncertain.is_empty() {
                    CheckResult::new(
                        "G3",
                        Kind::Hard,
                        Status::Unknown,
                        Some("routing_evidence_incomplete"),
                    )
                } else {
                    CheckResult::new("G3", Kind::Hard, Status::Pass, None)
                };
                for w in wrong {
                    r = r.with("core log", w);
                }
                for m in missing {
                    r = r.with("core log", format!("no connection to {m} by name: the client did not hand the host name to the core"));
                }
                for e in evidence {
                    r = r.with("core log", e);
                }
                for domain in uncertain {
                    r = r.with("routing", format!("{domain}: earlier geodata/IP rule cannot be evaluated from domain evidence alone"));
                }
                if addresses > 0 {
                    r = r.with(
                        "core log",
                        format!("{addresses} connection(s) asked for a bare address in this run"),
                    );
                }
                r
            }
        },
        _ => CheckResult::new("G3", Kind::Hard, Status::Unknown, Some("no_core_log")).with(
            "core",
            "observing a proxy this application does not run; its log is not available",
        ),
    };
    r.with("system DNS", system_dns_line(&input.system_dns))
}

fn describe(r: &Result<Response, HttpError>) -> String {
    match r {
        Ok(resp) => format!("HTTP {} in {} ms", resp.status, resp.elapsed.as_millis()),
        Err(e) => e.to_string(),
    }
}

fn reason_of(r: &Result<Response, HttpError>) -> String {
    match r {
        Ok(resp) => format!("http_{}", resp.status),
        Err(HttpError::Timeout) => "timeout".into(),
        Err(HttpError::Connect(_)) => "connect_failed".into(),
        Err(HttpError::Other(_)) => "request_error".into(),
    }
}

/// One retry on transport failure; an HTTP status is an answer and is not retried.
fn send_retry(http: &dyn HttpClient, req: &Request) -> Result<Response, HttpError> {
    match http.send(req) {
        Err(_) => http.send(req),
        ok => ok,
    }
}

struct TargetProbe<'a> {
    target: &'a Target,
    url: String,
    result: Result<Response, HttpError>,
    ip: Option<String>,
    loc: Option<String>,
}

fn probe_target<'a>(http: &dyn HttpClient, target: &'a Target, port: u16) -> TargetProbe<'a> {
    let (url, req) = if target.trace {
        let url = format!("https://{}/cdn-cgi/trace", target.probe_domain());
        (url.clone(), Request::get(url, Route::Proxy(port)))
    } else {
        let url = format!("https://{}/", target.probe_domain());
        (url.clone(), Request::head(url, Route::Proxy(port)))
    };
    let result = send_retry(http, &req);
    let parsed = match (&result, target.trace) {
        (Ok(r), true) if r.status == 200 => parse_cf_trace(&r.body).ok(),
        _ => None,
    };
    TargetProbe {
        target,
        url,
        ip: parsed.as_ref().map(|t| t.ip.clone()),
        loc: parsed.and_then(|t| t.loc),
        result,
    }
}

/// Most common address among the target traces; ties go to the earliest target.
fn majority_ip(probes: &[TargetProbe]) -> Option<String> {
    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (i, p) in probes.iter().enumerate() {
        if let Some(ip) = &p.ip {
            let e = counts.entry(ip).or_insert((0, i));
            e.0 += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(b.1 .1.cmp(&a.1 .1)))
        .map(|(ip, _)| ip.to_string())
}

/// Both addresses are IPv4, or both are IPv6. Anything that does not parse counts as its own
/// family only when it is textually identical, so a malformed trace never passes as a match.
fn same_family(a: &str, b: &str) -> bool {
    use std::net::IpAddr;
    match (a.parse::<IpAddr>(), b.parse::<IpAddr>()) {
        (Ok(x), Ok(y)) => x.is_ipv4() == y.is_ipv4(),
        _ => true,
    }
}

#[derive(Debug, PartialEq)]
enum NetworkMatch {
    Same(u32),
    Different(u32, u32),
    NoEvidence,
}

/// Whether two addresses belong to the same network, by the lookup sources that answered with an
/// ASN for both. One source that sees two different networks is enough to call it different.
fn same_network(
    a: &[(String, Result<Geo, String>)],
    b: &[(String, Result<Geo, String>)],
) -> NetworkMatch {
    let mut seen = None;
    for (id, ga) in a {
        let asn_a = ga.as_ref().ok().and_then(|g| g.asn);
        let asn_b = b
            .iter()
            .find(|(other, _)| other == id)
            .and_then(|(_, g)| g.as_ref().ok().and_then(|g| g.asn));
        if let (Some(x), Some(y)) = (asn_a, asn_b) {
            if x != y {
                return NetworkMatch::Different(x, y);
            }
            seen = Some(x);
        }
    }
    seen.map_or(NetworkMatch::NoEvidence, NetworkMatch::Same)
}

fn geo_lookup(
    http: &dyn HttpClient,
    providers: &ProviderSet,
    ip: &str,
    port: u16,
) -> Vec<(String, Result<Geo, String>)> {
    providers
        .usable(Purpose::Geo, false)
        .map(|p| {
            let mut req = Request::get(p.url.replace("{ip}", ip), Route::Proxy(port));
            req.connect_timeout = Duration::from_millis(p.connect_timeout_ms.into());
            req.total_timeout = Duration::from_millis(p.total_timeout_ms.into());
            let r = send_retry(http, &req);
            let geo = match &r {
                Ok(resp) if resp.status == 200 => match p.parser {
                    Parser::Ipwhois => parse_ipwhois(&resp.body),
                    Parser::IpsbGeoip => parse_ipsb(&resp.body),
                    Parser::CfTrace | Parser::None => Err("not a geo parser".into()),
                },
                other => Err(reason_of(other)),
            };
            (p.id.clone(), geo)
        })
        .collect()
}

fn trace_via(
    http: &dyn HttpClient,
    providers: &ProviderSet,
    purpose: Purpose,
    route: Route,
) -> Option<(String, Result<Response, HttpError>)> {
    let p = providers
        .usable(purpose, false)
        .find(|p| p.parser == Parser::CfTrace)?;
    let mut req = Request::get(p.url.clone(), route);
    req.connect_timeout = Duration::from_millis(p.connect_timeout_ms.into());
    req.total_timeout = Duration::from_millis(p.total_timeout_ms.into());
    Some((p.id.clone(), send_retry(http, &req)))
}

pub fn run(input: &CheckInput, http: &dyn HttpClient) -> CheckReport {
    let profile = input.profile;
    let port = input.proxy_port;
    let local_only = profile.detection.mode == DetectionMode::LocalOnly;
    let mut results = Vec::new();
    let report = |results, exit_ip| CheckReport {
        generated_at: input.generated_at.clone(),
        mode: profile.detection.mode,
        proxy_port: port,
        results,
        exit_ip,
    };

    if let Err(e) = &input.core {
        results.push(
            CheckResult::new("G1", Kind::Hard, Status::Unknown, Some("core_unreachable"))
                .with("core", e.clone()),
        );
        for t in &profile.targets {
            results.push(
                CheckResult::new(
                    format!("G2:{}", t.domain),
                    Kind::Hard,
                    Status::Unknown,
                    Some("core_unreachable"),
                )
                .with("core", e.clone()),
            );
        }
        results.push(
            CheckResult::new("G3", Kind::Hard, Status::Unknown, Some("core_unreachable"))
                .with("core", e.clone())
                .with("system DNS", system_dns_line(&input.system_dns)),
        );
        return report(results, None);
    }

    let mark = input.observer.map(|o| o.mark());

    // ---- G2 probes, which also provide G1's evidence ----
    let probes: Vec<TargetProbe> = profile
        .targets
        .iter()
        .map(|t| probe_target(http, t, port))
        .collect();
    let ip_proxy = majority_ip(&probes);

    // ---- G1 ----
    let g1 = match &ip_proxy {
        None => {
            let mut r =
                CheckResult::new("G1", Kind::Hard, Status::Unknown, Some("no_target_trace"));
            for p in &probes {
                r = r.with(&p.url, describe(&p.result));
            }
            r
        }
        Some(ip) => {
            let first = probes
                .iter()
                .find(|p| p.ip.as_deref() == Some(ip))
                .expect("majority comes from a probe");
            let loc = first.loc.clone();
            let mut r = CheckResult::new("G1", Kind::Hard, Status::Pass, None).with(
                &first.url,
                format!("exit {ip} loc={}", loc.as_deref().unwrap_or("?")),
            );
            let x = &profile.egress.expected;
            if let Some(want) = &x.ip {
                if want != ip {
                    r.status = Status::Fail;
                    r.reason = Some("unexpected_exit_ip".into());
                    r = r.with("profile", format!("expected exit {want}"));
                }
            }
            if let (Some(want), Some(got)) = (&x.country_code, &loc) {
                if !want.eq_ignore_ascii_case(got) && r.status == Status::Pass {
                    r.status = Status::Fail;
                    r.reason = Some("unexpected_country".into());
                    r = r.with("profile", format!("expected country {want}"));
                }
            }
            if !local_only {
                match trace_via(http, input.providers, Purpose::DirectIp, Route::Direct) {
                    Some((id, Ok(resp))) if resp.status == 200 => {
                        match parse_cf_trace(&resp.body) {
                            Ok(t) if &t.ip == ip => {
                                r.status = Status::Fail;
                                r.reason = Some("same_as_direct".into());
                                r = r.with(
                                    id,
                                    format!(
                                        "direct exit is also {}: the proxy changes nothing",
                                        t.ip
                                    ),
                                );
                            }
                            Ok(t) => r = r.with(id, format!("direct exit {} differs", t.ip)),
                            Err(e) => r = r.with(id, format!("direct trace unreadable: {e}")),
                        }
                    }
                    Some((id, other)) => {
                        r = r.with(id, format!("direct trace failed: {}", describe(&other)))
                    }
                    None => r = r.with("providers", "no direct-ip provider enabled"),
                }
            }
            if x.ip.is_none() && r.status == Status::Pass {
                r = r.with(
                    "profile",
                    format!("expected exit not pinned; confirm {ip} to pin it"),
                );
            }
            r
        }
    };
    results.push(g1);

    // ---- G2, one per target ----
    // The majority exit's network is looked up at most once, and only when a target saw an address
    // of the other family.
    let mut majority_geo: Option<Vec<(String, Result<Geo, String>)>> = None;
    for p in &probes {
        let id = format!("G2:{}", p.target.domain);
        let r = match &p.result {
            Err(_) => CheckResult::new(id, Kind::Hard, Status::Fail, Some(&reason_of(&p.result))),
            Ok(resp) if resp.status >= 500 => {
                CheckResult::new(id, Kind::Hard, Status::Fail, Some(&reason_of(&p.result)))
            }
            Ok(_) if !p.target.trace => CheckResult::new(id, Kind::Hard, Status::Pass, None).with(
                "note",
                "reachability only; this target has no trace to compare exits",
            ),
            Ok(_) => match (&p.ip, &ip_proxy) {
                (Some(a), Some(b)) if a == b => {
                    CheckResult::new(id, Kind::Hard, Status::Pass, None)
                }
                (Some(a), Some(b)) if same_family(a, b) => {
                    CheckResult::new(id, Kind::Hard, Status::Fail, Some("split_routing"))
                        .with("compare", format!("this target sees {a}, the others {b}"))
                }
                (Some(a), Some(b)) => {
                    let compare = format!(
                        "this target sees {a}, the others {b}: one is IPv4, the other IPv6, so the addresses alone cannot tell whether it is the same exit"
                    );
                    if local_only {
                        CheckResult::new(id, Kind::Hard, Status::Unknown, Some("mixed_ip_family"))
                            .with("compare", compare)
                    } else {
                        let theirs = majority_geo
                            .get_or_insert_with(|| geo_lookup(http, input.providers, b, port));
                        let mine = geo_lookup(http, input.providers, a, port);
                        match same_network(&mine, theirs) {
                            NetworkMatch::Same(asn) => {
                                CheckResult::new(id, Kind::Hard, Status::Pass, None)
                                    .with("compare", compare)
                                    .with(
                                        "network",
                                        format!("both addresses are in AS{asn}: a dual-stack exit"),
                                    )
                            }
                            NetworkMatch::Different(x, y) => CheckResult::new(
                                id,
                                Kind::Hard,
                                Status::Fail,
                                Some("split_routing"),
                            )
                            .with("compare", compare)
                            .with("network", format!("{a} is in AS{x}, {b} is in AS{y}")),
                            NetworkMatch::NoEvidence => CheckResult::new(
                                id,
                                Kind::Hard,
                                Status::Unknown,
                                Some("mixed_ip_family"),
                            )
                            .with("compare", compare)
                            .with("network", "no lookup source answered for both addresses"),
                        }
                    }
                }
                _ => CheckResult::new(id, Kind::Hard, Status::Fail, Some("trace_unreadable")),
            },
        };
        results.push(r.with(&p.url, describe(&p.result)));
    }

    // ---- R1 ASN / country ----
    let r1 = match (&ip_proxy, local_only) {
        (_, true) => CheckResult::new("R1", Kind::ReadOnly, Status::Unknown, Some("local_only")),
        (None, _) => CheckResult::new("R1", Kind::ReadOnly, Status::Unknown, Some("no_exit_ip")),
        (Some(ip), false) => {
            let answers = geo_lookup(http, input.providers, ip, port);
            let ok: Vec<&Geo> = answers
                .iter()
                .filter_map(|(_, g)| g.as_ref().ok())
                .collect();
            let mut r = if ok.is_empty() {
                CheckResult::new(
                    "R1",
                    Kind::ReadOnly,
                    Status::Unknown,
                    Some("all_sources_failed"),
                )
            } else {
                let disagree = ok
                    .windows(2)
                    .any(|w| w[0].asn != w[1].asn || w[0].country_code != w[1].country_code);
                let off = profile
                    .egress
                    .expected
                    .asn
                    .is_some_and(|want| ok.iter().any(|g| g.asn.is_some_and(|a| a != want)));
                match (disagree, off) {
                    (true, _) => CheckResult::new(
                        "R1",
                        Kind::ReadOnly,
                        Status::Warn,
                        Some("sources_disagree"),
                    ),
                    (_, true) => {
                        CheckResult::new("R1", Kind::ReadOnly, Status::Warn, Some("unexpected_asn"))
                    }
                    _ => CheckResult::new("R1", Kind::ReadOnly, Status::Pass, None),
                }
            };
            for (id, g) in answers {
                r = r.with(
                    id,
                    match g {
                        Ok(g) => format!(
                            "AS{} {} {}",
                            g.asn.map(|a| a.to_string()).unwrap_or("?".into()),
                            g.org.unwrap_or_default(),
                            g.country_code.unwrap_or_default()
                        ),
                        Err(e) => format!("unknown: {e}"),
                    },
                );
            }
            r
        }
    };
    results.push(r1);

    // ---- R2 latency: median over the first trace target ----
    let r2 = match probes.iter().find(|p| p.target.trace && p.result.is_ok()) {
        None => CheckResult::new(
            "R2",
            Kind::ReadOnly,
            Status::Unknown,
            Some("no_reachable_target"),
        ),
        Some(p) => {
            let mut samples = vec![p.result.as_ref().map(|r| r.elapsed).unwrap_or_default()];
            for _ in 1..LATENCY_SAMPLES {
                if let Ok(r) = http.send(&Request::get(p.url.clone(), Route::Proxy(port))) {
                    samples.push(r.elapsed);
                }
            }
            samples.sort();
            let median = samples[samples.len() / 2];
            let status = if median > LATENCY_WARN {
                Status::Warn
            } else {
                Status::Pass
            };
            CheckResult::new(
                "R2",
                Kind::ReadOnly,
                status,
                (status == Status::Warn).then_some("slow"),
            )
            .with(
                &p.url,
                format!(
                    "median {} ms over {} samples",
                    median.as_millis(),
                    samples.len()
                ),
            )
        }
    };
    results.push(r2);

    // ---- R9 exit of non-target traffic ----
    let r9 = if local_only {
        CheckResult::new("R9", Kind::ReadOnly, Status::Unknown, Some("local_only"))
    } else {
        match trace_via(
            http,
            input.providers,
            Purpose::OtherExit,
            Route::Proxy(port),
        ) {
            Some((id, Ok(resp))) if resp.status == 200 => match parse_cf_trace(&resp.body) {
                Ok(t) => CheckResult::new("R9", Kind::ReadOnly, Status::Pass, None).with(
                    id,
                    format!("non-target exit {} loc={}", t.ip, t.loc.unwrap_or_default()),
                ),
                Err(e) => {
                    CheckResult::new("R9", Kind::ReadOnly, Status::Unknown, Some("parse_error"))
                        .with(id, e)
                }
            },
            Some((id, other)) => CheckResult::new(
                "R9",
                Kind::ReadOnly,
                Status::Unknown,
                Some(&reason_of(&other)),
            )
            .with(id, describe(&other)),
            None => CheckResult::new("R9", Kind::ReadOnly, Status::Unknown, Some("disabled")),
        }
    };
    results.push(r9);

    // ---- G3 last, so the core has written every connection of this run; listed after G2 ----
    let g3 = dns_path(input, mark);
    let at = results
        .iter()
        .rposition(|r| r.id.starts_with("G2:"))
        .map_or(results.len(), |i| i + 1);
    results.insert(at, g3);

    report(results, ip_proxy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProfileSet;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const EXIT: &str = "198.51.100.7";
    const HOME: &str = "203.0.113.9";

    fn trace(ip: &str, loc: &str) -> Result<Response, HttpError> {
        Ok(Response {
            status: 200,
            body: format!("fl=1\nip={ip}\nloc={loc}\ncolo=LAX\n"),
            elapsed: Duration::from_millis(300),
        })
    }
    fn json(body: &str) -> Result<Response, HttpError> {
        Ok(Response {
            status: 200,
            body: body.into(),
            elapsed: Duration::from_millis(200),
        })
    }

    /// Answers by (url, route); records every request so tests can assert what was (not) sent.
    #[derive(Default)]
    struct FakeHttp {
        answers: HashMap<(String, Route), Result<Response, HttpError>>,
        sent: RefCell<Vec<Request>>,
    }
    impl FakeHttp {
        fn on(mut self, url: &str, route: Route, r: Result<Response, HttpError>) -> Self {
            self.answers.insert((url.into(), route), r);
            self
        }
        fn urls(&self) -> Vec<String> {
            self.sent.borrow().iter().map(|r| r.url.clone()).collect()
        }
    }
    impl HttpClient for FakeHttp {
        fn send(&self, req: &Request) -> Result<Response, HttpError> {
            self.sent.borrow_mut().push(req.clone());
            self.answers
                .get(&(req.url.clone(), req.route))
                .cloned()
                .unwrap_or(Err(HttpError::Timeout))
        }
    }

    const P: Route = Route::Proxy(17893);

    fn healthy() -> FakeHttp {
        FakeHttp::default()
            .on("https://claude.ai/cdn-cgi/trace", P, trace(EXIT, "US"))
            .on("https://www.anthropic.com/cdn-cgi/trace", P, trace(EXIT, "US"))
            .on("https://chatgpt.com/cdn-cgi/trace", P, trace(EXIT, "US"))
            .on("https://www.cloudflare.com/cdn-cgi/trace", Route::Direct, trace(HOME, "CN"))
            .on("https://www.cloudflare.com/cdn-cgi/trace", P, trace(EXIT, "US"))
            .on(&format!("https://ipwho.is/{EXIT}"), P, json(r#"{"success":true,"country_code":"US","connection":{"asn":64500,"org":"Example ISP"}}"#))
            .on(&format!("https://api.ip.sb/geoip/{EXIT}"), P, json(r#"{"ip":"x","asn":64500,"asn_organization":"Example ISP","country_code":"US"}"#))
    }

    fn profile() -> AgentProfile {
        ProfileSet::default().agent.remove(0)
    }

    /// Core log stand-in: returns whatever records it holds; `listening` and `fail` shape G3.
    struct FakeObserver {
        records: Vec<ConnRecord>,
        listening: bool,
        fail: Option<String>,
    }

    impl CoreObserver for FakeObserver {
        fn mark(&self) -> u64 {
            0
        }
        fn records_since(&self, _: u64) -> Result<Vec<ConnRecord>, String> {
            self.fail
                .clone()
                .map_or_else(|| Ok(self.records.clone()), Err)
        }
        fn dns_listening(&self) -> Result<bool, String> {
            Ok(self.listening)
        }
    }

    fn record(host: &str, group: &str) -> ConnRecord {
        ConnRecord {
            network: "TCP".into(),
            host: host.into(),
            port: 443,
            rule: "Match".into(),
            group: group.into(),
            via: Some("upstream".into()),
            error: None,
        }
    }

    /// Every default target seen by name, routed through the profile's group.
    fn good_log() -> FakeObserver {
        FakeObserver {
            records: ["claude.ai", "www.anthropic.com", "chatgpt.com"]
                .iter()
                .map(|h| record(h, "agent-egress"))
                .collect(),
            listening: true,
            fail: None,
        }
    }

    fn run_full(
        p: &AgentProfile,
        http: &FakeHttp,
        core: Result<(), String>,
        observer: Option<&dyn CoreObserver>,
    ) -> CheckReport {
        let providers = ProviderSet::default();
        run(
            &CheckInput {
                profile: p,
                providers: &providers,
                proxy_port: 17893,
                core,
                observer,
                system_dns: Ok(vec!["192.0.2.53".into()]),
                generated_at: "t".into(),
            },
            http,
        )
    }

    fn run_with(p: &AgentProfile, http: &FakeHttp, core: Result<(), String>) -> CheckReport {
        run_full(p, http, core, Some(&good_log()))
    }

    fn get<'a>(r: &'a CheckReport, id: &str) -> &'a CheckResult {
        r.results
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no {id} in {r:#?}"))
    }

    #[test]
    fn healthy_environment_passes() {
        let r = run_with(&profile(), &healthy(), Ok(()));
        for id in [
            "G1",
            "G2:claude.ai",
            "G2:www.anthropic.com",
            "G2:chatgpt.com",
            "G3",
            "R1",
            "R2",
            "R9",
        ] {
            assert_eq!(get(&r, id).status, Status::Pass, "{id}: {:#?}", get(&r, id));
        }
        assert!(r.hard_checks_pass());
        assert!(get(&r, "G1")
            .evidence
            .iter()
            .any(|e| e.detail.contains("not pinned")));
    }

    #[test]
    fn proxy_that_changes_nothing_fails_g1() {
        let http = healthy().on(
            "https://www.cloudflare.com/cdn-cgi/trace",
            Route::Direct,
            trace(EXIT, "US"),
        );
        let r = run_with(&profile(), &http, Ok(()));
        assert_eq!(
            (get(&r, "G1").status, get(&r, "G1").reason.as_deref()),
            (Status::Fail, Some("same_as_direct"))
        );
        assert!(!r.hard_checks_pass());
    }

    #[test]
    fn expected_exit_and_country_are_enforced() {
        let mut p = profile();
        p.egress.expected.ip = Some("198.51.100.8".into());
        assert_eq!(
            get(&run_with(&p, &healthy(), Ok(())), "G1")
                .reason
                .as_deref(),
            Some("unexpected_exit_ip")
        );
        let mut p = profile();
        p.egress.expected.ip = Some(EXIT.into());
        p.egress.expected.country_code = Some("JP".into());
        assert_eq!(
            get(&run_with(&p, &healthy(), Ok(())), "G1")
                .reason
                .as_deref(),
            Some("unexpected_country")
        );
        p.egress.expected.country_code = Some("US".into());
        let r = run_with(&p, &healthy(), Ok(()));
        assert_eq!(get(&r, "G1").status, Status::Pass);
        assert!(!get(&r, "G1")
            .evidence
            .iter()
            .any(|e| e.detail.contains("not pinned")));
    }

    #[test]
    fn unreachable_core_makes_everything_unknown_and_sends_nothing() {
        let http = healthy();
        let r = run_with(&profile(), &http, Err("controller refused".into()));
        assert!(r.results.iter().all(
            |c| c.status == Status::Unknown && c.reason.as_deref() == Some("core_unreachable")
        ));
        assert!(http.urls().is_empty());
        assert!(!r.hard_checks_pass());
    }

    #[test]
    fn no_trace_answers_are_unknown_for_g1_and_failures_for_g2() {
        let r = run_with(&profile(), &FakeHttp::default(), Ok(()));
        assert_eq!(
            (get(&r, "G1").status, get(&r, "G1").reason.as_deref()),
            (Status::Unknown, Some("no_target_trace"))
        );
        assert_eq!(
            (
                get(&r, "G2:claude.ai").status,
                get(&r, "G2:claude.ai").reason.as_deref()
            ),
            (Status::Fail, Some("timeout"))
        );
        assert_eq!(get(&r, "R1").status, Status::Unknown);
        assert_eq!(get(&r, "R2").status, Status::Unknown);
    }

    #[test]
    fn retries_once_on_transport_failure() {
        let http = FakeHttp::default();
        run_with(&profile(), &http, Ok(()));
        let n = http
            .urls()
            .iter()
            .filter(|u| u.as_str() == "https://claude.ai/cdn-cgi/trace")
            .count();
        assert_eq!(n, 2);
    }

    const EXIT6: &str = "2001:db8::3";

    /// claude.ai answers over IPv4 while the other targets answer over IPv6, as reported by a
    /// tester whose exit is dual-stack.
    fn dual_stack(asn6: u32) -> FakeHttp {
        healthy()
            .on("https://www.anthropic.com/cdn-cgi/trace", P, trace(EXIT6, "US"))
            .on("https://chatgpt.com/cdn-cgi/trace", P, trace(EXIT6, "US"))
            .on(&format!("https://ipwho.is/{EXIT6}"), P, json(&format!(r#"{{"success":true,"country_code":"US","connection":{{"asn":{asn6},"org":"Example ISP"}}}}"#)))
            .on(&format!("https://api.ip.sb/geoip/{EXIT6}"), P, json(&format!(r#"{{"ip":"x","asn":{asn6},"asn_organization":"Example ISP","country_code":"US"}}"#)))
    }

    #[test]
    fn a_dual_stack_exit_in_one_network_passes_g2() {
        let r = run_with(&profile(), &dual_stack(64500), Ok(()));
        let g2 = get(&r, "G2:claude.ai");
        assert_eq!(g2.status, Status::Pass, "{g2:#?}");
        assert!(g2.evidence.iter().any(|e| e.detail.contains("AS64500")));
        assert!(r.hard_checks_pass(), "{r:#?}");
    }

    #[test]
    fn a_dual_stack_exit_in_two_networks_fails_g2() {
        let r = run_with(&profile(), &dual_stack(64501), Ok(()));
        assert_eq!(
            get(&r, "G2:claude.ai").reason.as_deref(),
            Some("split_routing")
        );
    }

    #[test]
    fn mixed_families_without_a_lookup_are_unknown() {
        let http = dual_stack(64500)
            .on(
                &format!("https://ipwho.is/{EXIT}"),
                P,
                Err(HttpError::Timeout),
            )
            .on(
                &format!("https://api.ip.sb/geoip/{EXIT}"),
                P,
                Err(HttpError::Timeout),
            );
        let r = run_with(&profile(), &http, Ok(()));
        let g2 = get(&r, "G2:claude.ai");
        assert_eq!(
            (g2.status, g2.reason.as_deref()),
            (Status::Unknown, Some("mixed_ip_family"))
        );
        let mut p = profile();
        p.detection.mode = DetectionMode::LocalOnly;
        let r = run_with(&p, &dual_stack(64500), Ok(()));
        assert_eq!(
            get(&r, "G2:claude.ai").reason.as_deref(),
            Some("mixed_ip_family")
        );
    }

    #[test]
    fn split_routing_and_server_errors_fail_g2() {
        let http = healthy()
            .on(
                "https://chatgpt.com/cdn-cgi/trace",
                P,
                trace("192.0.2.1", "SG"),
            )
            .on(
                "https://www.anthropic.com/cdn-cgi/trace",
                P,
                Ok(Response {
                    status: 502,
                    body: String::new(),
                    elapsed: Duration::ZERO,
                }),
            );
        let r = run_with(&profile(), &http, Ok(()));
        assert_eq!(
            get(&r, "G1").status,
            Status::Pass,
            "majority exit is still the fixed one"
        );
        assert_eq!(
            get(&r, "G2:chatgpt.com").reason.as_deref(),
            Some("split_routing")
        );
        assert_eq!(
            get(&r, "G2:www.anthropic.com").reason.as_deref(),
            Some("http_502")
        );
    }

    #[test]
    fn resource_root_probes_the_service_subdomain_without_narrowing_protection() {
        let mut p = profile();
        p.egress.fixed_exit = true;
        p.egress.expected.ip = Some(EXIT.into());
        p.targets = serde_json::from_str(
            r#"[{"domain":"claude.ai","trace":true},{"domain":"oaiusercontent.com","trace":false}]"#,
        ).unwrap();
        let http = healthy().on(
            "https://files.oaiusercontent.com/",
            P,
            Ok(Response {
                status: 404,
                body: String::new(),
                elapsed: Duration::ZERO,
            }),
        );
        let observer = FakeObserver {
            records: vec![
                record("claude.ai", "relay"),
                record("files.oaiusercontent.com", "relay"),
            ],
            listening: true,
            fail: None,
        };
        let report = run_full(&p, &http, Ok(()), Some(&observer));
        assert_eq!(get(&report, "G2:oaiusercontent.com").status, Status::Pass);
        assert_eq!(get(&report, "G3").status, Status::Pass);
        assert!(http
            .sent
            .borrow()
            .iter()
            .any(|q| q.url == "https://files.oaiusercontent.com/"
                && q.route == P
                && q.method == crate::http::Method::Head));
        assert!(!http.urls().contains(&"https://oaiusercontent.com/".into()));
        assert!(crate::fixed::protects(&p, "another.oaiusercontent.com"));
        // A failed service probe still fails the gate: no fabricated pass or direct fallback.
        let failed = run_full(&p, &healthy(), Ok(()), Some(&observer));
        assert_eq!(get(&failed, "G2:oaiusercontent.com").status, Status::Fail);
    }

    #[test]
    fn target_without_trace_is_probed_with_head() {
        let mut p = profile();
        p.targets = vec![
            Target {
                domain: "claude.ai".into(),
                trace: true,
            },
            Target {
                domain: "example.org".into(),
                trace: false,
            },
        ];
        let http = healthy().on(
            "https://example.org/",
            P,
            Ok(Response {
                status: 301,
                body: String::new(),
                elapsed: Duration::ZERO,
            }),
        );
        let r = run_with(&p, &http, Ok(()));
        assert_eq!(get(&r, "G2:example.org").status, Status::Pass);
        assert!(http
            .sent
            .borrow()
            .iter()
            .any(|q| q.url == "https://example.org/" && q.method == crate::http::Method::Head));
    }

    #[test]
    fn local_only_sends_no_third_party_request() {
        let mut p = profile();
        p.detection.mode = DetectionMode::LocalOnly;
        let http = healthy();
        let r = run_with(&p, &http, Ok(()));
        assert_eq!(get(&r, "G1").status, Status::Pass);
        assert_eq!(get(&r, "R1").reason.as_deref(), Some("local_only"));
        assert_eq!(get(&r, "R9").reason.as_deref(), Some("local_only"));
        let targets: Vec<String> = p
            .targets
            .iter()
            .map(|t| format!("https://{}/cdn-cgi/trace", t.domain))
            .collect();
        for u in http.urls() {
            assert!(
                targets.contains(&u),
                "local-only mode sent a non-target request: {u}"
            );
        }
    }

    #[test]
    fn geo_sources_are_compared() {
        let http = healthy().on(
            &format!("https://api.ip.sb/geoip/{EXIT}"),
            P,
            json(r#"{"ip":"x","asn":64501,"country_code":"US"}"#),
        );
        assert_eq!(
            get(&run_with(&profile(), &http, Ok(())), "R1")
                .reason
                .as_deref(),
            Some("sources_disagree")
        );
        let http = healthy().on(
            &format!("https://api.ip.sb/geoip/{EXIT}"),
            P,
            Ok(Response {
                status: 403,
                body: "Forbidden".into(),
                elapsed: Duration::ZERO,
            }),
        );
        let r = run_with(&profile(), &http, Ok(()));
        assert_eq!(
            get(&r, "R1").status,
            Status::Pass,
            "one source is enough when the other fails"
        );
        assert!(get(&r, "R1")
            .evidence
            .iter()
            .any(|e| e.detail.contains("http_403")));
        let mut p = profile();
        p.egress.expected.asn = Some(13335);
        assert_eq!(
            get(&run_with(&p, &healthy(), Ok(())), "R1")
                .reason
                .as_deref(),
            Some("unexpected_asn")
        );
    }

    #[test]
    fn slow_exit_warns() {
        let slow = Ok(Response {
            status: 200,
            body: format!("ip={EXIT}\nloc=US\n"),
            elapsed: Duration::from_millis(2500),
        });
        let http = healthy().on("https://claude.ai/cdn-cgi/trace", P, slow);
        let r = run_with(&profile(), &http, Ok(()));
        assert_eq!(
            (get(&r, "R2").status, get(&r, "R2").reason.as_deref()),
            (Status::Warn, Some("slow"))
        );
        assert!(
            r.hard_checks_pass(),
            "a read-only warning does not block readiness"
        );
    }

    fn tz(
        current: Result<&str, &str>,
        timezone_writable: bool,
        ours: bool,
        writers: &[&str],
    ) -> (CheckResult, CheckResult) {
        let ctx = TimezoneContext {
            current: current.map(str::to_string).map_err(str::to_string),
            timezone_writable,
            changed_by_us: ours,
            foreign_writers: writers.iter().map(|w| w.to_string()).collect(),
            privilege: Ok(true),
            browser_timezones: Vec::new(),
        };
        let mut v = timezone_checks(&profile(), &ctx);
        let r10 = v.pop().unwrap();
        (v.pop().unwrap(), r10)
    }

    #[test]
    fn g4_cases() {
        let east = "Eastern Standard Time";
        let (g, r) = tz(Ok(east), true, true, &[]);
        assert_eq!((g.status, r.status), (Status::Pass, Status::Pass));
        let (g, r) = tz(Ok(east), false, false, &["Local\\AIEgressGuard is held"]);
        assert_eq!(
            (g.status, r.status, r.reason.as_deref()),
            (Status::Pass, Status::Warn, Some("maintained_externally"))
        );
        let (_, r) = tz(Ok(east), false, false, &[]);
        assert_eq!(
            (r.status, r.reason.as_deref()),
            (Status::Pass, Some("already_at_target"))
        );
        let (g, _) = tz(Ok("China Standard Time"), true, false, &[]);
        assert_eq!(
            (g.status, g.reason.as_deref()),
            (Status::Fail, Some("mismatch"))
        );
        let (g, _) = tz(Ok("China Standard Time"), false, false, &[]);
        assert_eq!(
            (g.status, g.reason.as_deref()),
            (Status::Unknown, Some("global_state_not_taken_over"))
        );
        let (g, _) = tz(Err("GetDynamicTimeZoneInformation"), true, false, &[]);
        assert_eq!(g.status, Status::Unknown);
        assert!(tz(Ok(east), true, true, &[])
            .0
            .evidence
            .iter()
            .any(|e| e.detail.contains("SeTimeZonePrivilege held")));
    }

    fn signals() -> crate::checkpage::BrowserSignals {
        crate::checkpage::BrowserSignals {
            language: Some("en-US".into()),
            languages: vec!["en-US".into(), "en".into()],
            timezone: Some("America/New_York".into()),
            cookie_enabled: Some(true),
            canvas_sha256: Some("ab".into()),
            webgl_vendor: Some("Google Inc.".into()),
            webgl_renderer: Some("ANGLE".into()),
            candidates: vec![
                "candidate:1 1 udp 2113937151 0b1c.local 54321 typ host generation 0".into(),
                format!(
                    "candidate:2 1 udp 1677729535 {EXIT} 61000 typ srflx raddr 0.0.0.0 rport 0"
                ),
            ],
            user_agent: None,
            accept_language_header: Some("en-US,en;q=0.9".into()),
        }
    }

    fn browser(
        s: Option<&crate::checkpage::BrowserSignals>,
        exit: Option<&str>,
        p: &AgentProfile,
    ) -> HashMap<String, CheckResult> {
        browser_checks(p, "edge", s, exit)
            .0
            .into_iter()
            .map(|r| (r.id.split(':').next().unwrap().to_string(), r))
            .collect()
    }

    #[test]
    fn browser_signals_matching_the_profile_pass() {
        let r = browser(Some(&signals()), Some(EXIT), &profile());
        for id in ["G5", "R3", "R4", "R5"] {
            assert_eq!(r[id].status, Status::Pass, "{id}: {:#?}", r[id]);
        }
        assert_eq!(r["G5"].id, "G5:edge");
        assert_eq!(
            browser_checks(&profile(), "edge", Some(&signals()), None)
                .1
                .as_deref(),
            Some("America/New_York")
        );
    }

    #[test]
    fn browser_language_failures() {
        let mut s = signals();
        s.language = Some("zh-CN".into());
        assert_eq!(
            browser(Some(&s), Some(EXIT), &profile())["G5"]
                .reason
                .as_deref(),
            Some("language_mismatch")
        );
        let mut s = signals();
        s.accept_language_header = Some("zh-CN,zh;q=0.9,en;q=0.8".into());
        assert_eq!(
            browser(Some(&s), Some(EXIT), &profile())["G5"]
                .reason
                .as_deref(),
            Some("accept_language_mismatch")
        );
        let mut s = signals();
        s.accept_language_header = None;
        assert_eq!(
            browser(Some(&s), Some(EXIT), &profile())["G5"].status,
            Status::Unknown
        );
        let none = browser(None, Some(EXIT), &profile());
        assert!(none
            .values()
            .all(|r| r.status == Status::Unknown && r.reason.as_deref() == Some("no_report")));
    }

    #[test]
    fn webrtc_revealing_another_address_warns() {
        let mut s = signals();
        s.candidates
            .push(format!("candidate:3 1 udp 1 {HOME} 1 typ srflx"));
        let r = browser(Some(&s), Some(EXIT), &profile());
        assert_eq!(
            (r["R3"].status, r["R3"].reason.as_deref()),
            (Status::Warn, Some("webrtc_other_address"))
        );
        assert!(r["R3"]
            .evidence
            .iter()
            .any(|e| e.detail.contains("disable_non_proxied_udp")));
        let mut s = signals();
        s.candidates.retain(|c| c.contains("typ host"));
        assert_eq!(
            browser(Some(&s), Some(EXIT), &profile())["R3"].status,
            Status::Unknown,
            "no public candidate is not proof of anything"
        );
        assert_eq!(
            browser(Some(&s), Some(EXIT), &profile())["R3"]
                .reason
                .as_deref(),
            Some("no_public_candidate")
        );
        assert_eq!(
            browser(Some(&signals()), None, &profile())["R3"]
                .reason
                .as_deref(),
            Some("no_exit_ip")
        );
        let mut p = profile();
        p.detection.mode = DetectionMode::LocalOnly;
        assert_eq!(
            browser(Some(&signals()), Some(EXIT), &p)["R3"]
                .reason
                .as_deref(),
            Some("local_only")
        );
        let mut s = signals();
        s.cookie_enabled = Some(false);
        assert_eq!(
            browser(Some(&s), Some(EXIT), &profile())["R5"].status,
            Status::Warn
        );
    }

    #[test]
    fn browser_time_zone_feeds_g4() {
        let ctx = |tz: &str| TimezoneContext {
            current: Ok("Eastern Standard Time".into()),
            timezone_writable: true,
            changed_by_us: true,
            foreign_writers: vec![],
            privilege: Ok(true),
            browser_timezones: vec![("edge".into(), tz.into())],
        };
        assert_eq!(
            timezone_checks(&profile(), &ctx("America/New_York"))[0].status,
            Status::Pass
        );
        let g4 = timezone_checks(&profile(), &ctx("Asia/Shanghai")).remove(0);
        assert_eq!(
            (g4.status, g4.reason.as_deref()),
            (Status::Fail, Some("browser_timezone_mismatch"))
        );
    }

    fn g3(observer: Option<&dyn CoreObserver>) -> CheckResult {
        let r = run_full(&profile(), &healthy(), Ok(()), observer);
        get(&r, "G3").clone()
    }

    #[test]
    fn g3_passes_when_every_target_went_by_name_through_the_group() {
        let r = g3(Some(&good_log()));
        assert_eq!(r.status, Status::Pass, "{r:#?}");
        assert!(r
            .evidence
            .iter()
            .any(|e| e.source == "system DNS" && e.detail == "192.0.2.53"));
        let order = run_with(&profile(), &healthy(), Ok(()));
        let ids: Vec<&str> = order.results.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            &ids[..5],
            [
                "G1",
                "G2:claude.ai",
                "G2:www.anthropic.com",
                "G2:chatgpt.com",
                "G3"
            ]
        );
    }

    #[test]
    fn g3_fails_on_wrong_group_missing_name_or_dead_dns() {
        let mut log = good_log();
        log.records[1] = record("www.anthropic.com", "DIRECT");
        assert_eq!(g3(Some(&log)).reason.as_deref(), Some("wrong_group"));

        let mut log = good_log();
        log.records.retain(|c| c.host != "chatgpt.com");
        log.records.push(record("104.18.32.47", "agent-egress"));
        let r = g3(Some(&log));
        assert_eq!(r.reason.as_deref(), Some("no_domain_record"));
        assert!(r.evidence.iter().any(|e| e.detail.contains("bare address")));

        let mut log = good_log();
        log.listening = false;
        assert_eq!(g3(Some(&log)).reason.as_deref(), Some("dns_not_listening"));
    }

    #[test]
    fn g3_dial_errors_do_not_fail_the_dns_path() {
        let mut log = good_log();
        log.records[2].error = Some("dial tcp 192.0.2.10:443: i/o timeout".into());
        let r = g3(Some(&log));
        assert_eq!(r.status, Status::Pass);
        assert!(r
            .evidence
            .iter()
            .any(|e| e.detail.contains("could not reach it")));
    }

    #[test]
    fn g3_is_unknown_without_a_readable_core_log() {
        assert_eq!(g3(None).reason.as_deref(), Some("no_core_log"));
        let mut log = good_log();
        log.fail = Some("access denied".into());
        assert_eq!(
            g3(Some(&log)).reason.as_deref(),
            Some("core_log_unreadable")
        );
        let r = run_full(
            &profile(),
            &healthy(),
            Err("down".into()),
            Some(&good_log()),
        );
        assert_eq!(get(&r, "G3").reason.as_deref(), Some("core_unreachable"));
        assert!(!r.hard_checks_pass());
    }
}
