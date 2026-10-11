//! Egress relay (SPEC 4.13, ticket 11): a local SOCKS5 front for a pool of upstream candidates that
//! all leave from the same exit address. Rewrite of an earlier Python relay; the semantics it
//! keeps are numbered S1–S12 in `docs/SPEC.md` (4.13) and referenced
//! below. Plain threads, like the original, so behaviour under load stays comparable.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::profile::UpstreamProto;

/// Below this much budget no new attempt starts (S4).
const MIN_ATTEMPT_BUDGET: Duration = Duration::from_millis(150);
const THROTTLE_WINDOW: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upstream {
    pub host: String,
    pub port: u16,
    pub proto: UpstreamProto,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub pass: Option<String>,
}

impl Upstream {
    /// What logs may show: never the credentials (S12).
    pub fn label(&self) -> String {
        let p = match self.proto {
            UpstreamProto::Socks5 => "socks5",
            UpstreamProto::Http => "http",
        };
        format!("{}:{}({p})", self.host, self.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayConfig {
    pub listen: String,
    pub candidates: Vec<Upstream>,
    pub connect_timeout_ms: u64,
    pub dial_budget_ms: u64,
    pub cooldown_ms: u64,
    pub max_inflight: usize,
    pub max_rounds: usize,
    #[serde(default = "default_handshake_ms")]
    pub handshake_timeout_ms: u64,
}

fn default_handshake_ms() -> u64 {
    10_000
}

/// S1: a non-empty pool whose candidates all name the same exit host. A changing exit address mid
/// session is the signal the whole design avoids, so a mixed pool refuses to start.
pub fn validate_pool(c: &[Upstream]) -> Result<(), String> {
    let first = c.first().ok_or("the candidate pool is empty")?;
    let mut hosts: Vec<&str> = c.iter().map(|u| u.host.as_str()).collect();
    hosts.sort_unstable();
    hosts.dedup();
    if hosts.len() > 1 {
        return Err(format!(
            "the pool names {} different exit hosts ({}); refusing to start",
            hosts.len(),
            hosts.join(", ")
        ));
    }
    let _ = first;
    Ok(())
}

// ---- pool state and ordering (S4, S5) ---------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct CandState {
    cooldown_until: Option<Instant>,
    ok: u64,
    fail: u64,
}

pub struct Pool {
    pub upstreams: Vec<Upstream>,
    state: Mutex<Vec<CandState>>,
}

impl Pool {
    pub fn new(upstreams: Vec<Upstream>) -> Result<Self, String> {
        validate_pool(&upstreams)?;
        let n = upstreams.len();
        Ok(Self {
            upstreams,
            state: Mutex::new(vec![CandState::default(); n]),
        })
    }

    /// Candidates not cooling down first (pool order), then cooling ones by earliest expiry.
    /// Cooling is a preference, never an exclusion (S5).
    pub fn order(&self, now: Instant) -> Vec<usize> {
        let st = self.state.lock().expect("lock");
        let mut idx: Vec<usize> = (0..st.len()).collect();
        idx.sort_by_key(|&i| match st[i].cooldown_until {
            Some(t) if t > now => (1, Some(t)),
            _ => (0, None),
        });
        idx
    }

    pub fn note_ok(&self, i: usize) {
        let mut st = self.state.lock().expect("lock");
        st[i].ok += 1;
        st[i].cooldown_until = None;
    }

    pub fn note_fail(&self, i: usize, cooldown: Duration, now: Instant) {
        let mut st = self.state.lock().expect("lock");
        st[i].fail += 1;
        st[i].cooldown_until = Some(now + cooldown);
    }

    pub fn counts(&self, i: usize) -> (u64, u64) {
        let st = self.state.lock().expect("lock");
        (st[i].ok, st[i].fail)
    }
}

pub struct DialOutcome<S> {
    pub stream: Option<S>,
    pub winner: Option<usize>,
    pub elapsed: Duration,
    /// One line per failed attempt: label, error, time spent (S11).
    pub tried: Vec<String>,
}

/// S4: walk the pool in up to `max_rounds` rounds within `budget`. `dial(i, deadline)` performs one
/// attempt; `now` is injected so tests can drive time.
pub fn dial_pool<S>(
    pool: &Pool,
    budget: Duration,
    max_rounds: usize,
    cooldown: Duration,
    now: &dyn Fn() -> Instant,
    dial: &mut dyn FnMut(usize, Instant) -> io::Result<S>,
) -> DialOutcome<S> {
    let start = now();
    let deadline = start + budget;
    let mut tried = Vec::new();
    for _ in 0..max_rounds {
        for i in pool.order(now()) {
            if now() + MIN_ATTEMPT_BUDGET >= deadline {
                tried.push("dial budget exhausted".to_string());
                return DialOutcome {
                    stream: None,
                    winner: None,
                    elapsed: now() - start,
                    tried,
                };
            }
            let t0 = now();
            match dial(i, deadline) {
                Ok(s) => {
                    pool.note_ok(i);
                    return DialOutcome {
                        stream: Some(s),
                        winner: Some(i),
                        elapsed: now() - t0,
                        tried,
                    };
                }
                Err(e) => {
                    pool.note_fail(i, cooldown, now());
                    tried.push(format!(
                        "{}={} {:.2}s",
                        pool.upstreams[i].label(),
                        e,
                        (now() - t0).as_secs_f64()
                    ));
                }
            }
        }
    }
    tried.push(format!("rounds exhausted ({max_rounds})"));
    DialOutcome {
        stream: None,
        winner: None,
        elapsed: now() - start,
        tried,
    }
}

// ---- upstream connectors (S6, S7, S8) ---------------------------------------------------------

/// S6: never wait past the remaining budget.
fn step(timeout: Duration, deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "dial budget used up",
        ));
    }
    Ok(timeout.min(left))
}

