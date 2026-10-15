//! Local Agent control: one typed contract for HTTP and MCP, backed by the tray worker.
use crate::setup::SetupBackend;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const DISCOVERY: &str = "agent-api.json";
pub const ACCESS: &str = "agent-access.json";
const LIMIT: usize = 128 * 1024;

pub fn enabled(data: &Path) -> bool {
    std::fs::read(data.join(ACCESS))
        .ok()
        .and_then(|v| serde_json::from_slice::<bool>(&v).ok())
        .unwrap_or(false)
}

pub(crate) struct Auth {
    pub token: String,
    pub data: PathBuf,
}
impl Auth {
    pub fn enabled(&self) -> bool {
        enabled(&self.data)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Discovery {
    pub schema_version: u32,
    pub port: u16,
    pub credential_ref: String,
    pub pid: u32,
}

/// The token is session-scoped and exists only in Credential Manager and process memory.
#[cfg(windows)]
pub struct Server {
    server: Option<crate::setup::SetupServer>,
    data: PathBuf,
    reference: String,
}
#[cfg(windows)]
impl Server {
    pub fn start(data: &Path, backend: Arc<dyn SetupBackend>) -> Result<Self, String> {
        let token = crate::crypto::random_hex(32).map_err(|_| "随机令牌生成失败")?;
        let reference = format!(
            "agent-api-{}",
            crate::crypto::random_hex(16).map_err(|_| "随机凭据名称生成失败")?
        );
        let server = crate::setup::SetupServer::start_agent(
            backend,
            Auth {
                token: token.clone(),
                data: data.into(),
            },
        )
        .map_err(|_| "Agent 接口监听失败".to_string())?;
        crate::credentials::WindowsCredentials.store(&reference, "AEC local API", &token)?;
        let result = Self {
            server: Some(server),
            data: data.into(),
            reference,
        };
        let discovery = Discovery {
            schema_version: 1,
            port: result.server.as_ref().unwrap().port(),
            credential_ref: result.reference.clone(),
            pid: std::process::id(),
        };
        std::fs::write(
            data.join(DISCOVERY),
            serde_json::to_vec(&discovery).expect("serializes"),
        )
        .map_err(|_| "Agent 接口发现文件写入失败".to_string())?;
        Ok(result)
    }
}
#[cfg(windows)]
impl Drop for Server {
    fn drop(&mut self) {
        self.server.take();
        // Only remove our own discovery; a stale server must not erase a newer session.
        let ours = std::fs::read(self.data.join(DISCOVERY))
            .ok()
            .and_then(|v| serde_json::from_slice::<Discovery>(&v).ok())
            .is_some_and(|d| d.credential_ref == self.reference);
        if ours {
            let _ = std::fs::remove_file(self.data.join(DISCOVERY));
        }
        let _ = crate::credentials::WindowsCredentials.delete(&self.reference);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub name: String,
    #[serde(default = "empty")]
    pub arguments: Value,
}
fn empty() -> Value {
    json!({})
}
fn invalid() -> String {
    "invalid arguments; use aec_options and the tool schema".into()
}
fn fields(errors: Vec<crate::profile::FieldError>) -> String {
    // Validation can include user-supplied values. Never echo an arbitrary rejected payload.
    errors
        .iter()
        .map(|e| e.path.as_str())
        .collect::<Vec<_>>()
        .join(", ")
        + ": validation failed; check the local panel"
}
fn settings_properties() -> Value {
    let mut p = serde_json::Map::new();
    for key in [
        "coreMixedPort",
        "host",
        "ports",
        "username",
        "tunnelServers",
        "localProxy",
        "targets",
        "extraDomains",
    ] {
        p.insert(key.into(), json!({"type":"string"}));
    }
    for key in ["tunnelEnabled", "tunnelDirectFallback", "domesticDirect"] {
        p.insert(key.into(), json!({"type":"boolean"}));
    }
    for key in ["maxInflight", "connectTimeoutMs", "dialBudgetMs"] {
        p.insert(
            key.into(),
            json!({"type":"integer","minimum":0,"maximum":u32::MAX}),
        );
    }
    for key in ["proto", "localProxyProto"] {
        p.insert(
            key.into(),
            json!({"type":"string","enum":["socks5","http"]}),
        );
    }
    p.insert("timezone".into(), json!({"type":"string","enum":crate::setup::TIMEZONES.iter().map(|t|t.0).collect::<Vec<_>>() }));
    p.insert("language".into(), json!({"type":"string","enum":crate::setup::LANGUAGES.iter().map(|t|t.0).collect::<Vec<_>>() }));
    p.insert(
        "routingMode".into(),
        json!({"type":"string","enum":["all","split","custom"]}),
    );
    p.insert("tunnelPreset".into(), json!({"type":"string","enum":std::iter::once("custom").chain(crate::tunnel::PRESETS.iter().map(|p|p.key)).collect::<Vec<_>>() }));
    p.insert(
        "tunnelCipher".into(),
        json!({"type":"string","enum":crate::profile::TUNNEL_CIPHERS}),
    );
    Value::Object(p)
}
fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
pub fn tools() -> Value {
    let none = schema(json!({}), &[]);
    let rule = schema(
        json!({
            "kind":{"type":"string","enum":["domain","domain_suffix","domain_keyword","ip_cidr","geosite","geoip"]},
            "value":{"type":"string"},"target":{"type":"string"},"noResolve":{"type":"boolean"},"note":{"type":["string","null"]}
        }),
        &["kind", "value", "target"],
    );
    let specs = vec![
        ("aec_state", "Read mode, readiness summary, fixed-exit block, paths and rules. Unknown is never pass.", none.clone(), true),
        ("aec_settings", "Read settings without passwords. Do not publish private hosts, usernames or exit IP.", none.clone(), true),
        ("aec_options", "Read capabilities, dropdown options, HTTP contract and security guidance.", none.clone(), true),
        ("aec_configure", "Patch settings in Daily mode. User enters passwords in panel. Never disable protection or change expected IP without explicit user direction.", schema(settings_properties(), &[]), false),
        ("aec_rules", "Replace ordered rules and protected sites. Removing a site removes its protection; confirm the user's intended scope.", schema(json!({"sites":{"type":"array","items":{"type":"string"},"maxItems":128},"rules":{"type":"array","items":rule,"maxItems":512},"finalTarget":{"type":"string"}}), &["sites","rules","finalTarget"]), false),
        ("aec_route", "Select a listed route group member. Non-fixed members REMOVE fixed protection only for the group's explicit domain-suffix rules; fixed members restore it. Requires user intent for that domain scope. Connections are interrupted; inspect returned protection/readiness.", schema(json!({"group":{"type":"string"},"member":{"type":"string"}}), &["group","member"]), false),
        ("aec_path", "Choose a path listed by aec_state; disconnect, verify the SAME configured ISP IP, then release. Failure stays blocked.", schema(json!({"path":{"type":"string"}}), &["path"]), false),
        ("aec_startup", "Enable or disable this installation's login startup.", schema(json!({"enabled":{"type":"boolean"}}), &["enabled"]), false),
        ("aec_shortcuts", "Create desktop and Start menu shortcuts for this installation.", none.clone(), false),
        ("aec_update", "Launch the official Release updater. User must approve download and any core-service interruption in a local dialog. No argument can bypass confirmation. uiOnly never replaces networking binaries.", schema(json!({"uiOnly":{"type":"boolean"}}), &["uiOnly"]), false),
        ("aec_enter", "Enter Agent mode using existing takeover gates; returns final state, not a guarantee of readiness.", none.clone(), false),
        ("aec_exit", "Exit and restore snapshotted globals; connections may stop. Inspect returned state for recovery failures.", none.clone(), false),
        ("aec_recheck", "Run checks and return state; failure or unknown never becomes ready.", none, false),
    ];
    Value::Array(specs.into_iter().map(|(name,description,input,readonly)| json!({"name":name,"description":description,"inputSchema":input,"annotations":{"readOnlyHint":readonly,"destructiveHint":!readonly,"openWorldHint":!readonly}})).collect())
}

pub fn call(backend: &dyn SetupBackend, request: Call) -> Result<Value, String> {
    let a = request.arguments;
    let spec = tools()
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == request.name)
        .cloned()
        .ok_or_else(|| "unknown tool".to_string())?;
    let obj = a.as_object().ok_or_else(invalid)?;
    if !matches_schema(&a, &spec["inputSchema"]) {
        return Err(invalid());
    }
    match request.name.as_str() {
        "aec_state" => {
            return serde_json::to_value(backend.dashboard())
                .map_err(|_| "state unavailable".into())
        }
        "aec_settings" => {
            return serde_json::to_value(backend.view()).map_err(|_| "settings unavailable".into())
        }
        "aec_options" => {
            return Ok(
                json!({"apiVersion":1,"tools":tools(),"timezones":crate::setup::TIMEZONES,"languages":crate::setup::LANGUAGES,"tunnelPresets":crate::tunnel::PRESETS.iter().map(|p|json!({"key":p.key,"label":p.label,"source":p.source})).collect::<Vec<_>>(),"security":"Loopback only; same Windows user; passwords are entered by the user in panel. Global VPN/TUN and competing system proxy unsupported. No readiness guarantee without evidence."}),
            )
        }
        "aec_configure" => {
            backend.configure(a).map_err(fields)?;
        }
        "aec_rules" => backend
            .rules(serde_json::from_value(a).map_err(|_| invalid())?)
            .map_err(fields)?,
        "aec_route" => backend
            .route(serde_json::from_value(a).map_err(|_| invalid())?)
            .map_err(fields)?,
        "aec_path" => backend
            .path(
                obj.get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?
                    .into(),
            )
            .map_err(fields)?,
        "aec_startup" => backend
            .startup(
                obj.get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(invalid)?,
            )
            .map_err(fields)?,
        "aec_shortcuts" => backend.shortcuts().map_err(fields)?,
        "aec_update" => {
            backend
                .update(obj["uiOnly"].as_bool().ok_or_else(invalid)?)
                .map_err(fields)?;
            return Ok(json!({"launched":true,"completed":false,"requiresUserConfirmation":true}));
        }
        "aec_enter" | "aec_exit" | "aec_recheck" => backend
            .action(request.name.trim_start_matches("aec_"))
            .map_err(fields)?,
        _ => return Err("unknown tool".into()),
    }
    Ok(json!({"completed":true,"state":backend.dashboard()}))
}

/// Validate the subset of JSON Schema emitted by this module at both transports.
fn matches_schema(value: &Value, spec: &Value) -> bool {
    let kind = |s: &str| match s {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_u64() || value.is_i64(),
        "null" => value.is_null(),
        _ => false,
    };
    let valid_type = spec["type"].as_str().map_or_else(
        || {
            spec["type"]
                .as_array()
                .is_some_and(|types| types.iter().any(|t| t.as_str().is_some_and(kind)))
        },
        kind,
    );
    if !valid_type {
        return false;
    }
    if let Some(options) = spec["enum"].as_array() {
        if !options.contains(value) {
            return false;
        }
    }
    if let Some(v) = value.as_object() {
        let Some(props) = spec["properties"].as_object() else {
            return false;
        };
        if spec["required"].as_array().is_some_and(|required| {
            required
                .iter()
                .any(|r| !v.contains_key(r.as_str().unwrap()))
        }) {
            return false;
        }
        if v.iter()
            .any(|(k, v)| props.get(k).is_none_or(|s| !matches_schema(v, s)))
        {
            return false;
        }
    }
    if let Some(v) = value.as_array() {
        if spec["maxItems"]
            .as_u64()
            .is_some_and(|max| v.len() as u64 > max)
            || v.iter().any(|v| !matches_schema(v, &spec["items"]))
        {
            return false;
        }
    }
    if let Some(n) = value.as_f64() {
        if spec["minimum"].as_f64().is_some_and(|min| n < min)
            || spec["maximum"].as_f64().is_some_and(|max| n > max)
        {
            return false;
        }
    }
    true
}

pub fn patch_settings(
    view: crate::setup::SetupView,
    patch: Value,
) -> Result<crate::setup::ExitForm, Vec<crate::profile::FieldError>> {
    let error = || {
        vec![crate::profile::FieldError {
            path: "settings".into(),
            message: "无效的配置字段，请按接口选项填写".into(),
        }]
    };
    if !matches_schema(&patch, &schema(settings_properties(), &[])) {
        return Err(error());
    }
    let mut current = serde_json::to_value(view).map_err(|_| error())?;
    let base = current.as_object_mut().unwrap();
    for key in ["locked", "passwordStored", "tunnelPasswordStored"] {
        base.remove(key);
    }
    for (key, value) in patch.as_object().unwrap() {
        base.insert(key.clone(), value.clone());
    }
    serde_json::from_value(current).map_err(|_| error())
}

pub fn client(port: u16, token: &str, request: &Value) -> Result<Value, String> {
    let mut s = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(3))
        .map_err(|_| "AEC tray unavailable; start it and enable Agent access".to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(180)))
        .map_err(|_| "transport error")?;
    s.set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| "transport error")?;
    let body = request.to_string();
    write!(s,"POST /api/v1/call HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).map_err(|_| "transport write failed")?;
    let mut bytes = Vec::new();
    s.take((LIMIT * 8 + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            "operation result unknown (timeout); read aec_state before retrying any mutation"
                .to_string()
        })?;
    if bytes.len() > LIMIT * 8 {
        return Err("response too large".into());
    }
    let end = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("invalid HTTP response")?;
    if !bytes.starts_with(b"HTTP/1.1 200 ") {
        return Err("Agent access denied; enable it in the local panel".into());
    }
    let response: Value =
        serde_json::from_slice(&bytes[end + 4..]).map_err(|_| "invalid response")?;
    if response["ok"] == true {
        Ok(response["result"].clone())
    } else {
        Err(response["error"]
            .as_str()
            .unwrap_or("operation failed")
            .into())
    }
}

