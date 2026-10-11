//! Local check page (SPEC 4.5): a loopback-only HTTP server the Agent browser opens as its first
//! tab. The page reads what the browser itself reports (language, Intl time zone, cookie switch,
//! canvas/WebGL, WebRTC candidates) and posts it back once; the server adds the Accept-Language
//! header the browser actually sent. Data never leaves the machine.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const MAX_BODY: usize = 64 * 1024;

/// What the page reports, plus the Accept-Language header seen by the server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSignals {
    pub language: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    pub timezone: Option<String>,
    pub cookie_enabled: Option<bool>,
    pub canvas_sha256: Option<String>,
    pub webgl_vendor: Option<String>,
    pub webgl_renderer: Option<String>,
    #[serde(default)]
    pub candidates: Vec<String>,
    pub user_agent: Option<String>,
    /// Filled in by the server, never by the page.
    #[serde(default)]
    pub accept_language_header: Option<String>,
}

#[derive(Default)]
struct Shared {
    accept_language: Option<String>,
    report: Option<BrowserSignals>,
    rejected: Vec<String>,
}

pub struct CheckPageServer {
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
}

fn page(token: &str, stun: Option<&str>) -> String {
    let stun = stun
        .map(|s| format!("\"{s}\""))
        .unwrap_or_else(|| "null".into());
    PAGE.replace("__TOKEN__", token).replace("__STUN__", &stun)
}

fn respond(s: &mut TcpStream, status: &str, ctype: &str, body: &str) {
    let _ = write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

fn handle(mut s: TcpStream, port: u16, token: &str, stun: Option<&str>, shared: &Mutex<Shared>) {
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
    let reject = |why: &str, s: &mut TcpStream, status: &str| {
        shared
            .lock()
            .expect("lock")
            .rejected
            .push(format!("{method} {path}: {why}"));
        respond(s, status, "text/plain", why);
    };

    match (method, path) {
        ("GET", "/check") if token_ok => {
            shared.lock().expect("lock").accept_language =
                header(&head, "accept-language").map(str::to_string);
            respond(
                &mut s,
                "200 OK",
                "text/html; charset=utf-8",
                &page(token, stun),
            );
        }
        ("POST", "/report") if token_ok => {
            let origin_ok =
                header(&head, "origin") == Some(format!("http://127.0.0.1:{port}").as_str());
            if !origin_ok {
                return reject("origin rejected", &mut s, "403 Forbidden");
            }
            let len: usize = header(&head, "content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if len > MAX_BODY {
                return reject("body too large", &mut s, "413 Payload Too Large");
            }
            while buf.len() < head_end + len {
                match s.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let body = &buf[head_end..buf.len().min(head_end + len)];
            let mut guard = shared.lock().expect("lock");
            if guard.report.is_some() {
                drop(guard);
                return reject("token already used", &mut s, "409 Conflict");
            }
            match serde_json::from_slice::<BrowserSignals>(body) {
                Ok(mut r) => {
                    r.accept_language_header = guard.accept_language.clone();
                    guard.report = Some(r);
                    drop(guard);
                    respond(&mut s, "204 No Content", "text/plain", "");
                }
                Err(e) => {
                    drop(guard);
                    reject(&format!("bad report: {e}"), &mut s, "400 Bad Request");
                }
            }
        }
        (_, "/check") | (_, "/report") => reject("token rejected", &mut s, "403 Forbidden"),
        _ => respond(&mut s, "404 Not Found", "text/plain", "not found"),
    }
}

impl CheckPageServer {
    /// Listen on 127.0.0.1:`port` (0 picks a free port). `stun` is used by the page to learn the
    /// public address WebRTC would reveal; `None` in local-only mode.
    pub fn start(port: u16, token: String, stun: Option<String>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (stop2, shared2, token2) = (stop.clone(), shared.clone(), token.clone());
        let thread = std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, _)) => {
                        let _ = s.set_nonblocking(false);
                        handle(s, port, &token2, stun.as_deref(), &shared2);
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        });
        Ok(Self {
            port,
            token,
            stop,
            shared,
            thread: Some(thread),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/check?t={}", self.port, self.token)
    }

    /// Wait for the page's report.
    pub fn wait(&self, timeout: Duration) -> Option<BrowserSignals> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(r) = self.shared.lock().expect("lock").report.clone() {
                return Some(r);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    /// Requests the server refused, for the evidence trail.
    pub fn rejected(&self) -> Vec<String> {
        self.shared.lock().expect("lock").rejected.clone()
    }
}

impl Drop for CheckPageServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = TcpStream::connect_timeout(
                &SocketAddr::from(([127, 0, 0, 1], self.port)),
                Duration::from_millis(200),
            );
            let _ = t.join();
        }
    }
}

