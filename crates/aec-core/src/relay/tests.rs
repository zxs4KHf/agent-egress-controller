//! Relay tests. Pure ordering and budget logic runs on a fake clock; protocol behaviour runs on
//! real loopback sockets against small fake upstreams.

use super::*;
use std::cell::Cell;
use std::sync::mpsc;

fn up(port: u16, proto: UpstreamProto) -> Upstream {
    Upstream {
        host: "127.0.0.1".into(),
        port,
        proto,
        user: None,
        pass: None,
    }
}

fn with_auth(mut u: Upstream) -> Upstream {
    u.user = Some("user1".into());
    u.pass = Some("s3cret-pass".into());
    u
}

// ---- S1, S4, S5: pool logic on a fake clock ---------------------------------------------------

#[test]
fn s1_pool_must_be_one_exit_host() {
    assert!(validate_pool(&[]).is_err());
    let mut other = up(2, UpstreamProto::Http);
    other.host = "192.0.2.1".into();
    let err = validate_pool(&[up(1, UpstreamProto::Socks5), other]).unwrap_err();
    assert!(err.contains("2 different exit hosts"), "{err}");
    assert!(validate_pool(&[up(1, UpstreamProto::Socks5), up(2, UpstreamProto::Http)]).is_ok());
}

#[test]
fn s5_cooling_is_ordering_not_exclusion() {
    let pool = Pool::new(vec![
        up(1, UpstreamProto::Socks5),
        up(2, UpstreamProto::Socks5),
        up(3, UpstreamProto::Socks5),
    ])
    .unwrap();
    let t = Instant::now();
    assert_eq!(pool.order(t), vec![0, 1, 2]);
    pool.note_fail(0, Duration::from_secs(3), t);
    assert_eq!(
        pool.order(t),
        vec![1, 2, 0],
        "the cooling candidate goes last"
    );
    pool.note_fail(1, Duration::from_secs(1), t);
    pool.note_fail(2, Duration::from_secs(2), t);
    assert_eq!(
        pool.order(t),
        vec![1, 2, 0],
        "all cooling: earliest expiry first, none excluded"
    );
    assert_eq!(
        pool.order(t + Duration::from_secs(5)),
        vec![0, 1, 2],
        "expired cooldowns no longer count"
    );
    pool.note_ok(0);
    assert_eq!(pool.counts(0), (1, 1));
}

/// Fake clock advanced by each simulated attempt.
struct Clock {
    base: Instant,
    offset: Cell<Duration>,
}
impl Clock {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            offset: Cell::new(Duration::ZERO),
        }
    }
    fn now(&self) -> Instant {
        self.base + self.offset.get()
    }
    fn advance(&self, d: Duration) {
        self.offset.set(self.offset.get() + d);
    }
}

#[test]
fn s4_fails_over_and_records_every_attempt() {
    let pool = Pool::new(vec![
        with_auth(up(1, UpstreamProto::Socks5)),
        up(2, UpstreamProto::Http),
    ])
    .unwrap();
    let clock = Clock::new();
    let out = dial_pool(
        &pool,
        Duration::from_secs(4),
        3,
        Duration::from_secs(3),
        &|| clock.now(),
        &mut |i, _| {
            clock.advance(Duration::from_millis(400));
            if i == 0 {
                Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
            } else {
                Ok(i)
            }
        },
    );
    assert_eq!((out.stream, out.winner), (Some(1), Some(1)));
    assert_eq!(out.tried.len(), 1);
    assert!(
        out.tried[0].starts_with("127.0.0.1:1(socks5)=timed out 0.40s"),
        "{:?}",
        out.tried
    );
    assert!(
        !out.tried[0].contains("s3cret") && !out.tried[0].contains("user1"),
        "S12: no credentials in logs"
    );
    assert_eq!(
        pool.order(clock.now()),
        vec![1, 0],
        "the failed one cools down"
    );
}

