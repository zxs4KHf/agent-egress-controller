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

use crate::profile::{self, Candidate, FieldError, ProfileSet, UpstreamProto};

/// The one credential the settings page manages.
pub const CREDENTIAL_REF: &str = "exit";
const MAX_BODY: usize = 16 * 1024;
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

/// What the page shows. No password, only whether one is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupView {
    pub host: String,
    pub proto: UpstreamProto,
    pub ports: String,
    pub username: String,
    pub password_stored: bool,
    pub timezone: String,
    pub language: String,
    pub expected_ip: String,
    pub takeover_enabled: bool,
    /// Agent mode is active: saving is refused until the user leaves it.
    pub locked: bool,
}

/// What the page sends. `password: None` (or empty) keeps the stored one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitForm {
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

/// The settings page's view of a profile set.
pub fn view(
    set: &ProfileSet,
    stored_user: Option<&str>,
    takeover_enabled: bool,
    locked: bool,
) -> SetupView {
    let p = &set.agent[0];
    let c = &p.egress.candidates;
    let tz = TIMEZONES
        .iter()
        .find(|t| t.2 == p.locale.timezone.windows_id && t.3 == p.locale.timezone.iana)
        .map_or("us-east", |t| t.0);
    SetupView {
        host: c.first().map(|c| c.host.clone()).unwrap_or_default(),
        proto: c.first().map_or(UpstreamProto::Socks5, |c| c.proto),
        ports: c
            .iter()
            .map(|c| c.port.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        username: stored_user.unwrap_or_default().to_string(),
        password_stored: stored_user.is_some(),
        timezone: tz.into(),
        language: p.locale.browser_lang.clone(),
        expected_ip: p.egress.expected.ip.clone().unwrap_or_default(),
        takeover_enabled,
        locked,
    }
}

fn parse_ports(s: &str) -> Result<Vec<u16>, FieldError> {
    let mut out = Vec::new();
    for part in s
        .split([',', ' ', '，', ';'])
        .filter(|p| !p.trim().is_empty())
    {
        let port: u16 = part.trim().parse().ok().filter(|p| *p > 0).ok_or_else(|| {
            err(
                "ports",
                &format!("「{}」不是 1–65535 的端口号", part.trim()),
            )
        })?;
        if !out.contains(&port) {
            out.push(port);
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

/// Apply a form to the first AgentProfile. On success the set is changed and the credential change
/// is returned for the caller to carry out; on failure the set is left as it was.
pub fn apply(
    set: &mut ProfileSet,
    form: &ExitForm,
    stored_user: Option<&str>,
) -> Result<(CredentialChange, bool), Vec<FieldError>> {
    let mut errors = Vec::new();
    let host = form.host.trim();
    if host.is_empty() {
        errors.push(err("host", "必须填写出口地址"));
    } else if host.contains("://") || host.contains('/') || host.contains(char::is_whitespace) {
        errors.push(err("host", "只填主机名或 IP，不要带 http:// 或路径"));
    } else if host.contains(':') && host.parse::<std::net::Ipv6Addr>().is_err() {
        errors.push(err("host", "端口填在下面的「端口」里，这里不要带冒号"));
    }
    let ports = parse_ports(&form.ports).map_err(|e| errors.push(e)).ok();
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
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut next = set.clone();
    let p = &mut next.agent[0];
    let credential_ref = (!user.is_empty()).then(|| CREDENTIAL_REF.to_string());
    p.egress.candidates = ports
        .expect("checked")
        .into_iter()
        .map(|port| Candidate {
            id: format!("exit-{port}"),
            host: host.to_string(),
            port,
            proto: form.proto,
            credential_ref: credential_ref.clone(),
        })
        .collect();
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
        .map(|e| match e.path.as_str() {
            "agent[0].egress.expected.ip" => err("expectedIp", "需要是一个完整的 IP 地址"),
            _ => e,
        })
        .collect();
    if !problems.is_empty() {
        return Err(problems);
    }
    *set = next;
    Ok((change, form.takeover_enabled))
}

/// What the page server needs from its owner.
pub trait SetupBackend: Send + Sync {
    fn view(&self) -> SetupView;
    /// Validate and store. Errors are per field.
    fn save(&self, form: ExitForm) -> Result<(), Vec<FieldError>>;
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
    PAGE.replace("__TOKEN__", token)
        .replace(
            "__VIEW__",
            &js(serde_json::to_value(view).expect("serializes")),
        )
        .replace("__TZ__", &js(serde_json::json!(tz)))
        .replace("__LANG__", &js(serde_json::json!(LANGUAGES)))
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
        ("GET", "/setup") if token_ok => respond(
            &mut s,
            "200 OK",
            "text/html; charset=utf-8",
            &page(token, &backend.view()),
        ),
        ("POST", "/save") if token_ok => {
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
            let result = match serde_json::from_slice::<ExitForm>(body) {
                Ok(form) => backend.save(form),
                Err(e) => Err(vec![err("form", &format!("表单格式不对：{e}"))]),
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
        (_, "/setup") | (_, "/save") => {
            respond(&mut s, "403 Forbidden", "text/plain", "token rejected")
        }
        _ => respond(&mut s, "404 Not Found", "text/plain", "not found"),
    }
}

impl SetupServer {
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
<title>出口与时区设置</title>
<style>
:root{--bg:#f6f7f5;--card:#fff;--fg:#1d2420;--muted:#5d6862;--line:#d8ddd9;--accent:#28664b;--err:#b3261e;--okbg:#e5f2ea;--errbg:#fbeaea}
@media (prefers-color-scheme:dark){:root{--bg:#141816;--card:#1d2320;--fg:#e6ebe8;--muted:#9aa5a0;--line:#333b37;--accent:#5fb98d;--err:#f2867e;--okbg:#1f3329;--errbg:#3a2120}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 system-ui,"Microsoft YaHei UI",sans-serif}
main{max-width:640px;margin:0 auto;padding:24px 16px 48px}
h1{font-size:22px;margin:0 0 4px}p.lead{color:var(--muted);margin:0 0 20px}
section{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:16px 18px;margin-bottom:16px}
h2{font-size:16px;margin:0 0 12px}
label{display:block;font-weight:600;margin:12px 0 4px}label:first-of-type{margin-top:0}
.hint{color:var(--muted);font-size:13px;font-weight:400;margin-top:3px}
input[type=text],input[type=password],select{width:100%;padding:8px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--fg);font:inherit}
.row{display:flex;gap:12px}.row>div{flex:1;min-width:0}.row>.narrow{flex:0 0 140px}
.check{display:flex;gap:10px;align-items:flex-start;font-weight:400}.check input{margin-top:4px}
.fe{color:var(--err);font-size:13px;margin-top:3px}
button{background:var(--accent);color:#fff;border:0;border-radius:6px;padding:10px 22px;font:inherit;font-weight:600;cursor:pointer}
button:disabled{opacity:.5;cursor:default}
#msg{margin-top:14px;padding:10px 12px;border-radius:6px;display:none}
#msg.ok{display:block;background:var(--okbg)}#msg.bad{display:block;background:var(--errbg);color:var(--err)}
@media (max-width:480px){.row{flex-direction:column;gap:0}.row>.narrow{flex:none}}
</style></head><body><main>
<h1>出口与时区设置</h1>
<p class="lead">填你自己的出口代理。只有从托盘打开的浏览器和 CLI 会走这个出口，其他软件不受影响。</p>
<form id="f" autocomplete="off">
<section><h2>出口代理</h2>
<div class="row"><div><label for="host">地址</label><input id="host" type="text" placeholder="例如 203.0.113.10 或 proxy.example.com"><div class="fe" data-for="host"></div></div>
<div class="narrow"><label for="proto">协议</label><select id="proto"><option value="socks5">SOCKS5</option><option value="http">HTTP</option></select></div></div>
<label for="ports">端口</label><input id="ports" type="text" placeholder="例如 12324，多个端口用逗号分隔">
<div class="hint">多个端口必须是同一个出口（同一台机器）；连不上一个会自动换下一个。</div><div class="fe" data-for="ports"></div>
<div class="row"><div><label for="username">用户名</label><input id="username" type="text" placeholder="没有就留空"><div class="fe" data-for="username"></div></div>
<div><label for="password">密码</label><input id="password" type="password" placeholder=""><div class="fe" data-for="password"></div></div></div>
<div class="hint">密码存进 Windows 凭据管理器，不写进任何配置文件，这个页面也不会显示已保存的密码。</div>
<label for="expectedIp">出口 IP（可选）</label><input id="expectedIp" type="text" placeholder="填了就严格比对，不一致算检测不通过">
<div class="fe" data-for="expectedIp"></div>
</section>
<section><h2>时区与语言</h2>
<div class="row"><div><label for="timezone">目标时区</label><select id="timezone"></select><div class="fe" data-for="timezone"></div></div>
<div><label for="language">浏览器语言</label><select id="language"></select><div class="fe" data-for="language"></div></div></div>
<label class="check"><input id="takeoverEnabled" type="checkbox"><span>进入 Agent 模式时把系统时区改成目标时区，退出时改回原样
<div class="hint">不勾选就不改系统设置，但时区检测（G4）会一直显示「未知」，托盘保持黄色。</div></span></label>
</section>
<button id="save" type="submit">保存</button>
<div id="msg"></div>
<div class="fe" data-for="form"></div><div id="other" class="fe"></div>
</form></main>
<script>
const T="__TOKEN__",V=__VIEW__,TZ=__TZ__,LANG=__LANG__;
const $=id=>document.getElementById(id);
for(const [k,l] of TZ){const o=document.createElement("option");o.value=k;o.textContent=l;$("timezone").append(o)}
for(const [k,l] of LANG){const o=document.createElement("option");o.value=k;o.textContent=l+"  "+k;$("language").append(o)}
for(const k of ["host","ports","username","expectedIp"])$(k).value=V[k]||"";
$("proto").value=V.proto;$("timezone").value=V.timezone;$("language").value=V.language;$("takeoverEnabled").checked=V.takeoverEnabled;
$("password").placeholder=V.passwordStored?"已保存，不改就留空":"没有就留空";
if(V.locked){$("save").disabled=true;show("bad","现在处于 Agent 模式。先从托盘「退出 Agent 模式」，再修改设置。")}
function show(c,t){const m=$("msg");m.className=c;m.textContent=t}
$("f").addEventListener("submit",async e=>{e.preventDefault();
 document.querySelectorAll(".fe").forEach(x=>x.textContent="");$("msg").className="";
 const form={host:$("host").value,proto:$("proto").value,ports:$("ports").value,username:$("username").value,
  password:$("password").value||null,timezone:$("timezone").value,language:$("language").value,
  expectedIp:$("expectedIp").value,takeoverEnabled:$("takeoverEnabled").checked};
 $("save").disabled=true;
 try{const r=await fetch("/save?t="+T,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(form)});
  const j=await r.json();
  if(j.ok){$("password").value="";$("password").placeholder=form.username?"已保存，不改就留空":"没有就留空";
   show("ok","已保存。回到托盘点「进入 Agent 模式」。这个页面可以关掉了。")}
  else{let other=[];for(const x of j.errors){const el=document.querySelector('.fe[data-for="'+x.path+'"]');
    if(el)el.textContent=x.message;else other.push(x.path+"："+x.message)}
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
            host: " 203.0.113.10 ".into(),
            proto: UpstreamProto::Socks5,
            ports: "12324, 12323，12324".into(),
            username: "alice".into(),
            password: Some("s3cret".into()),
            timezone: "us-west".into(),
            language: "en-GB".into(),
            expected_ip: "".into(),
            takeover_enabled: true,
        }
    }

    #[test]
    fn a_form_becomes_one_candidate_per_port_with_one_credential() {
        let mut set = ProfileSet::default();
        let (change, takeover) = apply(&mut set, &form(), None).unwrap();
        assert!(takeover);
        assert_eq!(
            change,
            CredentialChange::Store {
                user: "alice".into(),
                pass: "s3cret".into()
            }
        );
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

        let v = view(&set, Some("alice"), true, false);
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
    fn credential_rules() {
        let mut set = ProfileSet::default();
        let mut f = form();
        f.password = None;
        assert_eq!(
            apply(&mut set.clone(), &f, Some("alice")).unwrap().0,
            CredentialChange::Keep
        );
        let e = apply(&mut set.clone(), &f, Some("bob")).unwrap_err();
        assert_eq!(e[0].path, "password", "a new user name needs a password");
        f.username = " ".into();
        let (c, _) = apply(&mut set, &f, Some("alice")).unwrap();
        assert_eq!(c, CredentialChange::Delete);
        assert!(set.agent[0].egress.candidates[0].credential_ref.is_none());
        f.password = Some("x".into());
        assert_eq!(apply(&mut set, &f, None).unwrap_err()[0].path, "username");
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
            (
                "expectedIp",
                Box::new(|f| f.expected_ip = "1.2.3.0/24".into()),
            ),
        ];
        for (path, mutate) in cases {
            let mut f = form();
            mutate(&mut f);
            let mut set = before.clone();
            let errors = apply(&mut set, &f, None).unwrap_err();
            assert!(errors.iter().any(|e| e.path == path), "{path}: {errors:?}");
            assert_eq!(set, before);
        }
    }

    #[test]
    fn credential_change_debug_hides_the_password() {
        let c = CredentialChange::Store {
            user: "u".into(),
            pass: "hunter2".into(),
        };
        assert!(!format!("{c:?}").contains("hunter2"));
    }

    struct Fake(std::sync::Mutex<Vec<ExitForm>>);
    impl SetupBackend for Fake {
        fn view(&self) -> SetupView {
            view(&ProfileSet::default(), None, false, false)
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
    fn server_requires_token_host_and_origin() {
        let backend = Arc::new(Fake(Default::default()));
        let srv = SetupServer::start(0, "tok".into(), backend.clone()).unwrap();
        let port = srv.port;
        let h = format!("Host: 127.0.0.1:{port}\r\n");
        assert!(
            send(port, &format!("GET /setup?t=tok HTTP/1.1\r\n{h}\r\n")).contains("出口与时区设置")
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
    }
}