#[cfg(windows)]
pub fn local_call(data: &Path, request: &Value) -> Result<Value, String> {
    use crate::core::config::CredentialSource;
    if !enabled(data) {
        return Err("Agent access disabled; enable it in dashboard → Agent 接入".into());
    }
    let bytes = std::fs::read(data.join(DISCOVERY))
        .map_err(|_| "AEC tray is not running or needs upgrade")?;
    if bytes.len() > 4096 {
        return Err("invalid discovery".into());
    }
    let d: Discovery = serde_json::from_slice(&bytes).map_err(|_| "invalid discovery")?;
    if d.schema_version != 1 || d.port == 0 || !d.credential_ref.starts_with("agent-api-") {
        return Err("unsupported discovery".into());
    }
    let (_, token) = crate::credentials::WindowsCredentials
        .get(&d.credential_ref)
        .ok_or("authentication unavailable; restart tray")?;
    client(d.port, &token, request)
}

/// Newline-delimited JSON-RPC stdio. Never write diagnostics or credentials to stdout.
pub fn serve_mcp(
    input: impl BufRead,
    mut output: impl Write,
    mut invoke: impl FnMut(&Value) -> Result<Value, String>,
) -> std::io::Result<()> {
    let mut initialized = false;
    let mut ready = false;
    let mut input = input;
    loop {
        let mut bytes = Vec::new();
        let n = input
            .by_ref()
            .take((LIMIT + 1) as u64)
            .read_until(b'\n', &mut bytes)?;
        if n == 0 {
            return Ok(());
        }
        if n > LIMIT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MCP message too large",
            ));
        }
        let parsed = serde_json::from_slice::<Value>(&bytes);
        bytes.fill(0);
        let response = match parsed {
            Err(_) => {
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
            }
            Ok(r) => {
                let valid = r.is_object() && r["jsonrpc"] == "2.0" && r["method"].is_string();
                let id = r.get("id").cloned();
                if valid
                    && id.is_none()
                    && r["method"].as_str().unwrap().starts_with("notifications/")
                {
                    if r["method"] == "notifications/initialized" && initialized {
                        ready = true;
                    }
                    continue;
                }
                let has_id = id.is_some();
                let id = id.unwrap_or(Value::Null);
                let method = r["method"].as_str().unwrap_or("");
                let result = if !valid || !has_id || !(id.is_string() || id.is_number()) {
                    Err((-32600, "Invalid Request"))
                } else if method == "initialize"
                    && !initialized
                    && r["params"]["protocolVersion"].as_str().is_some_and(|v| {
                        ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"].contains(&v)
                    })
                {
                    initialized = true;
                    let version = r["params"]["protocolVersion"].as_str().unwrap();
                    Ok(
                        json!({"protocolVersion":if ["2025-11-25","2025-06-18","2025-03-26","2024-11-05"].contains(&version) {version} else {"2025-11-25"},"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"agent-egress-controller","version":env!("CARGO_PKG_VERSION")},"instructions":"Start tray, enable Agent access in panel. Use aec_options then aec_state. User enters passwords in local panel. Mutations affect networking; preserve expected ISP IP and takeover gates. A completed command is not verified readiness."}),
                    )
                } else if method == "initialize" && !initialized {
                    Err((
                        -32602,
                        "Unsupported protocol version or invalid initialize params",
                    ))
                } else if method == "ping" {
                    Ok(json!({}))
                } else if !ready {
                    Err((-32000, "Initialize first"))
                } else if method == "tools/list" {
                    Ok(json!({"tools":tools()}))
                } else if method == "tools/call" {
                    if serde_json::from_value::<Call>(r["params"].clone()).is_err() {
                        Err((-32602, "Invalid params"))
                    } else {
                        Ok(match invoke(&r["params"]) {
                            Ok(v) => {
                                json!({"content":[{"type":"text","text":v.to_string()}],"isError":false})
                            }
                            Err(e) => json!({"content":[{"type":"text","text":e}],"isError":true}),
                        })
                    }
                } else {
                    Err((-32601, "Method not found"))
                };
                match result {
                    Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                    Err((code, message)) => {
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
                    }
                }
            }
        };
        writeln!(output, "{response}")?;
        output.flush()?;
    }
}

