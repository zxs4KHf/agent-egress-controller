//! Explicit browser-environment plans. This module never changes the system proxy or time zone.
//! Launch flags apply only to AEC-owned user-data directories; global policies target Edge only.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::path::Path;

const STATE: &str = "browser-environment.json";
const JOURNAL: &str = "browser-environment-recovery.json";
const PENDING: &str = "browser-environment-preview.json";
const LAST_ERROR: &str = "browser-environment-last-error.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    AiOnly,
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    pub scope: Scope,
    pub language: String,
    pub prevent_webrtc: bool,
    pub disable_quic: bool,
    pub remote_dns_only: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            scope: Scope::AiOnly,
            language: "en-US".into(),
            prevent_webrtc: true,
            disable_quic: true,
            remote_dns_only: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Policy {
    QuicAllowed,
    WebRtcLocalhostIpHandling,
    WebRtcIPHandlingUrl,
    NetworkPredictionOptions,
    ApplicationLocaleValue,
}
impl Policy {
    pub fn name(self) -> &'static str {
        match self {
            Self::QuicAllowed => "QuicAllowed",
            Self::WebRtcLocalhostIpHandling => "WebRtcLocalhostIpHandling",
            Self::WebRtcIPHandlingUrl => "WebRtcIPHandlingUrl",
            Self::NetworkPredictionOptions => "NetworkPredictionOptions",
            Self::ApplicationLocaleValue => "ApplicationLocaleValue",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Dword(u32),
    Text(String),
}
/// Only these enumerated Edge policy values, not arbitrary registry keys, cross this seam.
pub trait Host {
    fn read(&self, policy: Policy, machine: bool) -> Result<Option<Value>, &'static str>;
    fn write(&self, policy: Policy, value: Option<&Value>) -> Result<(), &'static str>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    State,
    Preview {
        options: Options,
    },
    Apply {
        #[serde(rename = "planId")]
        plan_id: String,
    },
    Restore,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Change {
    policy: Policy,
    before: Option<Value>,
    after: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    id: String,
    expires: u64,
    options: Options,
    before: Option<Options>,
    changes: Vec<Change>,
}
fn load<T: serde::de::DeserializeOwned>(
    data: &Path,
    name: &str,
) -> Result<Option<T>, &'static str> {
    match std::fs::read(data.join(name)) {
        Ok(bytes) if bytes.len() <= 64 * 1024 => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "BROWSER_STATE_INVALID"),
        Ok(_) => Err("BROWSER_STATE_INVALID"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("BROWSER_STATE_UNREADABLE"),
    }
}
fn save(data: &Path, name: &str, value: &impl Serialize) -> Result<(), &'static str> {
    crate::store::write_atomic(
        &data.join(name),
        &serde_json::to_vec_pretty(value).map_err(|_| "BROWSER_STATE_INVALID")?,
    )
    .map_err(|_| "BROWSER_STATE_WRITE_FAILED")
}
fn remove(data: &Path, name: &str) -> Result<(), &'static str> {
    match std::fs::remove_file(data.join(name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("BROWSER_STATE_WRITE_FAILED"),
    }
}
fn validate(options: &Options) -> Result<(), &'static str> {
    if !crate::setup::LANGUAGES
        .iter()
        .any(|(lang, _)| *lang == options.language)
    {
        return Err("BROWSER_LANGUAGE_INVALID");
    }
    Ok(())
}
pub fn flags(options: &Options) -> Vec<String> {
    let primary = options.language.split('-').next().unwrap_or("en");
    let mut flags = vec![
        format!("--lang={}", options.language),
        format!("--accept-lang={},{primary}", options.language),
    ];
    if options.prevent_webrtc {
        flags.push("--webrtc-ip-handling-policy=disable_non_proxied_udp".into());
    }
    if options.disable_quic {
        flags.push("--disable-quic".into());
    }
    if options.remote_dns_only {
        // Chromium applies resolver rules to proxy addresses too, including numeric loopback.
        // Exempt only our literal proxy host; never exempt arbitrary target domains or all IPs.
        flags.push(
            "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE 127.0.0.1".into(),
        );
    }
    flags
}
pub fn launch_flags(data: &Path) -> Result<Vec<String>, &'static str> {
    current_options(data)?
        .map(|options| Ok(flags(&options)))
        .unwrap_or_else(|| Ok(Vec::new()))
}
fn current_options(data: &Path) -> Result<Option<Options>, &'static str> {
    let options: Option<Options> = load(data, STATE)?;
    if load::<Plan>(data, JOURNAL)?.is_some_and(|plan| options.as_ref() != Some(&plan.options)) {
        return Err("BROWSER_RECOVERY_REQUIRED");
    }
    if let Some(options) = &options {
        validate(options)?;
    }
    Ok(options)
}
pub fn effective_profile(
    data: &Path,
    profile: &crate::profile::AgentProfile,
) -> Result<crate::profile::AgentProfile, &'static str> {
    let mut effective = profile.clone();
    if let Some(options) = current_options(data)? {
        let primary = options.language.split('-').next().unwrap_or("en");
        effective.locale.browser_lang = options.language.clone();
        effective.locale.accept_language = format!("{},{primary}", options.language);
    }
    Ok(effective)
}
fn view(plan: &Plan) -> Json {
    json!({"planId":plan.id,"scope":plan.options.scope,"options":plan.options,"changes":plan.changes,
        "launchFlags":flags(&plan.options),"expiresAtUnixMs":plan.expires,"requiresConfirmation":true,"runtimeVerified":false,
        "affectedBrowsers":if plan.options.scope == Scope::Global {vec!["普通 Microsoft Edge 的全部 profile", "AEC 自己启动的专用 Edge / Chrome"]} else {vec!["仅 AEC 启动的专用 Edge / Chrome"]},
        "excludedBrowsers":["日常 Chrome / Chrome for Testing / 指纹实例，不写它们的全局策略或 profile"],
        "limitations":["全局范围当前仅支持 Edge；如指纹浏览器改用 Edge，请勿应用全局范围", "全局策略可能影响视频通话、HTTP/3，需用户自行重新启动 Edge；AEC 不关闭日常浏览器", "DNS 固定链路由选定站点的客户端协作策略提供；全局浏览器 DNS、UDP、WebRTC、EDNS/ECS 与 IPv6 泄露无运行证据仍是未知", "浏览器语言不等于 Windows 显示语言；系统时区仍须单独经过接管门，且可能影响其他应用"]})
}