fn read_exact_staged(
    s: &mut TcpStream,
    n: usize,
    timeout: Duration,
    stage: &str,
) -> io::Result<Vec<u8>> {
    s.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
    let mut buf = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match s.read(&mut buf[got..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("{stage}: peer closed after {got}/{n} bytes"),
                ))
            }
            Ok(k) => got += k,
            Err(e) => return Err(io::Error::new(e.kind(), format!("{stage}: {e}"))),
        }
    }
    Ok(buf)
}

fn connect_tcp(up: &Upstream, timeout: Duration, deadline: Instant) -> io::Result<TcpStream> {
    let addr: SocketAddr = (up.host.as_str(), up.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "upstream host did not resolve"))?;
    let s = TcpStream::connect_timeout(&addr, step(timeout, deadline)?)?;
    s.set_nodelay(true)?;
    Ok(s)
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (k, shift) in [18, 12, 6, 0].iter().enumerate() {
            if k <= chunk.len() {
                out.push(T[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// S7: anonymous CONNECT first (an IP allow-list may admit us); credentials only on a retry.
fn connect_http(
    up: &Upstream,
    host: &str,
    port: u16,
    timeout: Duration,
    deadline: Instant,
) -> io::Result<TcpStream> {
    let mut last = None;
    for with_auth in [false, true] {
        if with_auth && up.user.is_none() {
            break;
        }
        let attempt = (|| {
            let mut s = connect_tcp(up, timeout, deadline)?;
            let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
            if with_auth {
                let token = base64(
                    format!(
                        "{}:{}",
                        up.user.as_deref().unwrap_or(""),
                        up.pass.as_deref().unwrap_or("")
                    )
                    .as_bytes(),
                );
                req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
            }
            req.push_str("Proxy-Connection: keep-alive\r\n\r\n");
            s.write_all(req.as_bytes())?;
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.extend(read_exact_staged(
                    &mut s,
                    1,
                    step(timeout, deadline)?,
                    &format!("upstream {} HTTP response head", up.label()),
                )?);
                if head.len() > 8192 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "upstream response head too long",
                    ));
                }
            }
            let first = String::from_utf8_lossy(head.split(|&b| b == b'\r').next().unwrap_or(&[]))
                .into_owned();
            if !first.split_whitespace().nth(1).is_some_and(|c| c == "200") {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!(
                        "upstream refused CONNECT: {}",
                        first.chars().take(48).collect::<String>()
                    ),
                ));
            }
            Ok(s)
        })();
        match attempt {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("at least one attempt"))
}