#[test]
fn s4_stops_at_the_budget_and_after_the_rounds() {
    let pool = Pool::new(vec![
        up(1, UpstreamProto::Socks5),
        up(2, UpstreamProto::Socks5),
    ])
    .unwrap();
    let clock = Clock::new();
    let attempts = Cell::new(0);
    let out: DialOutcome<()> = dial_pool(
        &pool,
        Duration::from_millis(1000),
        10,
        Duration::ZERO,
        &|| clock.now(),
        &mut |_, _| {
            attempts.set(attempts.get() + 1);
            clock.advance(Duration::from_millis(300));
            Err(io::Error::other("refused"))
        },
    );
    assert!(out.stream.is_none());
    assert_eq!(
        attempts.get(),
        3,
        "attempts at 0, 300 and 600 ms; at 900 ms less than 150 ms are left"
    );
    assert_eq!(out.tried.last().unwrap(), "dial budget exhausted");

    let attempts = Cell::new(0);
    let out: DialOutcome<()> = dial_pool(
        &pool,
        Duration::from_secs(60),
        3,
        Duration::ZERO,
        &|| clock.now(),
        &mut |_, _| {
            attempts.set(attempts.get() + 1);
            Err(io::Error::other("refused"))
        },
    );
    assert_eq!(attempts.get(), 6, "3 rounds over 2 candidates");
    assert_eq!(out.tried.last().unwrap(), "rounds exhausted (3)");
}

#[test]
fn s3_gate_limits_concurrent_dials() {
    let g = Gate {
        used: Mutex::new(0),
        cv: Condvar::new(),
        max: 1,
    };
    assert!(g.acquire(Duration::from_millis(10)));
    let t = Instant::now();
    assert!(
        !g.acquire(Duration::from_millis(100)),
        "a second dial waits, then gives up"
    );
    assert!(t.elapsed() >= Duration::from_millis(90));
    g.release();
    assert!(g.acquire(Duration::from_millis(10)));
    assert_eq!(g.in_use(), 1);
}

#[test]
fn base64_matches_rfc4648() {
    for (i, o) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("user1:s3cret-pass", "dXNlcjE6czNjcmV0LXBhc3M="),
    ] {
        assert_eq!(base64(i.as_bytes()), o);
    }
}

// ---- real sockets ------------------------------------------------------------------------------

/// Echo server standing in for the final target.
fn echo_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut r = s.try_clone().unwrap();
                let mut w = s;
                let _ = io::copy(&mut r, &mut w);
            });
        }
    });
    port
}

/// A closed port: bind, read the port, drop.
fn dead_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Fake SOCKS5 upstream. `require_auth`: answer 0xFF to an offer without 0x02 and accept only
/// user1/s3cret-pass. Reports each greeting's offered methods on `offers`.
fn socks5_upstream(require_auth: bool, offers: mpsc::Sender<Vec<u8>>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let offers = offers.clone();
            std::thread::spawn(move || {
                let mut s = s;
                let mut h = [0u8; 2];
                if s.read_exact(&mut h).is_err() {
                    return;
                }
                let mut m = vec![0u8; h[1] as usize];
                s.read_exact(&mut m).unwrap();
                let _ = offers.send(m.clone());
                if require_auth {
                    if !m.contains(&2) {
                        let _ = s.write_all(&[5, 0xFF]);
                        return;
                    }
                    s.write_all(&[5, 2]).unwrap();
                    let mut v = [0u8; 2];
                    s.read_exact(&mut v).unwrap();
                    let mut u = vec![0u8; v[1] as usize];
                    s.read_exact(&mut u).unwrap();
                    let mut pl = [0u8; 1];
                    s.read_exact(&mut pl).unwrap();
                    let mut p = vec![0u8; pl[0] as usize];
                    s.read_exact(&mut p).unwrap();
                    let ok = u == b"user1" && p == b"s3cret-pass";
                    s.write_all(&[1, if ok { 0 } else { 1 }]).unwrap();
                    if !ok {
                        return;
                    }
                } else {
                    s.write_all(&[5, 0]).unwrap();
                }
                let mut r = [0u8; 4];
                s.read_exact(&mut r).unwrap();
                let mut n = [0u8; 1];
                s.read_exact(&mut n).unwrap();
                let mut name = vec![0u8; n[0] as usize];
                s.read_exact(&mut name).unwrap();
                let mut pb = [0u8; 2];
                s.read_exact(&mut pb).unwrap();
                let target = format!(
                    "{}:{}",
                    String::from_utf8_lossy(&name),
                    u16::from_be_bytes(pb)
                );
                match TcpStream::connect(target) {
                    Ok(t) => {
                        s.write_all(&REPLY_OK).unwrap();
                        pump(s, t);
                    }
                    Err(_) => {
                        let _ = s.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]);
                    }
                }
            });
        }
    });
    port
}