fn edge_changes(host: &dyn Host, options: &Options) -> Result<Vec<Change>, &'static str> {
    let mut desired = vec![(
        Policy::ApplicationLocaleValue,
        Value::Text(options.language.clone()),
    )];
    if options.disable_quic {
        desired.push((Policy::QuicAllowed, Value::Dword(0)));
    }
    if options.prevent_webrtc {
        desired.push((
            Policy::WebRtcLocalhostIpHandling,
            Value::Text("disable_non_proxied_udp".into()),
        ));
        desired.push((
            Policy::WebRtcIPHandlingUrl,
            Value::Text(r#"[{"handling":"disable_non_proxied_udp","url":"*"}]"#.into()),
        ));
    }
    if options.remote_dns_only {
        desired.push((Policy::NetworkPredictionOptions, Value::Dword(2)));
    }
    desired
        .into_iter()
        .map(|(policy, after)| {
            if host.read(policy, true)?.is_some() {
                return Err("BROWSER_MACHINE_POLICY_CONFLICT");
            }
            Ok(Change {
                policy,
                before: host.read(policy, false)?,
                after,
            })
        })
        .collect()
}
fn restore(data: &Path, host: &dyn Host) -> Result<Json, &'static str> {
    let Some(plan) = load::<Plan>(data, JOURNAL)? else {
        return Ok(json!({"restored":true,"runtimeVerified":false}));
    };
    let current: Option<Options> = load(data, STATE)?;
    if current.as_ref() != Some(&plan.options) && current != plan.before {
        return Err("BROWSER_RESTORE_CONFLICT");
    }
    // Check every value before changing any. A foreign writer always wins; retain the journal.
    for change in &plan.changes {
        let current = host.read(change.policy, false)?;
        if current.as_ref() != Some(&change.after) && current != change.before {
            return Err("BROWSER_RESTORE_CONFLICT");
        }
    }
    for change in plan.changes.iter().rev() {
        let current = host.read(change.policy, false)?;
        if current.as_ref() != Some(&change.after) && current != change.before {
            return Err("BROWSER_RESTORE_CONFLICT");
        }
        if current.as_ref() == Some(&change.after) && change.before.as_ref() != Some(&change.after)
        {
            host.write(change.policy, change.before.as_ref())?;
            if host.read(change.policy, false)? != change.before {
                return Err("BROWSER_RESTORE_VERIFY_FAILED");
            }
        }
    }
    if let Some(before) = plan.before {
        save(data, STATE, &before)?;
    } else {
        remove(data, STATE)?;
    }
    remove(data, JOURNAL)?;
    Ok(json!({"restored":true,"runtimeVerified":false,"requiresBrowserRestart":true}))
}