/// S8: offer only "no authentication" first; offer only username/password on the retry. Offering
/// both at once made the real upstream pick the credential path and refuse it (legacy finding).
fn connect_socks5(
    up: &Upstream,
    host: &str,
    port: u16,
    timeout: Duration,
    deadline: Instant,
) -> io::Result<TcpStream> {
    let mut last = None;
    for (method, with_auth) in [(0x00u8, false), (0x02u8, true)] {
        if with_auth && up.user.is_none() {
            break;
        }
        let attempt = (|| {
            let mut s = connect_tcp(up, timeout, deadline)?;
            let label = up.label();
            s.write_all(&[5, 1, method])?;
            let rep = read_exact_staged(
                &mut s,
                2,
                step(timeout, deadline)?,
                &format!("upstream {label} method negotiation (offered {method:02x})"),
            )?;
            if rep[1] == 0xFF {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "upstream rejected the authentication method",
                ));
            }
            if rep[1] == 2 {
                let (u, p) = (
                    up.user.as_deref().unwrap_or("").as_bytes(),
                    up.pass.as_deref().unwrap_or("").as_bytes(),
                );
                let mut msg = vec![1, u.len() as u8];
                msg.extend_from_slice(u);
                msg.push(p.len() as u8);
                msg.extend_from_slice(p);
                s.write_all(&msg)?;
                let auth = read_exact_staged(
                    &mut s,
                    2,
                    step(timeout, deadline)?,
                    &format!("upstream {label} authentication"),
                )?;
                if auth[1] != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "upstream rejected the SOCKS5 credentials",
                    ));
                }
            }
            let hb = host.as_bytes();
            let mut req = vec![5, 1, 0, 3, hb.len() as u8];
            req.extend_from_slice(hb);
            req.extend_from_slice(&port.to_be_bytes());
            s.write_all(&req)?;
            let rep = read_exact_staged(
                &mut s,
                4,
                step(timeout, deadline)?,
                &format!("upstream {label} CONNECT reply"),
            )?;
            if rep[1] != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("upstream SOCKS5 CONNECT failed with code {}", rep[1]),
                ));
            }
            let rest = match rep[3] {
                1 => 6,
                4 => 18,
                3 => {
                    read_exact_staged(
                        &mut s,
                        1,
                        step(timeout, deadline)?,
                        "upstream reply name length",
                    )?[0] as usize
                        + 2
                }
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("upstream reply ATYP {other}"),
                    ))
                }
            };
            read_exact_staged(
                &mut s,
                rest,
                step(timeout, deadline)?,
                &format!("upstream {label} CONNECT reply address"),
            )?;
            Ok(s)
        })();
        match attempt {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("at least one attempt"))
}

pub fn dial_upstream(
    up: &Upstream,
    host: &str,
    port: u16,
    timeout: Duration,
    deadline: Instant,
) -> io::Result<TcpStream> {
    match up.proto {
        UpstreamProto::Http => connect_http(up, host, port, timeout, deadline),
        UpstreamProto::Socks5 => connect_socks5(up, host, port, timeout, deadline),
    }
}

// ---- inbound SOCKS5 (S2, S9, S10) -------------------------------------------------------------

const REPLY_OK: [u8; 10] = [5, 0, 0, 1, 0, 0, 0, 0, 0, 0];
const REPLY_FAIL: [u8; 10] = [5, 1, 0, 1, 0, 0, 0, 0, 0, 0];

struct Inbound<'a> {
    s: &'a mut TcpStream,
    seen: Vec<u8>,
    timeout: Duration,
}

impl Inbound<'_> {
    fn read(&mut self, n: usize, stage: &str) -> io::Result<Vec<u8>> {
        let b = read_exact_staged(self.s, n, self.timeout, &format!("inbound {stage}"))?;
        self.seen.extend_from_slice(&b);
        Ok(b)
    }
}

