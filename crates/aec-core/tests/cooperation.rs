#![cfg(windows)]
use aec_core::{
    cooperation::{self, Selection},
    core::{process::CoreManager, CorePorts},
    windows::WindowsHost,
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

struct Proxy {
    port: u16,
    stop: Arc<AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
    connects: Arc<AtomicUsize>,
}
impl Proxy {
    fn start(marker: u8) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let connects = Arc::new(AtomicUsize::new(0));
        let counter = connects.clone();
        let task = std::thread::spawn(move || {
            let mut tasks = Vec::new();
            while !done.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let counter = counter.clone();
                tasks.push(std::thread::spawn(move || {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut first = [0];
                    if stream.read_exact(&mut first).is_err() {
                        return;
                    }
                    if first[0] == 5 {
                        let mut count = [0];
                        if stream.read_exact(&mut count).is_err() {
                            return;
                        }
                        let mut methods = vec![0; count[0] as usize];
                        if stream.read_exact(&mut methods).is_err()
                            || stream.write_all(&[5, 0]).is_err()
                        {
                            return;
                        }
                        let mut header = [0; 4];
                        if stream.read_exact(&mut header).is_err() {
                            return;
                        }
                        let len = match header[3] {
                            1 => 4,
                            4 => 16,
                            3 => {
                                if stream.read_exact(&mut count).is_err() {
                                    return;
                                }
                                count[0] as usize
                            }
                            _ => return,
                        };
                        if stream.read_exact(&mut vec![0; len + 2]).is_err()
                            || stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).is_err()
                        {
                            return;
                        }
                        if stream.read_exact(&mut first).is_ok() {
                            let _ = stream.write_all(&[marker]);
                        }
                        return;
                    }
                    let mut header = vec![first[0]];
                    let mut b = [0];
                    while header.len() < 4096 && !header.ends_with(b"\r\n\r\n") {
                        if stream.read_exact(&mut b).is_err() {
                            return;
                        }
                        header.push(b[0]);
                    }
                    if !header.starts_with(b"CONNECT ") {
                        return;
                    }
                    counter.fetch_add(1, Ordering::Relaxed);
                    if stream
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .is_ok()
                        && stream.read_exact(&mut b).is_ok()
                    {
                        let _ = stream.write_all(&[marker]);
                    }
                }));
            }
            for task in tasks {
                task.join().unwrap();
            }
        });
        Self {
            port,
            stop,
            task: Some(task),
            connects,
        }
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

struct Core(CoreManager);
impl Core {
    fn new(base: u16) -> Self {
        // Windows/Hyper-V may reserve UDP ranges without a visible listener.
        let base = (base..=65000)
            .step_by(8)
            .find(|base| {
                (0..7).all(|offset| {
                    let tcp = TcpListener::bind(("127.0.0.1", *base + offset));
                    let udp = UdpSocket::bind(("127.0.0.1", *base + offset));
                    tcp.is_ok() && udp.is_ok()
                })
            })
            .expect("isolated TCP/UDP port block available");
        let root = std::env::temp_dir().join(format!(
            "aec-coop-{}",
            aec_core::crypto::random_hex(8).unwrap()
        ));
        Self(CoreManager {
            data_dir: root.clone(),
            bin: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/bin/aec-mihomo.exe"),
            relay_bin: root.join("unused-relay.exe"),
            ports: CorePorts {
                mixed: base,
                controller: base + 1,
                dns: base + 2,
                relay: base + 3,
                tunnel: base + 4,
            },
        })
    }
    fn start(&self, yaml: &str) {
        self.0
            .up(&WindowsHost, |ports, secret| {
                use serde_yaml_ng::Value;
                let mut config: Value = serde_yaml_ng::from_str(yaml).unwrap();
                config["mixed-port"] = ports.mixed.into();
                config["allow-lan"] = false.into();
                config["bind-address"] = "127.0.0.1".into();
                config["external-controller"] = format!("127.0.0.1:{}", ports.controller).into();
                config["secret"] = secret.into();
                config["mode"] = "rule".into();
                config["log-level"] = "info".into();
                if config.get("dns").is_none() {
                    config["dns"] =
                        serde_yaml_ng::from_str("enable: true\nnameserver: [127.0.0.1:9]").unwrap();
                }
                config["dns"]["listen"] = format!("127.0.0.1:{}", ports.dns).into();
                serde_yaml_ng::to_string(&config).map_err(|_| "fixture config".into())
            })
            .unwrap();
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        if self.0.down(&WindowsHost).is_ok() && !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.0.data_dir);
        }
    }
}