/// The page. Plain script, no external resources; the only network use besides posting back is
/// the optional STUN server for WebRTC.
const PAGE: &str = r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>Agent 环境检查</title>
<style>body{font:15px/1.5 system-ui,sans-serif;margin:40px;color:#222}code{background:#f3f3f3;padding:1px 4px}</style></head>
<body><h1>Agent 环境检查</h1><p id="s">正在读取本浏览器的语言、时区和只读信号……</p>
<p>结果只发回本机的 Agent Egress Controller，不会发送到其他地方。</p>
<script>
(async () => {
  const STUN = __STUN__;
  const out = {
    language: navigator.language,
    languages: Array.from(navigator.languages || []),
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    cookieEnabled: navigator.cookieEnabled,
    userAgent: navigator.userAgent,
    candidates: []
  };
  try {
    const c = document.createElement('canvas'); c.width = 240; c.height = 60;
    const g = c.getContext('2d'); g.textBaseline = 'top'; g.font = '16px Arial';
    g.fillStyle = '#f60'; g.fillRect(10, 10, 100, 30); g.fillStyle = '#069'; g.fillText('AEC canvas probe', 12, 15);
    const h = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(c.toDataURL()));
    out.canvasSha256 = Array.from(new Uint8Array(h)).map(b => b.toString(16).padStart(2, '0')).join('');
  } catch (e) {}
  try {
    const gl = document.createElement('canvas').getContext('webgl');
    const x = gl.getExtension('WEBGL_debug_renderer_info');
    out.webglVendor = String(x ? gl.getParameter(x.UNMASKED_VENDOR_WEBGL) : gl.getParameter(gl.VENDOR));
    out.webglRenderer = String(x ? gl.getParameter(x.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER));
  } catch (e) {}
  out.candidates = await new Promise(resolve => {
    const found = []; let pc;
    try { pc = new RTCPeerConnection({ iceServers: STUN ? [{ urls: STUN }] : [] }); } catch (e) { return resolve(found); }
    const done = () => { try { pc.close(); } catch (e) {} resolve(found); };
    pc.onicecandidate = e => { if (e.candidate && e.candidate.candidate) found.push(e.candidate.candidate); else if (!e.candidate) done(); };
    pc.createDataChannel('aec');
    pc.createOffer().then(o => pc.setLocalDescription(o)).catch(done);
    setTimeout(done, 5000);
  });
  const r = await fetch('/report?t=__TOKEN__', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(out) });
  document.getElementById('s').textContent = r.ok ? '检查完成，可以关闭此页。' : '回传失败：' + r.status;
})();
</script></body></html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn send(port: u16, raw: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    fn post(port: u16, token: &str, origin: &str, body: &str) -> String {
        send(
            port,
            &format!("POST /report?t={token} HTTP/1.1\r\nHost: x\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()),
        )
    }

    #[test]
    fn serves_page_and_accepts_one_report_with_header() {
        let srv = CheckPageServer::start(0, "tok123".into(), Some("stun:stun.example:3478".into()))
            .unwrap();
        let p = srv.port();
        let page = send(
            p,
            "GET /check?t=tok123 HTTP/1.1\r\nHost: x\r\nAccept-Language: en-US,en;q=0.9\r\n\r\n",
        );
        assert!(
            page.starts_with("HTTP/1.1 200")
                && page.contains("stun:stun.example:3478")
                && page.contains("/report?t=tok123")
        );
        let origin = format!("http://127.0.0.1:{p}");
        let r = post(
            p,
            "tok123",
            &origin,
            r#"{"language":"en-US","languages":["en-US","en"],"timezone":"America/New_York","cookieEnabled":true,"candidates":["candidate:1 1 udp 1 203.0.113.9 5 typ srflx"]}"#,
        );
        assert!(r.starts_with("HTTP/1.1 204"), "{r}");
        let got = srv.wait(Duration::from_secs(1)).unwrap();
        assert_eq!(
            got.accept_language_header.as_deref(),
            Some("en-US,en;q=0.9")
        );
        assert_eq!(got.timezone.as_deref(), Some("America/New_York"));
        assert!(
            post(p, "tok123", &origin, "{}").starts_with("HTTP/1.1 409"),
            "single use"
        );
    }

    #[test]
    fn rejects_wrong_token_and_foreign_origin() {
        let srv = CheckPageServer::start(0, "good".into(), None).unwrap();
        let p = srv.port();
        assert!(send(p, "GET /check?t=bad HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 403"));
        assert!(send(p, "GET /check HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 403"));
        assert!(post(p, "good", "https://evil.example", "{}").starts_with("HTTP/1.1 403"));
        assert!(send(p, "GET /other HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 404"));
        assert!(srv.wait(Duration::from_millis(200)).is_none());
        assert_eq!(srv.rejected().len(), 3);
        assert!(send(p, "GET /check?t=good HTTP/1.1\r\n\r\n").contains("const STUN = null"));
    }

    #[test]
    fn listens_on_loopback_only_and_stops_on_drop() {
        let srv = CheckPageServer::start(0, "t".into(), None).unwrap();
        let p = srv.port();
        assert!(srv
            .url()
            .starts_with(&format!("http://127.0.0.1:{p}/check?t=")));
        drop(srv);
        assert!(TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], p)),
            Duration::from_millis(300)
        )
        .is_err());
    }
}
