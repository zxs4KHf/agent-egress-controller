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