fn request(port: u16, host: &str) -> Option<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(
        stream,
        "CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n"
    )
    .unwrap();
    let mut header = Vec::new();
    let mut b = [0];
    while header.len() < 4096 && !header.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut b).ok()?;
        header.push(b[0]);
    }
    if !String::from_utf8_lossy(&header).contains("200") {
        return None;
    }
    stream.write_all(b"x").ok()?;
    stream.read_exact(&mut b).ok()?;
    Some(b[0])
}

fn ordinary_config(port: u16) -> String {
    format!("dns:\n  enable: true\n  nameserver: [127.0.0.1:9]\nproxies:\n  - name: user-node\n    type: http\n    server: 127.0.0.1\n    port: {port}\nproxy-groups:\n  - name: user-choice\n    type: select\n    proxies: [user-node]\nrules:\n  - DOMAIN-SUFFIX,arena.ai,user-choice\n  - MATCH,user-choice\n")
}

#[test]
#[ignore = "requires pinned mihomo; isolated controller readback only"]
fn running_rule_evidence_uses_real_pinned_api_without_claiming_dns_or_endpoint_proof() {
    let fixed = Proxy::start(b'F');
    let ordinary = Proxy::start(b'O');
    let selection = Selection {
        domains: vec!["claude.ai".into(), "anthropic.com".into()],
        fixed_port: fixed.port,
    };
    let core = Core::new(51000);
    core.start(
        &cooperation::prepare(&ordinary_config(ordinary.port), &selection)
            .unwrap()
            .candidate_yaml(),
    );
    struct Local<'a>(&'a CoreManager);
    impl cooperation::RuntimeHost for Local<'_> {
        fn get(&self, path: &str) -> Result<serde_json::Value, &'static str> {
            let text = std::fs::read_to_string(self.0.config_path()).map_err(|_| "fixture")?;
            let config: serde_yaml_ng::Value =
                serde_yaml_ng::from_str(&text).map_err(|_| "fixture")?;
            let secret = config["secret"].as_str().ok_or("fixture")?;
            let (_, body) = aec_core::core::process::http_get_local(
                self.0.ports.controller,
                path,
                Some(secret),
            )
            .map_err(|_| "fixture")?;
            serde_json::from_str(&body).map_err(|_| "fixture")
        }
    }
    let evidence = cooperation::verify_runtime(&Local(&core.0), &selection);
    assert_eq!(evidence["rules"], "pass", "{evidence}");
    assert_eq!(evidence["dnsPolicy"], "unknown");
    assert_eq!(evidence["endpoint"], "unknown");
    assert_eq!(evidence["runtimeVerified"], false);
}

