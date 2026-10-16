use aec_core::{core::CorePorts, local_upstream as upstream, profile::*};

fn configured() -> ProfileSet {
    let mut set = ProfileSet::default();
    let p = &mut set.agent[0];
    p.egress.fixed_exit = true;
    p.egress.expected.ip = Some("198.51.100.8".into());
    p.egress.candidates.push(Candidate {
        id: "isp".into(),
        host: "198.51.100.8".into(),
        port: 12323,
        proto: UpstreamProto::Http,
        credential_ref: None,
    });
    p.targets = vec![Target {
        domain: "claude.ai".into(),
        trace: true,
    }];
    p.routing.rules.clear();
    p.routing.outbounds.push(
        serde_json::from_value(
            serde_json::json!({"name":"xxyun", "kind":"http", "server":"127.0.0.1", "port":7892}),
        )
        .unwrap(),
    );
    p.routing.groups.push(serde_json::from_value(serde_json::json!({"name":"GPT", "type":"select", "members":["relay","xxyun"], "selected":"xxyun", "switchable":true})).unwrap());
    p.routing.rules.push(
        serde_json::from_value(
            serde_json::json!({"kind":"domain_suffix", "value":"chatgpt.com", "target":"GPT"}),
        )
        .unwrap(),
    );
    p.routing.final_target = "xxyun".into();
    set
}

#[test]
fn claude_family_stays_pinned_even_if_the_editable_site_list_omits_it() {
    let mut set = configured();
    set.agent[0].targets = vec![Target {
        domain: "example.test".into(),
        trace: false,
    }];
    let yaml = aec_core::core::config::from_profile(
        &set.agent[0],
        &CorePorts::default(),
        "fixture",
        &aec_core::core::config::NoCredentials,
    )
    .unwrap();
    let rules: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).unwrap();
    let rules = rules["rules"].as_sequence().unwrap();
    for domain in [
        "claude.ai",
        "anthropic.com",
        "claude.com",
        "claudeusercontent.com",
    ] {
        assert!(
            rules
                .iter()
                .any(|rule| rule.as_str() == Some(&format!("DOMAIN-SUFFIX,{domain},relay"))),
            "Missing pinned service {domain}"
        );
    }
    assert!(aec_core::fixed::protects(
        &set.agent[0],
        "api.anthropic.com"
    ));
    assert!(!aec_core::fixed::protects(
        &set.agent[0],
        "anthropic.com.attacker.test"
    ));
}

#[test]
fn adding_and_selecting_local_upstreams_preserves_fixed_identity_and_existing_gpt_default() {
    let original = configured();
    let save = serde_json::from_value(serde_json::json!({"action":"save", "name":"Clash", "host":"127.0.0.1", "port":7890, "protocol":"socks5"})).unwrap();
    let saved = upstream::plan(&original, &save, CorePorts::default()).unwrap();
    assert_eq!(upstream::view(&saved).selected.as_deref(), Some("xxyun"));
    assert_eq!(
        saved.agent[0]
            .routing
            .groups
            .iter()
            .find(|g| g.name == "GPT")
            .unwrap()
            .selected
            .as_deref(),
        Some(upstream::GROUP)
    );
    let select =
        serde_json::from_value(serde_json::json!({"action":"select", "name":"Clash"})).unwrap();
    let next = upstream::plan(&saved, &select, CorePorts::default()).unwrap();
    assert_eq!(upstream::view(&next).selected.as_deref(), Some("Clash"));
    assert_eq!(next.agent[0].egress, original.agent[0].egress);
    assert_eq!(next.agent[0].targets, original.agent[0].targets);
    assert_eq!(next.agent[0].proxy_scope, original.agent[0].proxy_scope);
    assert!(validate(&next).is_empty());
}

#[test]
fn ordinary_tray_route_selection_never_removes_a_protected_domain() {
    let mut original = configured();
    let save = serde_json::from_value(serde_json::json!({"action":"save", "name":"Clash", "host":"127.0.0.1", "port":7890, "protocol":"http"})).unwrap();
    original = upstream::plan(&original, &save, CorePorts::default()).unwrap();
    original.agent[0].routing.rules.insert(0, serde_json::from_value(serde_json::json!({"kind":"domain_suffix", "value":"claude.ai", "target":upstream::GROUP})).unwrap());
    let next = aec_core::route_switch::plan(
        &original,
        &aec_core::route_switch::Form {
            group: upstream::GROUP.into(),
            member: "Clash".into(),
        },
    )
    .unwrap();
    assert_eq!(next.profiles.agent[0].targets, original.agent[0].targets);
    assert_eq!(next.profiles.agent[0].egress, original.agent[0].egress);
}