/// Caller serializes operations. `local_confirmation` must never be true for an Agent call.
/// A durable recovery journal is written before any policy or owned launch-setting mutation.
pub fn execute(
    data: &Path,
    host: &dyn Host,
    command: Command,
    now: u64,
    nonce: &str,
    local_confirmation: bool,
) -> Result<Json, &'static str> {
    let action = match &command {
        Command::State => "state",
        Command::Preview { .. } => "preview",
        Command::Apply { .. } => "apply",
        Command::Restore => "restore",
    };
    let result = run(data, host, command, now, nonce, local_confirmation);
    if let Err(code) = result {
        let _ = save(
            data,
            LAST_ERROR,
            &json!({"code":code,"action":action,"recordedAtUnixMs":now,"historical":true}),
        );
    }
    result
}

fn run(
    data: &Path,
    host: &dyn Host,
    command: Command,
    now: u64,
    nonce: &str,
    local_confirmation: bool,
) -> Result<Json, &'static str> {
    match command {
        Command::State => {
            let active: Option<Plan> = load(data, JOURNAL)?;
            let options: Option<Options> = load(data, STATE)?;
            let pending: Option<Plan> = load(data, PENDING)?;
            Ok(
                json!({"options":options,"canRestore":active.is_some(),"pendingPreview":pending.filter(|plan| now <= plan.expires).map(|plan|view(&plan)),"recentError":load::<Json>(data,LAST_ERROR).ok().flatten(),"runtimeVerified":false,"dns":"unknown","webrtc":"unknown","udp":"unknown","ednsEcs":"unknown","ipv6":"unknown"}),
            )
        }
        Command::Preview { options } => {
            validate(&options)?;
            if load::<Plan>(data, JOURNAL)?.is_some() {
                return Err("BROWSER_RESTORE_REQUIRED");
            }
            let changes = if options.scope == Scope::Global {
                edge_changes(host, &options)?
            } else {
                Vec::new()
            };
            let plan = Plan {
                id: nonce.into(),
                expires: now.saturating_add(5 * 60 * 1000),
                options,
                before: load(data, STATE)?,
                changes,
            };
            save(data, PENDING, &plan)?;
            Ok(view(&plan))
        }
        Command::Apply { plan_id } => {
            if !local_confirmation {
                return Err("BROWSER_LOCAL_CONFIRMATION_REQUIRED");
            }
            let plan: Plan = load(data, PENDING)?.ok_or("BROWSER_PREVIEW_REQUIRED")?;
            if plan.id != plan_id || now > plan.expires {
                return Err("BROWSER_PREVIEW_EXPIRED");
            }
            if load::<Plan>(data, JOURNAL)?.is_some()
                || load::<Options>(data, STATE)? != plan.before
            {
                return Err("BROWSER_STATE_CONFLICT");
            }
            for change in &plan.changes {
                if host.read(change.policy, true)?.is_some()
                    || host.read(change.policy, false)? != change.before
                {
                    return Err("BROWSER_POLICY_CONFLICT");
                }
            }
            save(data, JOURNAL, &plan)?;
            remove(data, PENDING)?;
            let result = (|| {
                for change in &plan.changes {
                    if host.read(change.policy, true)?.is_some()
                        || host.read(change.policy, false)? != change.before
                    {
                        return Err("BROWSER_POLICY_CONFLICT");
                    }
                    if change.before.as_ref() != Some(&change.after) {
                        host.write(change.policy, Some(&change.after))?;
                        if host.read(change.policy, false)?.as_ref() != Some(&change.after) {
                            return Err("BROWSER_POLICY_VERIFY_FAILED");
                        }
                    }
                }
                save(data, STATE, &plan.options)
            })();
            if let Err(code) = result {
                return Err(if restore(data, host).is_ok() {
                    code
                } else {
                    "BROWSER_RECOVERY_REQUIRED"
                });
            }
            Ok(
                json!({"saved":true,"runtimeVerified":false,"requiresBrowserRestart":true,"canRestore":true}),
            )
        }
        Command::Restore => {
            if !local_confirmation {
                return Err("BROWSER_LOCAL_CONFIRMATION_REQUIRED");
            }
            restore(data, host)
        }
    }
}