#[test]
#[ignore = "requires pinned mihomo; isolated Windows named-pipe controller"]
fn default_windows_pipe_controller_reads_rules_with_tcp_controller_disabled() {
    use cooperation::{windows::ControllerChannel, RuntimeHost};
    let fixed = Proxy::start(b'F');
    let ordinary = Proxy::start(b'O');
    let selection = Selection {
        domains: vec!["claude.ai".into(), "anthropic.com".into()],
        fixed_port: fixed.port,
    };
    let core = Core::new(52000);
    let pipe = format!(
        r"\\.\pipe\aec-test-{}",
        aec_core::crypto::random_hex(12).unwrap()
    );
    let yaml = cooperation::prepare(&ordinary_config(ordinary.port), &selection)
        .unwrap()
        .candidate_yaml()
        + &format!("\nexternal-controller-pipe: '{pipe}'\n");
    core.start(&yaml);
    // The extra TCP endpoint belongs only to CoreManager's isolated lifecycle. The
    // client-facing generated config matches CVR's default: pipe, no TCP, no secret.
    let root = core.0.data_dir.join("client");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("clash-verge.yaml"),
        format!("external-controller: ''\nexternal-controller-pipe: '{pipe}'\n"),
    )
    .unwrap();
    let channel = ControllerChannel::open(&root).unwrap_or_else(|e| {
        panic!("Default Windows pipe must be usable without enabling TCP: {e}")
    });
    assert_eq!(channel.get("/version").unwrap()["version"], "v1.19.31");
    let evidence = cooperation::verify_runtime(&channel, &selection);
    assert_eq!(evidence["rules"], "pass", "{evidence}");
    assert_eq!(evidence["runtimeVerified"], false);
    assert_eq!(request(core.0.ports.mixed, "claude.ai"), Some(b'F'));
    assert_eq!(request(core.0.ports.mixed, "google.com"), Some(b'O'));
    let mut system = aec_core::model::ProxySettings {
        flags: aec_core::model::proxy_flags::DIRECT | aec_core::model::proxy_flags::PROXY,
        proxy_server: Some(format!("127.0.0.1:{}", core.0.ports.mixed)),
        bypass: Some("<local>;localhost;127.*;10.*;192.168.*;172.16.*".into()),
        auto_config_url: None,
    };
    let scope = cooperation::scope_check(&channel, &selection, &system);
    assert_eq!(
        scope.reason.as_deref(),
        Some("cooperation_endpoint_dns_unverified")
    );
    system.flags = aec_core::model::proxy_flags::DIRECT;
    let off = cooperation::scope_check(&channel, &selection, &system);
    assert_eq!(off.status, aec_core::checks::Status::Fail);
    system.flags |= aec_core::model::proxy_flags::PROXY;
    system.bypass = Some("*".into());
    let bypass = cooperation::scope_check(&channel, &selection, &system);
    assert_eq!(
        bypass.reason.as_deref(),
        Some("cooperation_proxy_bypass_unverified")
    );
    system.bypass = Some("claude.ai".into());
    assert_eq!(
        cooperation::scope_check(&channel, &selection, &system)
            .reason
            .as_deref(),
        Some("cooperation_proxy_bypass_unverified")
    );
    system.bypass = None;
    system.proxy_server = Some(format!("127.0.0.1:{}", ordinary.port));
    assert_eq!(
        cooperation::scope_check(&channel, &selection, &system)
            .reason
            .as_deref(),
        Some("cooperation_system_proxy_bypassed")
    );
    // Re-enabling the same verified entry requires fresh rule evidence, never a stale pass.
    system.proxy_server = Some(format!("127.0.0.1:{}", core.0.ports.mixed));
    assert_eq!(
        cooperation::scope_check(&channel, &selection, &system).status,
        aec_core::checks::Status::Unknown
    );
    assert!(channel.get("/configs?bad=1").is_err());
    assert!(channel
        .get("/dns/query?name=bad HTTP/1.1\r\n.test.dnscheck.tools&type=TXT")
        .is_err());
    std::fs::write(root.join("clash-verge.yaml"), "external-controller: ''\n").unwrap();
    assert_eq!(channel.get("/version").unwrap_err(), "COOP_RUNTIME_CHANGED");
}

#[test]
fn planning_refuses_to_forward_the_client_back_to_its_own_listener() {
    let current = ordinary_config(7890) + "\nmixed-port: 7897\n";
    assert!(
        cooperation::prepare(
            &current,
            &Selection {
                domains: vec!["claude.ai".into()],
                fixed_port: 7897,
            }
        )
        .is_err(),
        "A client-to-itself proxy loop must not be generated"
    );
}