#[test]
fn local_proxy_used_as_a_tunnel_dialer_is_not_editable_as_an_ordinary_upstream() {
    let mut set = configured();
    let r = &mut set.agent[0].routing;
    r.groups.push(
        serde_json::from_value(
            serde_json::json!({"name":"transport", "type":"select", "members":["xxyun"]}),
        )
        .unwrap(),
    );
    r.outbounds.push(serde_json::from_value(serde_json::json!({"name":"ss", "kind":"ss", "server":"192.0.2.4", "port":443, "cipher":"chacha20-ietf-poly1305", "credentialRef":"test", "dialerProxy":"transport"})).unwrap());
    assert!(upstream::view(&set).entries.is_empty());
    let save = serde_json::from_value(serde_json::json!({"action":"save", "name":"xxyun", "host":"127.0.0.1", "port":7890, "protocol":"http"})).unwrap();
    assert!(upstream::plan(&set, &save, CorePorts::default()).is_err());
}

struct Panel(std::sync::Mutex<ProfileSet>);
impl aec_core::setup::SetupBackend for Panel {
    fn upstream(&self, command: upstream::Command) -> Result<serde_json::Value, Vec<FieldError>> {
        let mut set = self.0.lock().unwrap();
        *set = upstream::plan(&set, &command, CorePorts::default()).map_err(|message| {
            vec![FieldError {
                path: "upstream".into(),
                message,
            }]
        })?;
        Ok(serde_json::json!({"upstreams":upstream::view(&set)}))
    }
    fn view(&self) -> aec_core::setup::SetupView {
        aec_core::setup::view(
            &self.0.lock().unwrap(),
            &aec_core::setup::ViewInput {
                core_mixed_port: None,
                stored_user: None,
                tunnel_password_stored: false,
                takeover_enabled: false,
                locked: false,
            },
        )
    }
    fn save(&self, _: aec_core::setup::ExitForm) -> Result<(), Vec<FieldError>> {
        unreachable!()
    }
    fn dashboard(&self) -> Option<aec_core::dashboard::View> {
        Some(aec_core::dashboard::view(
            &self.0.lock().unwrap(),
            "daily",
            false,
            false,
            None,
        ))
    }
}