/// Fake HTTP CONNECT upstream that answers 407 until the right Basic credentials arrive.
fn http_upstream(heads: mpsc::Sender<String>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let heads = heads.clone();
            std::thread::spawn(move || {
                let mut s = s;
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if s.read_exact(&mut b).is_err() {
                        return;
                    }
                    head.push(b[0]);
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let _ = heads.send(text.clone());
                if !text.contains("Proxy-Authorization: Basic dXNlcjE6czNjcmV0LXBhc3M=") {
                    let _ = s.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n");
                    return;
                }
                let target = text.split_whitespace().nth(1).unwrap().to_string();
                let t = TcpStream::connect(target).unwrap();
                s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .unwrap();
                pump(s, t);
            });
        }
    });
    port
}

struct Logs(Arc<Mutex<Vec<String>>>);
impl Logs {
    fn new() -> (Self, LogSink) {
        let v = Arc::new(Mutex::new(Vec::new()));
        let v2 = v.clone();
        (
            Self(v),
            Arc::new(move |level: &str, msg: &str| {
                v2.lock().unwrap().push(format!("[{level}] {msg}"))
            }),
        )
    }
    fn text(&self) -> String {
        self.0.lock().unwrap().join("\n")
    }
}

fn cfg(candidates: Vec<Upstream>) -> RelayConfig {
    RelayConfig {
        listen: "127.0.0.1:0".into(),
        candidates,
        connect_timeout_ms: 800,
        dial_budget_ms: 2500,
        cooldown_ms: 3000,
        max_inflight: 3,
        max_rounds: 3,
        handshake_timeout_ms: 2000,
    }
}

/// Client side of SOCKS5 to the relay: no-auth, CONNECT by name. Returns the stream and reply code.
fn socks_connect(relay: SocketAddr, host: &str, port: u16) -> (TcpStream, u8) {
    let mut s = TcpStream::connect(relay).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(&[5, 1, 0]).unwrap();
    let mut r = [0u8; 2];
    s.read_exact(&mut r).unwrap();
    assert_eq!(r, [5, 0]);
    let mut req = vec![5, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).unwrap();
    (s, rep[1])
}

fn roundtrip(s: &mut TcpStream) -> Vec<u8> {
    s.write_all(b"ping through the relay").unwrap();
    let mut buf = vec![0u8; 22];
    s.read_exact(&mut buf).unwrap();
    buf
}

#[test]
fn s4_s10_fails_over_to_a_working_candidate_and_relays_bytes() {
    let echo = echo_server();
    let (tx, _rx) = mpsc::channel();
    let good = socks5_upstream(false, tx);
    let (logs, sink) = Logs::new();
    let relay = start(
        cfg(vec![
            up(dead_port(), UpstreamProto::Socks5),
            up(good, UpstreamProto::Socks5),
        ]),
        sink,
    )
    .unwrap();
    let (mut s, code) = socks_connect(relay.addr, "127.0.0.1", echo);
    assert_eq!(code, 0);
    assert_eq!(roundtrip(&mut s), b"ping through the relay");
    assert_eq!(relay.pool.counts(0).1, 1, "the dead candidate failed once");
    assert_eq!(relay.pool.counts(1).0, 1);
    let text = logs.text();
    assert!(
        text.contains("[conn] 127.0.0.1:") && text.contains("(1 failed attempt(s) before)"),
        "{text}"
    );
}

#[test]
fn s9_s11_s12_all_candidates_failing_is_reported_truthfully() {
    let (logs, sink) = Logs::new();
    let relay = start(
        cfg(vec![
            with_auth(up(dead_port(), UpstreamProto::Socks5)),
            with_auth(up(dead_port(), UpstreamProto::Http)),
        ]),
        sink,
    )
    .unwrap();
    let (_s, code) = socks_connect(relay.addr, "example.org", 443);
    assert_eq!(code, 1, "general failure, no made-up success");
    let text = logs.text();
    assert!(
        text.contains("[fail] example.org:443 every candidate failed ->"),
        "{text}"
    );
    assert!(
        text.matches("(socks5)=").count() >= 1 && text.matches("(http)=").count() >= 1,
        "{text}"
    );
    assert!(
        !text.contains("s3cret-pass") && !text.contains("user1"),
        "credentials leaked: {text}"
    );
}