#[test]
fn cooperation_rechecks_rules_after_system_proxy_is_reenabled() {
    use aec_core::{
        checks::Status,
        model::{proxy_flags, ProxySettings},
    };
    use std::cell::Cell;
    struct Client {
        installed: Cell<bool>,
        reads: Cell<usize>,
    }
    impl cooperation::RuntimeHost for Client {
        fn get(&self, path: &str) -> Result<serde_json::Value, &'static str> {
            self.reads.set(self.reads.get() + 1);
            Ok(match path {
                "/configs" => serde_json::json!({"mixed-port":7897,"mode":"rule"}),
                "/proxies" if self.installed.get() => {
                    serde_json::json!({"proxies":{"AEC-MANAGED-FIXED":{"type":"Http"}}})
                }
                "/proxies" => serde_json::json!({"proxies":{}}),
                "/rules" => serde_json::json!({"rules":[
                    {"type":"AND","payload":"((DomainSuffix,claude.ai) && (Network,udp))","proxy":"REJECT"},
                    {"type":"DomainSuffix","payload":"claude.ai","proxy":"AEC-MANAGED-FIXED"}
                ]}),
                _ => return Err("UNEXPECTED_READ"),
            })
        }
    }
    let client = Client {
        installed: Cell::new(true),
        reads: Cell::new(0),
    };
    let selection = Selection::for_sites(vec!["claude.ai".into()]);
    let mut system = ProxySettings {
        flags: proxy_flags::DIRECT,
        proxy_server: Some("127.0.0.1:7897".into()),
        bypass: None,
        auto_config_url: None,
    };
    // Starting AEC before Clash's switch is allowed, but cannot be called protected.
    assert_eq!(
        cooperation::scope_check(&client, &selection, &system).status,
        Status::Fail
    );
    assert_eq!(client.reads.get(), 0);
    system.flags |= proxy_flags::PROXY;
    assert_eq!(
        cooperation::scope_check(&client, &selection, &system)
            .reason
            .as_deref(),
        Some("cooperation_endpoint_dns_unverified")
    );
    system.flags = proxy_flags::DIRECT;
    assert_eq!(
        cooperation::scope_check(&client, &selection, &system).status,
        Status::Fail
    );
    client.installed.set(false); // Subscription changed while the system switch was off.
    system.flags |= proxy_flags::PROXY;
    let result = cooperation::scope_check(&client, &selection, &system);
    assert_eq!(result.status, Status::Fail);
    assert_eq!(
        result.reason.as_deref(),
        Some("cooperation_rules_missing_or_changed")
    );
    client.installed.set(true);
    assert_eq!(
        cooperation::scope_check(&client, &selection, &system).status,
        Status::Unknown,
        "matching rules are fresh evidence, never proof of browser-wide fixed exit/DNS"
    );
}

#[test]
fn selected_sites_cannot_inject_extra_rules_or_override_client_owned_names() {
    let current = ordinary_config(7890);
    for domain in [
        "https://claude.ai",
        "claude.ai,DIRECT",
        "*.ai",
        "",
        "127.0.0.1",
    ] {
        assert!(
            cooperation::prepare(
                &current,
                &Selection {
                    domains: vec![domain.into()],
                    fixed_port: 17328,
                }
            )
            .is_err(),
            "{domain}"
        );
    }
    let collision = current.replace("name: user-node", "name: AEC-MANAGED-FIXED");
    assert!(
        cooperation::prepare(
            &collision,
            &Selection {
                domains: vec!["claude.ai".into()],
                fixed_port: 17328,
            }
        )
        .is_err(),
        "An existing same-name client node is not owned by AEC"
    );
}

#[test]
fn planning_never_claims_protection_when_the_client_is_in_global_or_direct_mode() {
    for mode in ["global", "direct"] {
        let current = ordinary_config(7890) + &format!("\nmode: {mode}\n");
        assert!(
            cooperation::prepare(
                &current,
                &Selection {
                    domains: vec!["claude.ai".into()],
                    fixed_port: 17328,
                }
            )
            .is_err(),
            "Domain rules are bypassed in {mode} mode"
        );
    }
}

#[test]
fn planning_does_not_overwrite_an_existing_client_dns_policy() {
    let current = ordinary_config(7890).replace(
        "nameserver: [127.0.0.1:9]",
        "nameserver: [127.0.0.1:9]\n  nameserver-policy:\n    '+.claude.ai': [127.0.0.1:53]",
    );
    assert!(
        cooperation::prepare(
            &current,
            &Selection {
                domains: vec!["claude.ai".into()],
                fixed_port: 17328,
            }
        )
        .is_err(),
        "An existing DNS policy needs an explicit ownership/merge decision"
    );
}

