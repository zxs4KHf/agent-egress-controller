//! End to end on loopback: client -> aec-mihomo (mixed port) -> aec-relay -> candidate 1 (dead)
//! -> candidate 2 (fake SOCKS5 upstream) -> local HTTP server. No real exit, no credentials.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const PORTS: &str = "29893,29909,29553,29325";
const HOST: &str = "relay-chain.test";

fn http_target() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut s = s;
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nrelay-chain-ok",
            );
        }
    });
    port
}

/// Minimal no-auth SOCKS5 upstream. Whatever name it is asked for, it connects to 127.0.0.1 on the
/// requested port, so the test can use a name the core cannot resolve or special-case.
fn socks5_upstream() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut s = s;
                let mut h = [0u8; 2];
                if s.read_exact(&mut h).is_err() {
                    return;
                }
                let mut m = vec![0u8; h[1] as usize];
                s.read_exact(&mut m).unwrap();
                s.write_all(&[5, 0]).unwrap();
                let mut r = [0u8; 5];
                s.read_exact(&mut r).unwrap();
                let mut name = vec![0u8; r[4] as usize];
                s.read_exact(&mut name).unwrap();
                let mut p = [0u8; 2];
                s.read_exact(&mut p).unwrap();
                let t = TcpStream::connect(("127.0.0.1", u16::from_be_bytes(p))).unwrap();
                s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
                let (mut a, mut b) = (s.try_clone().unwrap(), t.try_clone().unwrap());
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut a, &mut b);
                });
                let (mut c, mut d) = (t, s);
                let _ = std::io::copy(&mut c, &mut d);
            });
        }
    });
    port
}

fn aec_dev(data: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aec-dev"))
        .args(args)
        .env("AEC_DEV_DATA_DIR", data)
        .env("AEC_DEV_PORTS", PORTS)
        .output()
        .unwrap()
}

fn listening(port: u16) -> bool {
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(300)).is_ok()
}

#[test]
#[ignore = "needs runtime/bin/aec-mihomo.exe"]
fn request_flows_through_core_relay_and_fails_over() {
    let target = http_target();
    let good = socks5_upstream();
    let dead = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let data: PathBuf = std::env::temp_dir().join(format!("aec-chain-{}", std::process::id()));
    std::fs::create_dir_all(data.join("profiles")).unwrap();
    let profile = serde_json::json!({
        "schemaVersion": 1,
        "agent": [{
            "schemaVersion": 1, "id": "agent-default", "name": "Agent",
            "egress": { "candidates": [
                { "id": "dead", "host": "127.0.0.1", "port": dead, "proto": "socks5" },
                { "id": "good", "host": "127.0.0.1", "port": good, "proto": "socks5" }
            ]},
            "proxyScope": { "mode": "explicit_only", "bypass": ["<-loopback>", "127.0.0.1", "localhost"] },
            "routing": { "targetGroup": "agent-egress", "rulesTemplate": "default-v1" },
            "locale": { "timezone": { "windowsId": "Eastern Standard Time", "iana": "America/New_York" }, "browserLang": "en-US", "acceptLanguage": "en-US,en" },
            "targets": [{ "domain": "claude.ai" }],
            "entries": [],
            "detection": { "mode": "standard" }
        }],
        "daily": [{ "schemaVersion": 1, "id": "daily-default", "name": "daily", "restore": { "source": "snapshot", "closeLaunchedEntries": true } }]
    });
    std::fs::write(
        data.join("profiles").join("profiles.json"),
        serde_json::to_vec_pretty(&profile).unwrap(),
    )
    .unwrap();

    let up = aec_dev(&data, &["core", "up"]);
    let up_text =
        String::from_utf8_lossy(&up.stdout).into_owned() + &String::from_utf8_lossy(&up.stderr);

    let result = std::panic::catch_unwind(|| {
        assert!(up.status.success(), "core up failed: {up_text}");
        assert!(
            !data.join("core").join("relay.json").exists(),
            "the relay deletes its credential file after reading it"
        );
        // Not "localhost": mihomo resolves that through its built-in hosts and sends it DIRECT
        // (observed; good for the product, useless for this test). An unresolvable name has to
        // go to the relay by name.
        let mut s = TcpStream::connect(("127.0.0.1", 29893)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        write!(s, "GET http://{HOST}:{target}/ HTTP/1.1\r\nHost: {HOST}:{target}\r\nConnection: close\r\n\r\n").unwrap();
        let mut resp = String::new();
        let _ = s.read_to_string(&mut resp);
        assert!(resp.contains("relay-chain-ok"), "response: {resp}");
        std::thread::sleep(Duration::from_millis(200));
        let log_dir = data.join("logs");
        let read_logs = |prefix: &str| -> String {
            std::fs::read_dir(&log_dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
                .collect()
        };
        let relay_log = read_logs("relay-");
        let core_log: String = read_logs("core-")
            .lines()
            .filter(|l| l.contains("[TCP]"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            core_log.contains(&format!("{HOST}:{target}"))
                && core_log.contains("using agent-egress[relay]"),
            "core did not route the request through the relay:\n{core_log}"
        );
        assert!(
            relay_log.contains(&format!("{HOST}:{target} -> 127.0.0.1:{good}(socks5)"))
                && relay_log.contains("(1 failed attempt(s) before)"),
            "relay log: {relay_log}"
        );
    });

    let down = aec_dev(&data, &["core", "down"]);
    let gone = !listening(29893) && !listening(29325);
    let _ = std::fs::remove_dir_all(&data);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    assert!(gone, "core and relay ports must be free after core down");
}