/// S2: returns the requested target, or `None` when the client was answered with a refusal.
fn inbound_handshake(ib: &mut Inbound) -> io::Result<Option<(String, u16)>> {
    let head = ib.read(2, "greeting")?;
    if head[0] != 5 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("first byte is {} not 5: not SOCKS5", head[0]),
        ));
    }
    let methods = if head[1] > 0 {
        ib.read(head[1] as usize, "method list")?
    } else {
        Vec::new()
    };
    if methods.contains(&2) {
        ib.s.write_all(&[5, 2])?;
        ib.read(1, "auth version")?;
        let ulen = ib.read(1, "user length")?[0] as usize;
        ib.read(ulen, "user")?;
        let plen = ib.read(1, "password length")?[0] as usize;
        ib.read(plen, "password")?;
        ib.s.write_all(&[1, 0])?;
    } else if methods.contains(&0) {
        ib.s.write_all(&[5, 0])?;
    } else {
        ib.s.write_all(&[5, 0xFF])?;
        return Ok(None);
    }
    let req = ib.read(4, "CONNECT request")?;
    if req[0] != 5 || req[1] != 1 {
        ib.s.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])?;
        return Ok(None);
    }
    let host = match req[3] {
        1 => {
            let b = ib.read(4, "IPv4 address")?;
            IpAddr::from([b[0], b[1], b[2], b[3]]).to_string()
        }
        3 => {
            let n = ib.read(1, "name length")?[0] as usize;
            String::from_utf8_lossy(&ib.read(n, "name")?).into_owned()
        }
        4 => {
            let b: [u8; 16] = ib.read(16, "IPv6 address")?.try_into().expect("16 bytes");
            IpAddr::from(b).to_string()
        }
        _ => {
            ib.s.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0])?;
            return Ok(None);
        }
    };
    let p = ib.read(2, "port")?;
    Ok(Some((host, u16::from_be_bytes([p[0], p[1]]))))
}

/// S10: copy both ways; when either direction ends, shut both sockets so the other copy returns.
fn pump(a: TcpStream, b: TcpStream) {
    let (a2, b2) = match (a.try_clone(), b.try_clone()) {
        (Ok(x), Ok(y)) => (x, y),
        _ => return,
    };
    let t = std::thread::spawn(move || {
        let (mut r, mut w) = (a2, b2);
        let _ = io::copy(&mut r, &mut w);
        let _ = r.shutdown(Shutdown::Both);
        let _ = w.shutdown(Shutdown::Both);
    });
    let (mut r, mut w) = (b, a);
    let _ = io::copy(&mut r, &mut w);
    let _ = r.shutdown(Shutdown::Both);
    let _ = w.shutdown(Shutdown::Both);
    let _ = t.join();
}

// ---- concurrency gate (S3) and logging (S11, S12) ---------------------------------------------

struct Gate {
    used: Mutex<usize>,
    cv: Condvar,
    max: usize,
}

impl Gate {
    fn acquire(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut used = self.used.lock().expect("lock");
        while *used >= self.max {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            used = self.cv.wait_timeout(used, left).expect("lock").0;
        }
        *used += 1;
        true
    }
    fn release(&self) {
        *self.used.lock().expect("lock") -= 1;
        self.cv.notify_one();
    }
    #[cfg(test)]
    fn in_use(&self) -> usize {
        *self.used.lock().expect("lock")
    }
}

pub type LogSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Collapse repeats of the same message within a window into one line with a count (S11). The
/// first occurrence is always written.
struct Throttle {
    seen: Mutex<std::collections::HashMap<String, (Instant, usize)>>,
}

impl Throttle {
    fn emit(&self, sink: &LogSink, key: &str, level: &str, msg: &str) {
        let now = Instant::now();
        let mut seen = self.seen.lock().expect("lock");
        let suppressed = match seen.get_mut(key) {
            Some((last, n)) if now.duration_since(*last) < THROTTLE_WINDOW => {
                *n += 1;
                return;
            }
            Some((_, n)) => *n,
            None => 0,
        };
        seen.insert(key.to_string(), (now, 0));
        drop(seen);
        if suppressed > 0 {
            sink(
                level,
                &format!("{msg} ({suppressed} similar message(s) in the last window not written)"),
            );
        } else {
            sink(level, msg);
        }
    }
}

// ---- server -----------------------------------------------------------------------------------

pub struct RelayHandle {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub pool: Arc<Pool>,
}