#[test]
#[ignore = "requires the pinned mihomo; uses only isolated loopback proxies, no system writes"]
fn selected_sites_use_the_fixed_gateway_without_taking_over_other_rules() {
    let ordinary = Proxy::start(b'O');
    let fixed = Proxy::start(b'F');
    let original = ordinary_config(ordinary.port);
    let selection = Selection {
        domains: vec!["claude.ai".into(), "anthropic.com".into()],
        fixed_port: fixed.port,
    };
    let plan = cooperation::prepare(&original, &selection).unwrap();
    let candidate = plan.candidate_yaml();
    let before: serde_yaml_ng::Value = serde_yaml_ng::from_str(&original).unwrap();
    let after: serde_yaml_ng::Value = serde_yaml_ng::from_str(&candidate).unwrap();
    assert_eq!(after["proxy-groups"], before["proxy-groups"]);
    assert_eq!(after["proxies"][0], before["proxies"][0]);
    let front = Core::new(56890);
    front.start(&candidate);
    assert_eq!(request(front.0.ports.mixed, "claude.ai"), Some(b'F'));
    assert_eq!(
        request(front.0.ports.mixed, "api.anthropic.com"),
        Some(b'F')
    );
    assert_eq!(request(front.0.ports.mixed, "chatgpt.com"), Some(b'O'));
    assert_eq!(request(front.0.ports.mixed, "arena.ai"), Some(b'O'));
}

#[test]
#[ignore = "requires pinned mihomo; sends UDP only to a loopback capture socket"]
fn protected_udp_must_not_fall_through_to_a_later_direct_rule() {
    let ordinary = Proxy::start(b'O');
    let fixed = Proxy::start(b'F');
    let capture = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    capture
        .set_read_timeout(Some(Duration::from_millis(1000)))
        .unwrap();
    let destination = capture.local_addr().unwrap().port();
    let host = "protected.example.test";
    let original = ordinary_config(ordinary.port).replace("MATCH,user-choice", "MATCH,DIRECT")
        + "\nhosts:\n  protected.example.test: 127.0.0.1\n";
    let plan = cooperation::prepare(
        &original,
        &Selection {
            domains: vec![host.into()],
            fixed_port: fixed.port,
        },
    )
    .unwrap();
    let front = Core::new(54890);
    front.start(&plan.candidate_yaml());
    let mut association = TcpStream::connect(("127.0.0.1", front.0.ports.mixed)).unwrap();
    association
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    association.write_all(&[5, 1, 0]).unwrap();
    let mut method = [0; 2];
    association.read_exact(&mut method).unwrap();
    assert_eq!(method, [5, 0]);
    association
        .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
        .unwrap();
    let mut address = [0; 10];
    association.read_exact(&mut address).unwrap();
    assert_eq!(&address[..4], &[5, 0, 0, 1]);
    let port = u16::from_be_bytes([address[8], address[9]]);
    let sender = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let mut packet = vec![0, 0, 0, 3, host.len() as u8];
    packet.extend(host.as_bytes());
    packet.extend(destination.to_be_bytes());
    packet.extend(b"must-not-escape");
    sender.send_to(&packet, ("127.0.0.1", port)).unwrap();
    assert!(
        capture.recv_from(&mut [0; 256]).is_err(),
        "Protected UDP must be rejected before an unsupported HTTP rule can fall through to DIRECT"
    );
}