#[test]
fn panel_http_uses_the_same_commands_and_rejects_missing_origin_and_unknown_fields() {
    use std::io::{Read, Write};
    let backend = std::sync::Arc::new(Panel(std::sync::Mutex::new(configured())));
    let server =
        aec_core::setup::SetupServer::start(0, "upstream-test".into(), backend.clone()).unwrap();
    let send = |token: &str, origin: bool, body: &str| {
        let port = server.port();
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let origin = if origin {
            format!("Origin: http://127.0.0.1:{port}\r\n")
        } else {
            String::new()
        };
        write!(s,"POST /upstream?t={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{origin}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).unwrap();
        reply
    };
    let body =
        r#"{"action":"save","name":"Clash","host":"127.0.0.1","port":7890,"protocol":"http"}"#;
    assert!(send("wrong", true, body).starts_with("HTTP/1.1 403"));
    assert!(send("upstream-test", false, body).starts_with("HTTP/1.1 403"));
    assert_eq!(upstream::view(&backend.0.lock().unwrap()).entries.len(), 1);
    let reply = send("upstream-test", true, body);
    let result: serde_json::Value =
        serde_json::from_str(reply.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["result"]["upstreams"]["selected"], "xxyun");
    let reply = send(
        "upstream-test",
        true,
        r#"{"action":"select","name":"Clash","force":true}"#,
    );
    assert!(reply.contains("\"ok\":false"));
}

#[test]
fn agent_discovers_local_upstreams_without_credentials_or_global_configuration() {
    let panel = Panel(std::sync::Mutex::new(configured()));
    let result = aec_core::agent::call(
        &panel,
        aec_core::agent::Call {
            name: "aec_upstreams".into(),
            arguments: serde_json::json!({}),
        },
    )
    .unwrap();
    assert_eq!(result["selected"], "xxyun");
    assert_eq!(result["entries"][0]["port"], 7892);
    assert!(!result.to_string().contains("credential"));
}

#[test]
fn agent_can_save_select_and_delete_with_strict_fields() {
    let panel = Panel(std::sync::Mutex::new(configured()));
    let call = |name: &str, arguments| {
        aec_core::agent::call(
            &panel,
            aec_core::agent::Call {
                name: name.into(),
                arguments,
            },
        )
    };
    call(
        "aec_upstream_save",
        serde_json::json!({"name":"Clash", "host":"127.0.0.1", "port":7890, "protocol":"http"}),
    )
    .unwrap();
    call("aec_upstream_select", serde_json::json!({"name":"Clash"})).unwrap();
    assert_eq!(
        call("aec_upstreams", serde_json::json!({})).unwrap()["selected"],
        "Clash"
    );
    assert!(call(
        "aec_upstream_select",
        serde_json::json!({"name":"xxyun", "force":true})
    )
    .is_err());
    assert!(
        call("aec_upstream_delete", serde_json::json!({"name":"Clash"}))
            .unwrap_err()
            .contains("UPSTREAM_IN_USE")
    );
    call("aec_upstream_select", serde_json::json!({"name":"xxyun"})).unwrap();
    call("aec_upstream_delete", serde_json::json!({"name":"Clash"})).unwrap();
    assert_eq!(
        call("aec_upstreams", serde_json::json!({})).unwrap()["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn invalid_ports_hosts_and_duplicate_endpoints_leave_the_original_profile_intact() {
    let set = configured();
    let before = set.clone();
    for (host, port) in [
        ("127.0.0.1", 0),
        ("127.0.0.1", 17893),
        ("localhost", 7892),
        ("127.0.0.1", 17909),
        ("192.0.2.1", 7890),
    ] {
        let command = serde_json::from_value(serde_json::json!({"action":"save","name":"Clash","host":host,"port":port,"protocol":"http"})).unwrap();
        assert!(upstream::plan(&set, &command, CorePorts::default()).is_err());
        assert_eq!(set, before);
    }
}

#[test]
fn selecting_an_imported_upstream_cannot_loop_into_the_current_aec_listener() {
    let mut set = configured();
    set.agent[0].routing.outbounds[0].port = 7893;
    let ports = CorePorts {
        mixed: 7893,
        ..CorePorts::default()
    };
    assert!(upstream::plan(
        &set,
        &upstream::Command::Select {
            name: "xxyun".into()
        },
        ports
    )
    .is_err());
}

#[test]
fn socks_probe_uses_remote_dns_and_a_refused_target_is_not_a_pass() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut hello = [0; 2];
        s.read_exact(&mut hello).unwrap();
        assert_eq!(hello[0], 5);
        let mut methods = vec![0; hello[1] as usize];
        s.read_exact(&mut methods).unwrap();
        assert!(methods.contains(&0));
        assert!(
            !methods.contains(&1),
            "probe must not offer Windows/GSSAPI authentication"
        );
        s.write_all(&[5, 0]).unwrap();
        let mut request = [0; 5];
        s.read_exact(&mut request).unwrap();
        assert_eq!(&request[..4], &[5, 1, 0, 3]);
        let mut domain = vec![0; request[4] as usize];
        s.read_exact(&mut domain).unwrap();
        assert_eq!(domain, b"www.cloudflare.com");
        let mut target_port = [0; 2];
        s.read_exact(&mut target_port).unwrap();
        assert_eq!(u16::from_be_bytes(target_port), 443);
        s.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
    });
    let set = upstream::plan(
        &configured(),
        &upstream::Command::Save {
            name: "socks-test".into(),
            host: "127.0.0.1".into(),
            port,
            protocol: UpstreamProto::Socks5,
        },
        CorePorts::default(),
    )
    .unwrap();
    let result = upstream::probe(&set, "socks-test", CorePorts::default()).unwrap();
    server.join().unwrap();
    assert_ne!(result.status, "pass");
}

#[test]
fn an_auth_required_loopback_proxy_is_not_reported_as_usable_or_selected() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut buf = [0u8; 4096];
        let count = stream.read(&mut buf).unwrap();
        assert!(
            String::from_utf8_lossy(&buf[..count]).starts_with("CONNECT www.cloudflare.com:443 ")
        );
        stream.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let set = configured();
    let command = serde_json::from_value(serde_json::json!({"action":"save","name":"auth-required","host":"127.0.0.1","port":port,"protocol":"http"})).unwrap();
    let saved = upstream::plan(&set, &command, CorePorts::default()).unwrap();
    let result = upstream::probe(&saved, "auth-required", CorePorts::default()).unwrap();
    server.join().unwrap();
    assert_eq!(result.status, "fail");
    assert_eq!(result.code, "UPSTREAM_AUTH_REQUIRED");
    assert_eq!(upstream::view(&saved).selected.as_deref(), Some("xxyun"));
}