impl RelayHandle {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for RelayHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn handle_client(
    mut client: TcpStream,
    pool: Arc<Pool>,
    cfg: Arc<RelayConfig>,
    gate: Arc<Gate>,
    sink: LogSink,
    throttle: Arc<Throttle>,
) {
    let timeout = Duration::from_millis(cfg.handshake_timeout_ms);
    let budget = Duration::from_millis(cfg.dial_budget_ms);
    let target = {
        let mut ib = Inbound {
            s: &mut client,
            seen: Vec::new(),
            timeout,
        };
        match inbound_handshake(&mut ib) {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(e) => {
                let hex: String = ib.seen.iter().map(|b| format!("{b:02x}")).collect();
                sink(
                    "hs",
                    &format!(
                        "handshake incomplete: {e} | inbound so far={}",
                        if hex.is_empty() {
                            "(empty)".into()
                        } else {
                            hex
                        }
                    ),
                );
                return;
            }
        }
    };
    let (host, port) = target;
    if !gate.acquire(budget.max(Duration::from_millis(50))) {
        sink(
            "busy",
            &format!(
                "{host}:{port} all {} upstream slots busy; answering failure",
                cfg.max_inflight
            ),
        );
        let _ = client.write_all(&REPLY_FAIL);
        return;
    }
    let connect_timeout = Duration::from_millis(cfg.connect_timeout_ms);
    let outcome = dial_pool(
        &pool,
        budget,
        cfg.max_rounds,
        Duration::from_millis(cfg.cooldown_ms),
        &Instant::now,
        &mut |i, deadline| {
            dial_upstream(&pool.upstreams[i], &host, port, connect_timeout, deadline)
        },
    );
    gate.release();
    match (outcome.stream, outcome.winner) {
        (Some(up), Some(i)) => {
            if client.write_all(&REPLY_OK).is_err() {
                return;
            }
            throttle.emit(
                &sink,
                &format!("conn {host}"),
                "conn",
                &format!(
                    "{host}:{port} -> {} in {:.2}s ({} failed attempt(s) before)",
                    pool.upstreams[i].label(),
                    outcome.elapsed.as_secs_f64(),
                    outcome.tried.len()
                ),
            );
            pump(client, up);
        }
        _ => {
            throttle.emit(
                &sink,
                &format!("allfail {host}:{port}"),
                "fail",
                &format!(
                    "{host}:{port} every candidate failed -> {}",
                    outcome.tried.join(" | ")
                ),
            );
            let _ = client.write_all(&REPLY_FAIL);
        }
    }
}

/// Start listening. Fails before binding when the pool is invalid (S1).
pub fn start(cfg: RelayConfig, sink: LogSink) -> io::Result<RelayHandle> {
    let pool = Arc::new(
        Pool::new(cfg.candidates.clone())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
    );
    let listener = TcpListener::bind(cfg.listen.as_str())?;
    let addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Gate {
        used: Mutex::new(0),
        cv: Condvar::new(),
        max: cfg.max_inflight.max(1),
    });
    let throttle = Arc::new(Throttle {
        seen: Mutex::new(Default::default()),
    });
    if cfg.dial_budget_ms >= 5000 {
        sink("warn", &format!("dial budget {} ms is not below the core's 5 s dial timeout; the core will give up first", cfg.dial_budget_ms));
    }
    sink(
        "up",
        &format!(
            "relay {addr} candidates {} connect-timeout={}ms dial-budget={}ms cooldown={}ms max-inflight={} max-rounds={}",
            pool.upstreams.iter().map(Upstream::label).collect::<Vec<_>>().join(", "),
            cfg.connect_timeout_ms,
            cfg.dial_budget_ms,
            cfg.cooldown_ms,
            cfg.max_inflight,
            cfg.max_rounds
        ),
    );
    let cfg = Arc::new(cfg);
    let (stop2, pool2) = (stop.clone(), pool.clone());
    let thread = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((s, _)) => {
                    let _ = s.set_nonblocking(false);
                    let (p, c, g, k, t) = (
                        pool2.clone(),
                        cfg.clone(),
                        gate.clone(),
                        sink.clone(),
                        throttle.clone(),
                    );
                    std::thread::spawn(move || handle_client(s, p, c, g, k, t));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    });
    Ok(RelayHandle {
        addr,
        stop,
        thread: Some(thread),
        pool,
    })
}

#[cfg(test)]
mod tests;