#[test]
#[ignore = "requires two pinned cores and loopback proxy fixtures; never changes production"]
fn cooperative_gateway_is_guarded_even_for_dns_and_has_no_ordinary_fallback() {
    use aec_core::{
        core::config,
        fixed,
        profile::{Candidate, ProfileSet, Target, UpstreamProto},
    };
    let ordinary = Proxy::start(b'O');
    let isp = Proxy::start(b'F');
    let mut backend = Core::new(58890);
    backend.0.ports.relay = isp.port;
    let ingress = fixed::ingress_port(&backend.0.ports).unwrap();
    let mut set = ProfileSet::default();
    let p = &mut set.agent[0];
    p.egress.fixed_exit = true;
    p.egress.expected.ip = Some("198.51.100.7".into());
    p.egress.candidates = vec![Candidate {
        id: "fixture".into(),
        host: "198.51.100.7".into(),
        port: 12323,
        proto: UpstreamProto::Http,
        credential_ref: None,
    }];
    p.targets = vec![Target {
        domain: "claude.ai".into(),
        trace: true,
    }];
    p.routing.final_target = "DIRECT".into();
    backend
        .0
        .up(&WindowsHost, |ports, secret| {
            config::from_profile(p, ports, secret, &config::NoCredentials)
                .map_err(|_| "fixture config".into())
        })
        .unwrap();
    let plan = cooperation::prepare(
        &ordinary_config(ordinary.port),
        &Selection {
            domains: vec!["claude.ai".into()],
            fixed_port: ingress,
        },
    )
    .unwrap();
    let front = Core::new(57890);
    front.start(&plan.candidate_yaml());
    assert_eq!(
        request(front.0.ports.mixed, "claude.ai"),
        None,
        "starts blocked"
    );
    assert_eq!(request(front.0.ports.mixed, "chatgpt.com"), Some(b'O'));
    backend
        .0
        .select(&WindowsHost, "relay", fixed::TRANSPORT)
        .unwrap();
    assert_eq!(request(front.0.ports.mixed, "claude.ai"), Some(b'F'));
    assert_eq!(
        request(ingress, "resolver.example.test"),
        Some(b'F'),
        "DNS transport must not use the backend's ordinary DIRECT final rule"
    );
    backend.0.select(&WindowsHost, "relay", "REJECT").unwrap();
    assert_eq!(request(front.0.ports.mixed, "claude.ai"), None);
    assert_eq!(request(ingress, "resolver.example.test"), None);
    assert_eq!(request(front.0.ports.mixed, "arena.ai"), Some(b'O'));
    backend.0.down(&WindowsHost).unwrap();
    assert_eq!(request(front.0.ports.mixed, "claude.ai"), None);
    assert_eq!(request(front.0.ports.mixed, "chatgpt.com"), Some(b'O'));
}

#[test]
#[ignore = "pinned core; DNS failure observation uses only loopback capture sockets"]
fn protected_dns_uses_fixed_transport_and_never_falls_back_to_ordinary_dns() {
    let ordinary = Proxy::start(b'O');
    let fixed = Proxy::start(b'F');
    let capture = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    capture
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut input: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&ordinary_config(ordinary.port)).unwrap();
    input["dns"] = serde_yaml_ng::from_str(&format!(
        "enable: true\nnameserver: [127.0.0.1:{}]\nfallback: [127.0.0.1:{}]\nfallback-filter: {{geoip: false}}\n",
        capture.local_addr().unwrap().port(), capture.local_addr().unwrap().port())).unwrap();
    let original = serde_yaml_ng::to_string(&input).unwrap();
    let plan = cooperation::prepare(
        &original,
        &Selection {
            domains: vec!["protected.example.test".into()],
            fixed_port: fixed.port,
        },
    )
    .unwrap();
    let front = Core::new(52890);
    front.start(&plan.candidate_yaml());
    let query = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    query
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let mut packet = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in ["protected", "example", "test"] {
        packet.push(label.len() as u8);
        packet.extend(label.as_bytes());
    }
    packet.extend([0, 0, 1, 0, 1]);
    query
        .send_to(&packet, ("127.0.0.1", front.0.ports.dns))
        .unwrap();
    let _ = query.recv_from(&mut [0; 512]);
    assert!(
        fixed.connects.load(Ordering::Relaxed) > 0,
        "A protected DNS query must attempt the fixed proxy transport, not the user's ordinary DNS"
    );
    assert!(
        capture.recv_from(&mut [0; 512]).is_err(),
        "Fixed DoH failure must not send the query to ordinary DNS or fallback"
    );
}

