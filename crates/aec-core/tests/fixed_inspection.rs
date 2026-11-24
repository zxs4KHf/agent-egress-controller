use aec_core::{
    controller::Fault,
    fixed::{self, Inspection, InspectionOutcome},
    http::{HttpClient, HttpError, Request, Response, Route},
};

#[test]
fn a_provider_timeout_does_not_erase_independent_exact_exit_evidence() {
    struct Providers;
    impl HttpClient for Providers {
        fn send(&self, request: &Request) -> Result<Response, HttpError> {
            assert_eq!(request.route, Route::Proxy(17327));
            match request.url.as_str() {
                "https://www.cloudflare.com/cdn-cgi/trace" => Err(HttpError::Timeout),
                "https://api-ipv4.ip.sb/ip" => Ok(Response {
                    status: 200,
                    body: "198.51.100.7\n".into(),
                    elapsed: Duration::ZERO,
                }),
                _ => panic!("unexpected fixed identity service"),
            }
        }
    }
    assert_eq!(
        fixed::verify(&Providers, 17327, Some("198.51.100.7")),
        Ok(())
    );
}

#[test]
fn a_known_ip_mismatch_never_consults_the_backup_service() {
    struct WrongExit;
    impl HttpClient for WrongExit {
        fn send(&self, request: &Request) -> Result<Response, HttpError> {
            assert_eq!(request.route, Route::Proxy(17327));
            assert_eq!(request.url, "https://www.cloudflare.com/cdn-cgi/trace");
            Ok(Response {
                status: 200,
                body: "ip=198.51.100.8\n".into(),
                elapsed: Duration::ZERO,
            })
        }
    }
    assert!(fixed::verify(&WrongExit, 17327, Some("198.51.100.7")).is_err());
}
use std::{
    cell::Cell,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    time::Duration,
};

struct Relay {
    connection: TcpStream,
    verified: Cell<bool>,
    valid_evidence: bool,
}
impl Inspection for Relay {
    fn verified(&self) -> bool {
        self.verified.get()
    }
    fn verify(&self) -> Result<(), Fault> {
        if self.valid_evidence {
            Ok(())
        } else {
            Err(Fault::new(
                "FIXED_EXIT_UNVERIFIED",
                "fixture evidence unavailable",
            ))
        }
    }
    fn block(&self) -> Result<(), Fault> {
        self.verified.set(false);
        self.connection
            .shutdown(Shutdown::Both)
            .map_err(|_| Fault::new("BLOCK_FAILED", "fixture close failed"))
    }
    fn open(&self) -> Result<(), Fault> {
        self.verify()?;
        self.verified.set(true);
        Ok(())
    }
}

#[test]
fn healthy_recheck_preserves_an_in_progress_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (mut remote, _) = listener.accept().unwrap();
    remote
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let gate = Relay {
        connection: remote.try_clone().unwrap(),
        verified: Cell::new(true),
        valid_evidence: true,
    };
    client.write_all(b"before").unwrap();
    let mut before = [0; 6];
    remote.read_exact(&mut before).unwrap();
    assert_eq!(&before, b"before");
    assert!(matches!(fixed::inspect(&gate), InspectionOutcome::Healthy));
    client.write_all(b"after").unwrap();
    let mut after = [0; 5];
    remote
        .read_exact(&mut after)
        .expect("A healthy recheck must not close the relay's existing user connection");
    assert_eq!(&after, b"after");
}

#[test]
fn missing_fixed_identity_evidence_still_closes_the_protected_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut remote, _) = listener.accept().unwrap();
    remote
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let gate = Relay {
        connection: remote.try_clone().unwrap(),
        verified: Cell::new(true),
        valid_evidence: false,
    };
    assert!(matches!(
        fixed::inspect(&gate),
        InspectionOutcome::Blocked {
            block_error: None,
            ..
        }
    ));
    assert!(!gate.verified());
    let result = remote.read(&mut [0; 1]);
    assert!(result.is_err() || result.is_ok_and(|n| n == 0));
}
