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
            // Longer than the default relay's 1200 ms handshake timeout: a successful CONNECT
            // must remain usable while the target prepares its first response.
            std::thread::sleep(Duration::from_millis(1800));
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

#[cfg(windows)]
#[test]
#[ignore = "needs pinned mihomo and aec-relay; loopback only, no global state"]
fn fixed_gate_blocks_reload_and_closes_existing_connections() {
    use aec_core::{
        core::{config, process::CoreManager, CorePorts},
        profile::{Candidate, ProfileSet, UpstreamProto},
        windows::WindowsHost,
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let data = std::env::temp_dir().join(format!("aec-fixed-chain-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    // Windows runners use an 8.3 alias in TEMP. Exercise that spelling locally too,
    // when available: canonicalizing only the reload filename must not break -d safety.
    let data = {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        #[link(name = "kernel32")]
        extern "system" {
            fn GetShortPathNameW(input: *const u16, output: *mut u16, length: u32) -> u32;
        }
        let input: Vec<u16> = data.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut output = vec![0u16; 32768];
        let length =
            unsafe { GetShortPathNameW(input.as_ptr(), output.as_mut_ptr(), output.len() as u32) }
                as usize;
        if length > 0 && length < output.len() {
            PathBuf::from(std::ffi::OsString::from_wide(&output[..length]))
        } else {
            data
        }
    };
    let mut manager = CoreManager::dev(&root, &data);
    manager.relay_bin = PathBuf::from(env!("CARGO_BIN_EXE_aec-relay"));
    manager.ports = CorePorts {
        mixed: 32893,
        controller: 32909,
        dns: 32553,
        relay: 32325,
        tunnel: 32326,
    };
    let mut p = ProfileSet::default().agent.remove(0);
    p.egress.fixed_exit = true;
    p.egress.expected.ip = Some("198.51.100.7".into());
    p.egress.candidates = vec![Candidate {
        id: "fake".into(),
        host: "127.0.0.1".into(),
        port: socks5_upstream(),
        proto: UpstreamProto::Socks5,
        credential_ref: None,
    }];
    p.targets = vec![aec_core::profile::Target {
        domain: HOST.into(),
        trace: false,
    }];
    p.routing.rules.clear();
    p.routing.final_target = "DIRECT".into();
    let relay = config::relay_config(&p, &manager.ports, &config::NoCredentials).unwrap();
    let target = http_target();
    let request = |port| {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
        write!(s,"GET http://{HOST}:{target}/ HTTP/1.1\r\nHost: {HOST}:{target}\r\nConnection: close\r\n\r\n").unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    };
    let result = std::panic::catch_unwind(|| {
        manager
            .up_with_relay(&WindowsHost, &relay, |ports, secret| {
                config::from_profile(&p, ports, secret, &config::NoCredentials)
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert!(
            !request(manager.ports.mixed).contains("relay-chain-ok"),
            "startup must be blocked"
        );
        assert!(
            request(32327).contains("relay-chain-ok"),
            "probe must work while gate is blocked"
        );
        manager
            .select(&WindowsHost, "relay", aec_core::fixed::TRANSPORT)
            .unwrap();
        assert!(
            request(manager.ports.mixed).contains("relay-chain-ok"),
            "protected rule must override DIRECT"
        );
        let hold = TcpListener::bind("127.0.0.1:0").unwrap();
        let hold_port = hold.local_addr().unwrap().port();
        let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut peer, _) = hold.accept().unwrap();
            let mut b = [0];
            peer.read_exact(&mut b).unwrap();
            let _ = arrived_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(20));
        });
        let mut active = TcpStream::connect(("127.0.0.1", manager.ports.mixed)).unwrap();
        active
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        write!(
            active,
            "CONNECT {HOST}:{hold_port} HTTP/1.1\r\nHost: {HOST}\r\n\r\n"
        )
        .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            active.read_exact(&mut b).unwrap();
            head.push(b[0]);
        }
        assert!(String::from_utf8(head).unwrap().contains("200"));
        active.write_all(b"x").unwrap();
        arrived_rx.recv_timeout(Duration::from_secs(8)).unwrap();
        manager
            .reload(&WindowsHost, &p, &config::NoCredentials)
            .unwrap();
        assert!(
            !request(manager.ports.mixed).contains("relay-chain-ok"),
            "reload must remain blocked"
        );
        let mut byte = [0];
        match active.read(&mut byte) {
            Ok(0) => {}
            Err(e) => assert!(!matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            )),
            other => panic!("old connection remained usable: {other:?}"),
        }
        let _ = release_tx.send(());
        assert!(
            request(32327).contains("relay-chain-ok"),
            "probe must still work after reload"
        );
        manager
            .select(&WindowsHost, "relay", aec_core::fixed::TRANSPORT)
            .unwrap();
        assert!(request(manager.ports.mixed).contains("relay-chain-ok"));
        // A user-authorized group switch removes only its own protection. Verify the actual
        // rendered configuration and core selection, including switching back while blocked.
        let mut set = ProfileSet::default();
        set.agent[0] = p.clone();
        set.agent[0].targets.push(aec_core::profile::Target {
            domain: "other-protected.test".into(),
            trace: false,
        });
        set.agent[0].routing.outbounds.push(serde_json::from_value(serde_json::json!({"name":"ordinary","kind":"socks5","server":"127.0.0.1","port":socks5_upstream()})).unwrap());
        set.agent[0].routing.groups.push(serde_json::from_value(serde_json::json!({"name":"Service","type":"select","members":["relay","ordinary"],"switchable":true})).unwrap());
        set.agent[0].routing.rules.push(
            serde_json::from_value(
                serde_json::json!({"kind":"domain_suffix","value":HOST,"target":"Service"}),
            )
            .unwrap(),
        );
        let ordinary = aec_core::route_switch::plan(
            &set,
            &aec_core::route_switch::Form {
                group: "Service".into(),
                member: "ordinary".into(),
            },
        )
        .unwrap();
        manager
            .reload(
                &WindowsHost,
                &ordinary.profiles.agent[0],
                &config::NoCredentials,
            )
            .unwrap();
        assert!(
            request(manager.ports.mixed).contains("relay-chain-ok"),
            "ordinary route must work while fixed relay is blocked"
        );
        assert!(ordinary.profiles.agent[0]
            .targets
            .iter()
            .any(|t| t.domain == "other-protected.test"));
        let fixed = aec_core::route_switch::plan(
            &ordinary.profiles,
            &aec_core::route_switch::Form {
                group: "Service".into(),
                member: "relay".into(),
            },
        )
        .unwrap();
        manager
            .reload(
                &WindowsHost,
                &fixed.profiles.agent[0],
                &config::NoCredentials,
            )
            .unwrap();
        assert!(
            !request(manager.ports.mixed).contains("relay-chain-ok"),
            "return to ISP stays blocked until verified"
        );
        manager
            .select(&WindowsHost, "relay", aec_core::fixed::TRANSPORT)
            .unwrap();
        assert!(request(manager.ports.mixed).contains("relay-chain-ok"));
    });
    let cleanup = manager.down(&WindowsHost);
    if cleanup.is_ok() {
        let _ = std::fs::remove_dir_all(&data);
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
    cleanup.unwrap();
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
