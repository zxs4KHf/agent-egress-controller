#![cfg(windows)]
use aec_core::guard_windows::{current_sid, Engine};
use std::net::{TcpListener, UdpSocket};
use std::time::Duration;

#[test]
#[ignore = "requires an elevated isolated Windows runner and guard_probe; never target real applications"]
fn wfp_persistent_rules_block_bypass_and_ipv6_even_after_engine_exit() {
    let sid = current_sid().unwrap();
    let scope = aec_core::crypto::random_hex(16).unwrap();
    let root = std::env::temp_dir().join(format!("aec-wfp-{scope}"));
    std::fs::create_dir(&root).unwrap();
    let fixture = root.join("isolated-probe.exe");
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/examples/guard_probe.exe");
    std::fs::copy(source, &fixture).unwrap();
    struct Cleanup {
        sid: String,
        scope: String,
        root: std::path::PathBuf,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(engine) = Engine::open_test(&self.sid, &self.scope, true) {
                let _ = engine.apply(1, &[]);
                let _ = engine.destroy_empty();
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    let _cleanup = Cleanup {
        sid: sid.clone(),
        scope: scope.clone(),
        root,
    };
    let allowed = TcpListener::bind("127.0.0.1:0").unwrap();
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let other_ip = TcpListener::bind("127.0.0.2:0").unwrap();
    let ipv6 = TcpListener::bind("[::1]:0").unwrap();
    let probe = |address: std::net::SocketAddr| {
        std::process::Command::new(&fixture)
            .args(["tcp", &address.to_string()])
            .status()
            .unwrap()
            .success()
    };
    assert!(
        probe(other.local_addr().unwrap()),
        "Baseline wrong-port connection must work before protection"
    );
    assert!(
        probe(ipv6.local_addr().unwrap()),
        "Baseline IPv6 connection must work before protection"
    );
    let engine = Engine::open_test(&sid, &scope, true)
        .expect("Real WFP test needs administrator privileges");
    let port = allowed.local_addr().unwrap().port();
    assert_eq!(
        engine
            .apply(port, &[fixture.to_string_lossy().into_owned()])
            .unwrap(),
        4
    );
    assert!(
        probe(allowed.local_addr().unwrap()),
        "The selected IPv4 proxy remains reachable"
    );
    assert!(
        !probe(other.local_addr().unwrap()),
        "Another loopback proxy must be blocked"
    );
    assert!(
        !probe(other_ip.local_addr().unwrap()),
        "Other IPv4 destinations must be blocked"
    );
    assert!(!probe(ipv6.local_addr().unwrap()), "IPv6 must be blocked");
    // Parent/application not selected: independent connectivity remains available.
    assert!(std::net::TcpStream::connect(other.local_addr().unwrap()).is_ok());
    let udp = UdpSocket::bind(("127.0.0.1", port)).unwrap();
    udp.set_read_timeout(Some(Duration::from_millis(800)))
        .unwrap();
    let echo = std::thread::spawn(move || {
        let mut b = [0; 16];
        if let Ok((n, source)) = udp.recv_from(&mut b) {
            udp.send_to(&b[..n], source).unwrap();
        }
    });
    assert!(
        !std::process::Command::new(&fixture)
            .args(["udp", &format!("127.0.0.1:{port}")])
            .status()
            .unwrap()
            .success(),
        "UDP cannot use the allowed TCP port as a bypass"
    );
    echo.join().unwrap();
    drop(engine);
    assert!(
        !probe(other.local_addr().unwrap()),
        "Persistent policy must survive its installer/manager exiting"
    );
    let engine = Engine::open_test(&sid, &scope, true).unwrap();
    assert_eq!(engine.count().unwrap(), 4);
    assert_eq!(engine.apply(port, &[]).unwrap(), 0);
    assert!(
        probe(other.local_addr().unwrap()),
        "Explicit release restores the fixture's ordinary connectivity"
    );
}