#[test]
fn s7_http_upstream_is_tried_anonymously_first() {
    let echo = echo_server();
    let (tx, rx) = mpsc::channel();
    let http = http_upstream(tx);
    let (_logs, sink) = Logs::new();
    let relay = start(cfg(vec![with_auth(up(http, UpstreamProto::Http))]), sink).unwrap();
    let (mut s, code) = socks_connect(relay.addr, "127.0.0.1", echo);
    assert_eq!(code, 0);
    assert_eq!(roundtrip(&mut s), b"ping through the relay");
    let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let second = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        !first.contains("Proxy-Authorization"),
        "first attempt is anonymous: {first}"
    );
    assert!(
        second.contains("Proxy-Authorization: Basic "),
        "credentials only on the retry: {second}"
    );
}

#[test]
fn s8_socks5_upstream_offers_one_method_at_a_time() {
    let echo = echo_server();
    let (tx, rx) = mpsc::channel();
    let socks = socks5_upstream(true, tx);
    let (_logs, sink) = Logs::new();
    let relay = start(cfg(vec![with_auth(up(socks, UpstreamProto::Socks5))]), sink).unwrap();
    let (mut s, code) = socks_connect(relay.addr, "127.0.0.1", echo);
    assert_eq!(code, 0);
    assert_eq!(roundtrip(&mut s), b"ping through the relay");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        vec![0x00],
        "first only 'no authentication'"
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        vec![0x02],
        "then only username/password"
    );
}

#[test]
fn s2_inbound_method_selection_and_garbage() {
    let (logs, sink) = Logs::new();
    let relay = start(cfg(vec![up(dead_port(), UpstreamProto::Socks5)]), sink).unwrap();
    // Only GSSAPI offered: refused.
    let mut s = TcpStream::connect(relay.addr).unwrap();
    s.write_all(&[5, 1, 1]).unwrap();
    let mut r = [0u8; 2];
    s.read_exact(&mut r).unwrap();
    assert_eq!(r, [5, 0xFF]);
    // Username/password offered: accepted with any credentials.
    let mut s = TcpStream::connect(relay.addr).unwrap();
    s.write_all(&[5, 2, 0, 2]).unwrap();
    s.read_exact(&mut r).unwrap();
    assert_eq!(r, [5, 2]);
    s.write_all(&[1, 1, b'x', 1, b'y']).unwrap();
    s.read_exact(&mut r).unwrap();
    assert_eq!(r, [1, 0]);
    // Not SOCKS5 at all: dropped, logged with the bytes seen.
    let mut s = TcpStream::connect(relay.addr).unwrap();
    s.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
    let mut buf = [0u8; 1];
    let _ = s.read(&mut buf);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        logs.text()
            .contains("[hs] handshake incomplete: first byte is 71 not 5")
            && logs.text().contains("inbound so far=4745"),
        "{}",
        logs.text()
    );
}

#[test]
fn s6_a_silent_upstream_cannot_outlast_the_budget() {
    // Accepts TCP, never answers the SOCKS5 greeting.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let silent = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let held: Vec<TcpStream> = l.incoming().flatten().take(10).collect();
        std::thread::sleep(Duration::from_secs(10));
        drop(held);
    });
    let (_logs, sink) = Logs::new();
    let mut c = cfg(vec![up(silent, UpstreamProto::Socks5)]);
    c.connect_timeout_ms = 5000;
    c.dial_budget_ms = 600;
    let relay = start(c, sink).unwrap();
    let t = Instant::now();
    let (_s, code) = socks_connect(relay.addr, "example.org", 443);
    assert_eq!(code, 1);
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "took {:?}",
        t.elapsed()
    );
}

#[test]
fn s1_mixed_pool_refuses_to_start() {
    let mut other = up(1, UpstreamProto::Socks5);
    other.host = "192.0.2.7".into();
    let (_logs, sink) = Logs::new();
    let err = start(cfg(vec![up(1, UpstreamProto::Socks5), other]), sink)
        .err()
        .expect("must refuse");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}