pub fn page(token: &str, info: &Value) -> String {
    crate::ui::template(
        "agent.html",
        include_str!("../../../ui/revisions/fluent-1/agent.html"),
    )
    .replace("__TOKEN__", token)
    .replace("__INFO__", &info.to_string().replace("</", "<\\/"))
}

pub fn config(data: &Path, exe: &Path) -> Value {
    json!({"mcpServers":{"agent-egress-controller":{"command":exe,"args":["--data-dir",data,"mcp"]}}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{FieldError, ProfileSet};
    use crate::setup::{ExitForm, SetupBackend, SetupView, ViewInput};
    use std::sync::Mutex;

    struct Fake(Mutex<Vec<ExitForm>>);
    impl SetupBackend for Fake {
        fn view(&self) -> SetupView {
            crate::setup::view(
                &ProfileSet::default(),
                &ViewInput {
                    core_mixed_port: None,
                    stored_user: None,
                    tunnel_password_stored: false,
                    takeover_enabled: false,
                    locked: false,
                },
            )
        }
        fn save(&self, f: ExitForm) -> Result<(), Vec<FieldError>> {
            self.0.lock().unwrap().push(f);
            Ok(())
        }
    }

    #[test]
    fn update_tool_cannot_bypass_user_confirmation_or_claim_completion() {
        struct Updater(std::sync::Mutex<Vec<bool>>);
        impl SetupBackend for Updater {
            fn view(&self) -> SetupView {
                panic!("not used")
            }
            fn save(&self, _form: ExitForm) -> Result<(), Vec<crate::profile::FieldError>> {
                panic!("not used")
            }
            fn update(&self, ui_only: bool) -> Result<(), Vec<crate::profile::FieldError>> {
                self.0.lock().unwrap().push(ui_only);
                Ok(())
            }
        }
        let updater = Updater(std::sync::Mutex::new(Vec::new()));
        for arguments in [
            json!({}),
            json!({"uiOnly":true,"confirmCore":true}),
            json!({"uiOnly":"false"}),
        ] {
            assert!(call(
                &updater,
                Call {
                    name: "aec_update".into(),
                    arguments
                }
            )
            .is_err());
        }
        assert!(updater.0.lock().unwrap().is_empty());
        let result = call(
            &updater,
            Call {
                name: "aec_update".into(),
                arguments: json!({"uiOnly":true}),
            },
        )
        .unwrap();
        assert_eq!(
            result,
            json!({"launched":true,"completed":false,"requiresUserConfirmation":true})
        );
        assert_eq!(*updater.0.lock().unwrap(), vec![true]);
    }

    #[test]
    fn schemas_are_strict_and_passwords_are_not_agent_fields() {
        let fake = Fake(Mutex::new(Vec::new()));
        for args in [
            json!({"systemProxy":true}),
            json!({"takeoverEnabled":true}),
            json!({"expectedIp":"198.51.100.7"}),
            json!({"password":"secret"}),
        ] {
            assert!(call(
                &fake,
                Call {
                    name: "aec_configure".into(),
                    arguments: args
                }
            )
            .is_err());
        }
        let config = Call {
            name: "aec_configure".into(),
            arguments: json!({"language":"xx"}),
        };
        assert!(call(&fake, config).is_err());
        let tools = tools();
        assert!(!tools.to_string().contains("password\""));
        assert!(fake.0.lock().unwrap().is_empty());
    }

    #[test]
    fn route_tool_uses_the_shared_backend_and_rejects_unlisted_arguments() {
        struct Routes(Mutex<Vec<crate::route_switch::Form>>);
        impl SetupBackend for Routes {
            fn view(&self) -> SetupView {
                Fake(Mutex::new(Vec::new())).view()
            }
            fn save(&self, _: ExitForm) -> Result<(), Vec<FieldError>> {
                unreachable!()
            }
            fn route(&self, form: crate::route_switch::Form) -> Result<(), Vec<FieldError>> {
                self.0.lock().unwrap().push(form);
                Ok(())
            }
        }
        let backend = Routes(Mutex::new(Vec::new()));
        for extra in [
            json!({"group":"GPT","member":"ordinary","force":true}),
            json!({"group":"GPT","member":"ordinary","expectedIp":"198.51.100.2"}),
        ] {
            assert!(call(
                &backend,
                Call {
                    name: "aec_route".into(),
                    arguments: extra
                }
            )
            .is_err());
        }
        call(
            &backend,
            Call {
                name: "aec_route".into(),
                arguments: json!({"group":"GPT","member":"ordinary"}),
            },
        )
        .unwrap();
        assert_eq!(backend.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn settings_patch_keeps_local_secrets_out_and_validates_options() {
        let fake = Fake(Mutex::new(Vec::new()));
        call(
            &fake,
            Call {
                name: "aec_configure".into(),
                arguments: json!({"language":"en-GB","tunnelDirectFallback":false}),
            },
        )
        .unwrap();
        let saved = fake.0.lock().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].language, "en-GB");
        assert!(!saved[0].tunnel_direct_fallback);
        assert_eq!(saved[0].password, None);
    }

    #[test]
    fn mcp_negotiates_then_exposes_tools_and_keeps_stdout_json_only() {
        let input=concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"aec_state\",\"arguments\":{}}}\n"
        );
        let mut output = Vec::new();
        serve_mcp(input.as_bytes(), &mut output, |request| {
            Ok(json!({"name":request["name"]}))
        })
        .unwrap();
        let lines = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(lines[1]["result"]["tools"][0]["name"], "aec_state");
        assert_eq!(
            lines[2]["result"]["content"][0]["text"],
            r#"{"name":"aec_state"}"#
        );
    }

    #[test]
    fn mcp_rejects_unsupported_protocol_version_and_requests_before_initialization() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"1900-01-01\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n"
        );
        let mut output = Vec::new();
        serve_mcp(input.as_bytes(), &mut output, |_| unreachable!()).unwrap();
        let lines = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|v| serde_json::from_str::<Value>(v).unwrap())
            .collect::<Vec<_>>();
        assert!(lines[0]["error"].is_object());
        assert!(lines[1]["error"].is_object());
    }

    #[test]
    fn mcp_rejects_oversized_frames_and_tools_do_not_accept_arbitrary_commands() {
        let too_big = format!("{}\n", " ".repeat(LIMIT + 1));
        assert_eq!(
            serve_mcp(too_big.as_bytes(), Vec::new(), |_| unreachable!())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(call(
            &Fake(Mutex::new(Vec::new())),
            Call {
                name: "exec".into(),
                arguments: json!({"command":"whoami"})
            }
        )
        .is_err());
    }
}
