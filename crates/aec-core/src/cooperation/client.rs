//! CVR's already-registered subscription script is the only writable client surface.
//! No subscription rewrite, profiles registration rewrite, activation, or lifecycle call.
use super::{client_script, Command, Selection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const PENDING: &str = "cooperation-preview.json";
const OWNED: &str = "cooperation-owned.json";
const ERROR: &str = "cooperation-error.json";
const MARKER: &str = "/* AEC managed footer v1 */";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Registration {
    profile: String,
    uid: String,
    file: String,
}
#[derive(Serialize, Deserialize)]
struct Preview {
    id: String,
    expires: u64,
    registration: Registration,
    hash: String,
    selection: Selection,
}
#[derive(Serialize, Deserialize)]
struct Owned {
    registration: Registration,
    original: String,
    footer: String,
    selection: Selection,
}

pub struct Manager<'a> {
    data: &'a Path,
    root: &'a Path,
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn read(path: &Path) -> Result<String, &'static str> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "COOP_FILE_UNAVAILABLE")?;
    use std::os::windows::fs::MetadataExt;
    if !metadata.is_file() || metadata.len() > 256 * 1024 || metadata.file_attributes() & 0x400 != 0
    {
        return Err("COOP_FILE_UNSAFE");
    }
    std::fs::read_to_string(path).map_err(|_| "COOP_FILE_UNAVAILABLE")
}
pub(super) fn yaml(path: &Path) -> Result<serde_yaml_ng::Value, &'static str> {
    serde_yaml_ng::from_str(&read(path)?).map_err(|_| "COOP_CLIENT_CONFIG_INVALID")
}
fn save(data: &Path, name: &str, value: &impl Serialize) -> Result<(), &'static str> {
    crate::store::write_atomic(
        &data.join(name),
        &serde_json::to_vec(value).map_err(|_| "COOP_STATE_INVALID")?,
    )
    .map_err(|_| "COOP_STATE_WRITE_FAILED")
}
fn load<T: serde::de::DeserializeOwned>(
    data: &Path,
    name: &str,
) -> Result<Option<T>, &'static str> {
    if !data.join(name).exists() {
        return Ok(None);
    }
    serde_json::from_str(&read(&data.join(name))?)
        .map(Some)
        .map_err(|_| "COOP_STATE_INVALID")
}
fn remove(data: &Path, name: &str) -> Result<(), &'static str> {
    match std::fs::remove_file(data.join(name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("COOP_STATE_WRITE_FAILED"),
    }
}
pub fn record_failure(data: &Path, code: &'static str) {
    let _ = save(
        data,
        ERROR,
        &json!({"action":"command","code":code,"atUnixMs":now()}),
    );
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 100
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
fn default_script(text: &str) -> bool {
    // Arbitrary JS cannot be proven safe without the client's JS runtime. Never guess at it.
    let mut text = text.trim_start_matches('\u{feff}').trim_ascii_start();
    if text.starts_with("// Define main function (script entry)") {
        let Some((comment, rest)) = text.split_once('\n') else {
            return false;
        };
        if comment.trim_ascii_end() != "// Define main function (script entry)" {
            return false;
        }
        text = rest;
    }
    if !text.is_ascii() {
        return false;
    }
    if let Some((_, rest)) = text.split_once("return") {
        let gap: String = rest.chars().take_while(char::is_ascii_whitespace).collect();
        if gap.contains(['\r', '\n']) {
            return false;
        }
    }
    // Keep identifier token boundaries. Removing all whitespace would accept invalid
    // `functionmain`/`returnconfig` syntax, which CVR silently falls back from.
    let normalized = text.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
    [
        "function main(config) { return config; }",
        "function main(config, profileName) { return config; }",
        "function main(config){return config;}",
        "function main(config,profileName){return config;}",
    ]
    .contains(&normalized.as_str())
}
impl<'a> Manager<'a> {
    pub fn new(data: &'a Path, client_root: &'a Path) -> Self {
        Self {
            data,
            root: client_root,
        }
    }
    fn registration(&self) -> Result<Registration, &'static str> {
        // Reject junctions/symlinks as well as traversal. AppData discovery is done by the owner.
        use std::os::windows::fs::MetadataExt;
        for component in [self.root.to_path_buf(), self.root.join("profiles")] {
            let meta = std::fs::symlink_metadata(component).map_err(|_| "COOP_CLIENT_NOT_FOUND")?;
            if !meta.is_dir() || meta.file_attributes() & 0x400 != 0 {
                return Err("COOP_FILE_UNSAFE");
            }
        }
        let profiles = yaml(&self.root.join("profiles.yaml"))?;
        let current = profiles["current"]
            .as_str()
            .filter(|s| safe_name(s))
            .ok_or("COOP_PROFILE_INVALID")?;
        let items = profiles["items"]
            .as_sequence()
            .ok_or("COOP_PROFILE_INVALID")?;
        let unique = |id: &str| {
            let matches: Vec<_> = items
                .iter()
                .filter(|v| v["uid"].as_str() == Some(id))
                .collect();
            if matches.len() == 1 {
                Ok(matches[0])
            } else {
                Err("COOP_REGISTRATION_AMBIGUOUS")
            }
        };
        let profile = unique(current)?;
        if !matches!(profile["type"].as_str(), Some("remote" | "local")) {
            return Err("COOP_PROFILE_INVALID");
        }
        let uid = profile["option"]["script"]
            .as_str()
            .filter(|s| safe_name(s))
            .ok_or("COOP_SCRIPT_NOT_REGISTERED")?;
        if uid.eq_ignore_ascii_case("script") || uid.eq_ignore_ascii_case("merge") {
            return Err("COOP_SCRIPT_SHARED");
        }
        if items
            .iter()
            .filter(|v| v["option"]["script"].as_str() == Some(uid))
            .count()
            != 1
        {
            return Err("COOP_SCRIPT_SHARED");
        }
        let script = unique(uid)?;
        let file = script["file"]
            .as_str()
            .ok_or("COOP_SCRIPT_NOT_REGISTERED")?;
        if script["type"].as_str() != Some("script")
            || !file.ends_with(".js")
            || !safe_name(file.trim_end_matches(".js"))
        {
            return Err("COOP_FILE_UNSAFE");
        }
        let verge = yaml(&self.root.join("verge.yaml"))?;
        let boolean = |v: &serde_yaml_ng::Value| {
            if v.is_null() {
                Ok(None)
            } else {
                v.as_bool().map(Some).ok_or("COOP_DNS_OVERRIDE_UNKNOWN")
            }
        };
        let override_enabled = boolean(&verge["profile_dns_settings"][current]["enabled"])?
            .or(boolean(&verge["enable_dns_settings"])?)
            .unwrap_or(false);
        // GUI DNS settings run after subscription scripts and may replace the whole policy map.
        if override_enabled {
            return Err("COOP_GUI_DNS_OVERRIDE");
        }
        Ok(Registration {
            profile: current.into(),
            uid: uid.into(),
            file: file.into(),
        })
    }
    fn path(&self, r: &Registration) -> Result<PathBuf, &'static str> {
        if !safe_name(&r.uid)
            || !safe_name(&r.profile)
            || !r.file.ends_with(".js")
            || !safe_name(r.file.trim_end_matches(".js"))
        {
            return Err("COOP_STATE_INVALID");
        }
        Ok(self.root.join("profiles").join(&r.file))
    }
    fn state(&self, selection: &Selection) -> Result<Value, &'static str> {
        let owned: Option<Owned> = load(self.data, OWNED)?;
        let installed = if let Some(o) = &owned {
            self.registration().is_ok_and(|r| r == o.registration)
                && self
                    .path(&o.registration)
                    .and_then(|p| read(&p))
                    .is_ok_and(|v| v == format!("{}{}", o.original, o.footer))
                && o.selection == *selection
        } else {
            false
        };
        Ok(
            json!({"persistentInstalled":installed,"installedByAec":owned.is_some(),"canRestore":owned.is_some(),
            "runtimeVerified":false,"status":if installed {"installed_pending_runtime"} else {"not_verified"},
            "activation":"manual_client_reload","clientContract":"Clash Verge Rev 2.5.8 subscription script",
            "recentError":load::<Value>(self.data,ERROR)?,
            "lastRuntimeReport":load::<Value>(self.data,"cooperation-runtime.json")?,
            "lastDnsReport":load::<Value>(self.data,"cooperation-dns.json")?,
            "limitations":["仅自动安装已注册且独占的默认订阅脚本；已有自定义 JS 请手动合并导出块", "保存不代表运行生效；请在 Clash 中手动重新激活当前配置，再运行核对", "规则模式限定；全局/DIRECT/TUN/其他客户端及绕过系统代理不在保护范围", "AEC 不重载客户端，不改系统代理，不关闭任何服务"]}),
        )
    }
    pub fn execute(&self, command: Command, selection: &Selection) -> Result<Value, &'static str> {
        let action = match &command {
            Command::State => "state",
            Command::Preview => "preview",
            Command::Install { .. } => "install",
            Command::Restore => "restore",
            Command::Verify => "verify",
            Command::DnsProbe { .. } => "dns_probe",
        };
        let result = self.run(command, selection);
        if let Err(code) = &result {
            let _ = save(
                self.data,
                ERROR,
                &json!({"action":action,"code":code,"atUnixMs":now()}),
            );
        }
        result
    }
    fn run(&self, command: Command, selection: &Selection) -> Result<Value, &'static str> {
        match command {
            Command::State => self.state(selection),
            Command::Preview => {
                super::client_script(selection)?;
                if load::<Owned>(self.data, OWNED)?.is_some() {
                    return Err("COOP_RESTORE_BEFORE_CHANGE");
                }
                let registration = self.registration()?;
                let path = self.path(&registration)?;
                if !default_script(&read(&path)?) {
                    return Err("COOP_CUSTOM_SCRIPT_MANUAL_MERGE");
                }
                let plan = Preview {
                    id: crate::crypto::random_hex(16).map_err(|_| "COOP_RANDOM_FAILED")?,
                    expires: now() + 300_000,
                    hash: crate::crypto::sha256_file(&path).map_err(|_| "COOP_FILE_UNAVAILABLE")?,
                    registration,
                    selection: selection.clone(),
                };
                save(self.data, PENDING, &plan)?;
                Ok(
                    json!({"planId":plan.id,"domains":selection.domains,"fixedPort":selection.fixed_port,"activation":"manual_client_reload",
                    "runtimeVerified":false,"expiresAtUnixMs":plan.expires,"changes":["只向当前订阅已注册的默认脚本追加 AEC 规则块", "保留订阅、节点、选择和系统代理；不自动激活或停服"],"restore":"移除自己的完整尾块；外部改动时拒绝覆盖"}),
                )
            }
            Command::Install { plan_id } => {
                let plan: Preview = load(self.data, PENDING)?.ok_or("COOP_PREVIEW_REQUIRED")?;
                if plan.id != plan_id || plan.expires < now() || plan.selection != *selection {
                    return Err("COOP_PREVIEW_STALE");
                }
                if self.registration()? != plan.registration {
                    return Err("COOP_PREVIEW_STALE");
                }
                let path = self.path(&plan.registration)?;
                let original = read(&path)?;
                if !default_script(&original)
                    || crate::crypto::sha256_file(&path).map_err(|_| "COOP_FILE_UNAVAILABLE")?
                        != plan.hash
                {
                    return Err("COOP_PREVIEW_STALE");
                }
                if load::<Owned>(self.data, OWNED)?.is_some() {
                    return Err("COOP_RESTORE_BEFORE_CHANGE");
                }
                let footer = format!(
                    "\n{MARKER}\n{}\n/* End AEC managed footer */\n",
                    client_script(selection)?
                );
                let owned = Owned {
                    registration: plan.registration,
                    original: original.clone(),
                    footer: footer.clone(),
                    selection: selection.clone(),
                };
                // Journal contains only the proven credential-free default template and our own block.
                save(self.data, OWNED, &owned)?;
                if read(&path)? != original {
                    return Err("COOP_EXTERNAL_CHANGE");
                }
                crate::store::write_atomic(&path, format!("{original}{footer}").as_bytes())
                    .map_err(|_| "COOP_INSTALL_FAILED")?;
                remove(self.data, PENDING)?;
                self.state(selection)
            }
            Command::Restore => {
                let owned: Owned = load(self.data, OWNED)?.ok_or("COOP_NOT_OWNED")?;
                let path = self.path(&owned.registration)?;
                let actual = read(&path)?;
                if actual != format!("{}{}", owned.original, owned.footer)
                    && actual != owned.original
                {
                    return Err("COOP_EXTERNAL_CHANGE");
                }
                // No default script or owned markers from a forged/malformed journal are trusted.
                if !default_script(&owned.original)
                    || owned.footer
                        != format!(
                            "\n{MARKER}\n{}\n/* End AEC managed footer */\n",
                            client_script(&owned.selection)?
                        )
                {
                    return Err("COOP_STATE_INVALID");
                }
                crate::store::write_atomic(&path, owned.original.as_bytes())
                    .map_err(|_| "COOP_RESTORE_FAILED")?;
                remove(self.data, OWNED)?;
                remove(self.data, PENDING)?;
                self.state(selection)
            }
            Command::Verify => {
                let runtime = super::windows::ClientRuntime::open(self.root)?;
                let evidence = super::verify_runtime(&runtime, selection);
                save(
                    self.data,
                    "cooperation-runtime.json",
                    &json!({"atUnixMs":now(),"evidence":evidence}),
                )?;
                Ok(evidence)
            }
            Command::DnsProbe { consent } => {
                if !consent {
                    return Err("COOP_DNS_CONSENT_REQUIRED");
                }
                use super::RuntimeHost;
                let runtime = super::windows::ClientRuntime::open(self.root)?;
                let nonce = crate::crypto::random_hex(4).map_err(|_| "COOP_RANDOM_FAILED")?;
                let started = std::time::Instant::now();
                let ordinary = runtime.get(&format!(
                    "/dns/query?name={nonce}.test.dnscheck.tools&type=TXT"
                ));
                let mut ordinary = ordinary
                    .map(|reply| {
                        super::evaluate_dns(&reply, &nonce, started.elapsed().as_millis() as u64)
                    })
                    .unwrap_or_else(|code| json!({"status":"unknown","code":code}));
                ordinary["scope"] = "current_client_resolver_diagnostic_not_selected_site".into();
                let nonce = crate::crypto::random_hex(4).map_err(|_| "COOP_RANDOM_FAILED")?;
                let fixed = super::probe_fixed_dns(
                    &crate::http::WinHttpClient,
                    selection.fixed_port,
                    &nonce,
                );
                let report = json!({"atUnixMs":now(),"clientResolver":ordinary,"fixedDoh":fixed,
                    "selectedSiteDns":"unknown","browserDns":"unknown","message":"两次诊断分别覆盖当前客户端解析器和显式固定入口 DoH；不扩大永久选定站点，不代表浏览器所有 DNS。"});
                save(self.data, "cooperation-dns.json", &report)?;
                Ok(report)
            }
        }
    }
}