#[test]
#[ignore = "pinned core; synthetic DNS wire ECS and padding captured on loopback only"]
fn dns_standard_ecs_is_zeroed_on_real_upstream_wire_without_removing_other_opt_options() {
    let capture = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    capture
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let address = capture.local_addr().unwrap();
    let task = std::thread::spawn(move || {
        let mut wire = [0u8; 2048];
        let (len, peer) = capture.recv_from(&mut wire).unwrap();
        let request = wire[..len].to_vec();
        wire[2] = 0x81;
        wire[3] = 0x83; // NXDOMAIN, preserving the actual question and OPT.
        capture.send_to(&wire[..len], peer).unwrap();
        request
    });
    let core = Core::new(50200);
    core.start(&format!("dns:\n  enable: true\n  nameserver: ['udp://{address}#DIRECT&ecs=0.0.0.0/0&ecs-override=true']\nproxies: []\nrules: [MATCH,DIRECT]\n").replace("rules: [MATCH,DIRECT]","rules: ['MATCH,DIRECT']"));
    let query = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    query
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut packet = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 1];
    for label in ["ecs", "example", "test"] {
        packet.push(label.len() as u8);
        packet.extend(label.as_bytes());
    }
    packet.extend([0, 0, 1, 0, 1]);
    // OPT: nonzero IPv4 /24 ECS plus unrelated two-byte padding option.
    packet.extend([
        0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 17, 0, 8, 0, 7, 0, 1, 24, 0, 203, 0, 113, 0, 12, 0, 2, 0,
        0,
    ]);
    query
        .send_to(&packet, ("127.0.0.1", core.0.ports.dns))
        .unwrap();
    let _ = query.recv_from(&mut [0; 2048]);
    let upstream = task.join().unwrap();
    assert!(
        upstream.windows(8).any(|w| w == [0, 8, 0, 4, 0, 1, 0, 0]),
        "standard ECS must be /0 with no identifying address bytes"
    );
    assert!(
        upstream.windows(6).any(|w| w == [0, 12, 0, 2, 0, 0]),
        "unrelated OPT padding must survive"
    );
    assert!(!upstream.windows(3).any(|w| w == [203, 0, 113]));
}

#[test]
#[ignore = "pinned core and synthetic authoritative TXT response; no external DNS"]
fn dns_authority_detector_accepts_the_actual_pinned_controller_json_contract() {
    let dns = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    dns.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let port = dns.local_addr().unwrap().port();
    let task = std::thread::spawn(move || {
        let mut buffer = [0; 2048];
        let (len, peer) = dns.recv_from(&mut buffer).unwrap();
        let request = &buffer[..len];
        let mut end = 12;
        while request[end] != 0 {
            end += request[end] as usize + 1;
        }
        end += 5;
        let mut reply = vec![request[0], request[1], 0x81, 0x80, 0, 1, 0, 2, 0, 0, 0, 0];
        reply.extend(&request[12..end]);
        for text in ["FROM: 192.0.2.8#1234 (udp)", "ID: 4321"] {
            reply.extend([0xc0, 0x0c, 0, 16, 0, 1, 0, 0, 0, 1]);
            reply.extend(((text.len() + 1) as u16).to_be_bytes());
            reply.push(text.len() as u8);
            reply.extend(text.as_bytes());
        }
        dns.send_to(&reply, peer).unwrap();
    });
    let core = Core::new(49400);
    core.start(&format!("dns:\n  enable: true\n  nameserver: [127.0.0.1:{port}]\nproxies: []\nrules: ['MATCH,DIRECT']\n"));
    let config: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&std::fs::read_to_string(core.0.config_path()).unwrap()).unwrap();
    let (status, body) = aec_core::core::process::http_get_local(
        core.0.ports.controller,
        "/dns/query?name=abcdef12.test.dnscheck.tools&type=TXT",
        config["secret"].as_str(),
    )
    .unwrap();
    task.join().unwrap();
    assert_eq!(status, 200);
    let reply: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(reply["Question"][0]["Qclass"], 1);
    assert_eq!(
        cooperation::evaluate_dns(&reply, "abcdef12", 500)["status"],
        "pass"
    );
}
