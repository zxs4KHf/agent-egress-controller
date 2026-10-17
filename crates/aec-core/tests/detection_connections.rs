#![cfg(windows)]
use aec_core::http::{HttpClient, Request, Route, WinHttpClient};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

#[test]
fn running_core_log_evidence_survives_utc_day_change() {
    use aec_core::checks::CoreObserver;
    use aec_core::core::{preflight::OwnedProcess, process::CoreManager, CorePorts};
    let data = std::env::temp_dir().join(format!(
        "aec-log-day-{}",
        aec_core::crypto::random_hex(8).unwrap()
    ));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(data.clone());
    std::fs::create_dir_all(data.join("core")).unwrap();
    std::fs::create_dir_all(data.join("logs")).unwrap();
    let owned = OwnedProcess {
        role: "core".into(),
        pid: 0,
        exe: "isolated-fixture.exe".into(),
        created: 1,
        started_at_unix_ms: 946_684_800_000,
    };
    std::fs::write(
        data.join("core/owned.json"),
        serde_json::to_vec(&owned).unwrap(),
    )
    .unwrap();
    let log = data.join("logs/core-20000101.log");
    std::fs::write(&log, "").unwrap();
    let manager = CoreManager {
        data_dir: data,
        bin: "unused.exe".into(),
        relay_bin: "unused-relay.exe".into(),
        ports: CorePorts::default(),
    };
    let observer = aec_core::live::CoreLogObserver::for_core(&manager);
    let mark = observer.mark();
    std::fs::write(&log, "time=\"2000-01-02T01:00:00Z\" level=info msg=\"[TCP] 127.0.0.1:45000 --> claude.ai:443 match DomainSuffix(claude.ai) using relay[aec-relay-transport]\"\n").unwrap();
    let records = observer.records_since(mark).expect("Detection must read the running core's existing log after midnight, not a nonexistent new-day file");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].host, "claude.ai");
}

#[test]
fn each_detection_request_has_fresh_proxy_connection_evidence() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicUsize::new(0));
    let (done, accepted) = (stop.clone(), count.clone());
    let server = std::thread::spawn(move || {
        let mut clients = Vec::new();
        while !done.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            accepted.fetch_add(1, Ordering::Relaxed);
            let done = done.clone();
            clients.push(std::thread::spawn(move || {
                stream.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
                let mut header = Vec::new();
                let mut byte = [0];
                while !done.load(Ordering::Relaxed) {
                    match stream.read(&mut byte) {
                        Ok(0) => break,
                        Ok(_) => header.push(byte[0]),
                        Err(_) => continue,
                    }
                    if header.ends_with(b"\r\n\r\n") {
                        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nOK").unwrap();
                        header.clear();
                    }
                }
            }));
        }
        for client in clients {
            client.join().unwrap();
        }
    });
    let results: Vec<_> = (0..2)
        .map(|_| {
            WinHttpClient.send(&Request::get(
                "http://probe.example.test/",
                Route::Proxy(port),
            ))
        })
        .collect();
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    for result in results {
        assert_eq!(result.unwrap().status, 200);
    }
    assert_eq!(
        count.load(Ordering::Relaxed),
        2,
        "Cross-session pooling hides this run's proxy/domain evidence"
    );
}