#[cfg(windows)]
#[test]
#[ignore = "needs pinned mihomo; isolated ports and local synthetic proxies only"]
fn real_core_switches_ordinary_upstreams_while_claude_remains_fail_closed() {
    use aec_core::{
        core::{config, process::CoreManager},
        windows::WindowsHost,
    };
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::Duration,
    };
    fn proxy(marker: u8, stop: Arc<AtomicBool>) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let thread = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let Ok((mut s, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                s.set_nonblocking(false).unwrap();
                s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut header = Vec::new();
                let mut b = [0];
                while header.len() < 4096 && !header.ends_with(b"\r\n\r\n") {
                    if s.read_exact(&mut b).is_err() {
                        break;
                    }
                    header.push(b[0]);
                }
                if !String::from_utf8_lossy(&header).starts_with("CONNECT chatgpt.com:443 ") {
                    continue;
                }
                if s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .is_ok()
                    && s.read_exact(&mut b).is_ok()
                {
                    let _ = s.write_all(&[marker]);
                }
            }
        });
        (port, thread)
    }
    fn request(port: u16, host: &str) -> Option<u8> {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        write!(s, "CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n").unwrap();
        let mut header = Vec::new();
        let mut b = [0];
        while header.len() < 4096 && !header.ends_with(b"\r\n\r\n") {
            if s.read_exact(&mut b).is_err() {
                return None;
            }
            header.push(b[0]);
        }
        if !String::from_utf8_lossy(&header).contains("200") {
            return None;
        }
        s.write_all(b"x").ok()?;
        s.read_exact(&mut b).ok()?;
        Some(b[0])
    }
    struct Creds;
    impl config::CredentialSource for Creds {
        fn get(&self, _: &str) -> Option<(String, String)> {
            None
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (xxyun, xthread) = proxy(b'X', stop.clone());
    let (clash, cthread) = proxy(b'C', stop.clone());
    let root = std::env::temp_dir().join(format!("aec-upstream-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let m = CoreManager {
        data_dir: root.clone(),
        bin: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../runtime/bin/aec-mihomo.exe"),
        relay_bin: root.join("unused-relay.exe"),
        ports: CorePorts {
            mixed: 46893,
            controller: 46909,
            dns: 46553,
            relay: 46325,
            tunnel: 46326,
        },
    };
    let result = std::panic::catch_unwind(|| {
        let mut set = configured();
        set.agent[0].routing.outbounds[0].port = xxyun;
        let save=serde_json::from_value(serde_json::json!({"action":"save","name":"Clash","host":"127.0.0.1","port":clash,"protocol":"http"})).unwrap();
        set = upstream::plan(&set, &save, m.ports).unwrap();
        m.up(&WindowsHost, |ports, secret| {
            config::from_profile(&set.agent[0], ports, secret, &Creds).map_err(|e| e.to_string())
        })
        .unwrap();
        m.apply_selections(&WindowsHost, &set.agent[0].routing)
            .unwrap();
        assert_eq!(request(m.ports.mixed, "chatgpt.com"), Some(b'X'));
        assert_eq!(request(m.ports.mixed, "claude.ai"), None);
        set = upstream::plan(
            &set,
            &upstream::Command::Select {
                name: "Clash".into(),
            },
            m.ports,
        )
        .unwrap();
        m.reload(&WindowsHost, &set.agent[0], &Creds).unwrap();
        assert_eq!(request(m.ports.mixed, "chatgpt.com"), Some(b'C'));
        assert_eq!(request(m.ports.mixed, "claude.ai"), None);
        // Updating the endpoint works through the same saved profile and core reload.
        let edit = upstream::Command::Save {
            name: "Clash".into(),
            host: "127.0.0.1".into(),
            port: 9,
            protocol: UpstreamProto::Http,
        };
        set = upstream::plan(&set, &edit, m.ports).unwrap();
        m.reload(&WindowsHost, &set.agent[0], &Creds).unwrap();
        assert_eq!(
            request(m.ports.mixed, "chatgpt.com"),
            None,
            "unavailable selected proxy must not silently fall back"
        );
        assert_eq!(request(m.ports.mixed, "claude.ai"), None);
        set = upstream::plan(
            &set,
            &upstream::Command::Select {
                name: "xxyun".into(),
            },
            m.ports,
        )
        .unwrap();
        m.reload(&WindowsHost, &set.agent[0], &Creds).unwrap();
        assert_eq!(request(m.ports.mixed, "chatgpt.com"), Some(b'X'));
        assert_eq!(request(m.ports.mixed, "claude.ai"), None);
    });
    let down = m.down(&WindowsHost);
    stop.store(true, Ordering::Relaxed);
    xthread.join().unwrap();
    cthread.join().unwrap();
    if down.is_ok() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    down.unwrap();
    if let Err(e) = result {
        std::panic::resume_unwind(e)
    }
}
