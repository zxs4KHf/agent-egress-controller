use aec_core::browser_environment::{self as env, Command, Host, Options, Policy, Scope, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Default)]
struct FakeBrowserHost(Mutex<BTreeMap<(Policy, bool), Value>>);
impl Host for FakeBrowserHost {
    fn read(&self, policy: Policy, machine: bool) -> Result<Option<Value>, &'static str> {
        Ok(self.0.lock().unwrap().get(&(policy, machine)).cloned())
    }
    fn write(&self, policy: Policy, value: Option<&Value>) -> Result<(), &'static str> {
        let mut values = self.0.lock().unwrap();
        match value {
            Some(value) => {
                values.insert((policy, false), value.clone());
            }
            None => {
                values.remove(&(policy, false));
            }
        }
        Ok(())
    }
}
struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "aec-browser-env-{}-{}",
            std::process::id(),
            aec_core::crypto::random_hex(8).unwrap()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn ai_only_preview_needs_confirmation_and_never_changes_browser_policies() {
    let temp = Temp::new();
    let host = FakeBrowserHost::default();
    let options = Options {
        scope: Scope::AiOnly,
        language: "zh-CN".into(),
        ..Options::default()
    };
    let plan = env::execute(
        &temp.0,
        &host,
        Command::Preview { options },
        1000,
        "plan-a",
        false,
    )
    .unwrap();
    assert_eq!(plan["scope"], "ai_only");
    assert_eq!(plan["runtimeVerified"], false);
    assert!(
        env::launch_flags(&temp.0).unwrap().is_empty(),
        "preview cannot apply browser flags"
    );
    assert!(
        env::execute(
            &temp.0,
            &host,
            Command::Apply {
                plan_id: "plan-a".into()
            },
            1001,
            "unused",
            false
        )
        .is_err(),
        "Agent cannot approve global or AI browser changes"
    );
    env::execute(
        &temp.0,
        &host,
        Command::Apply {
            plan_id: "plan-a".into(),
        },
        1001,
        "unused",
        true,
    )
    .unwrap();
    let flags = env::launch_flags(&temp.0).unwrap();
    assert!(flags.contains(&"--lang=zh-CN".into()));
    assert!(flags.contains(&"--webrtc-ip-handling-policy=disable_non_proxied_udp".into()));
    assert!(flags.contains(&"--disable-quic".into()));
    assert!(flags
        .iter()
        .any(|flag| flag.starts_with("--host-resolver-rules=MAP * ~NOTFOUND")));
    assert!(host.0.lock().unwrap().is_empty());
    env::execute(&temp.0, &host, Command::Restore, 1002, "unused", true).unwrap();
    assert!(env::launch_flags(&temp.0).unwrap().is_empty());
}

#[test]
fn global_preview_lists_exact_edge_changes_and_restores_original_chinese_locale() {
    let temp = Temp::new();
    let host = FakeBrowserHost::default();
    host.write(
        Policy::ApplicationLocaleValue,
        Some(&Value::Text("zh-CN".into())),
    )
    .unwrap();
    let before = host.0.lock().unwrap().clone();
    let options = Options {
        scope: Scope::Global,
        ..Options::default()
    };
    let plan = env::execute(
        &temp.0,
        &host,
        Command::Preview { options },
        1000,
        "global-a",
        false,
    )
    .unwrap();
    assert_eq!(
        *host.0.lock().unwrap(),
        before,
        "preview must not change real settings"
    );
    assert_eq!(plan["changes"].as_array().unwrap().len(), 5);
    assert!(plan["excludedBrowsers"].to_string().contains("Chrome"));
    assert!(
        plan["affectedBrowsers"].to_string().contains("AEC"),
        "global scope also changes future AEC-owned launch flags, which must be disclosed"
    );
    let locale = plan["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["policy"] == "ApplicationLocaleValue")
        .unwrap();
    assert_eq!(locale["before"], serde_json::json!({"text":"zh-CN"}));
    assert_eq!(locale["after"], serde_json::json!({"text":"en-US"}));
    env::execute(
        &temp.0,
        &host,
        Command::Apply {
            plan_id: "global-a".into(),
        },
        1001,
        "unused",
        true,
    )
    .unwrap();
    assert_eq!(
        host.read(Policy::QuicAllowed, false).unwrap(),
        Some(Value::Dword(0))
    );
    assert_eq!(
        host.read(Policy::NetworkPredictionOptions, false).unwrap(),
        Some(Value::Dword(2))
    );
    env::execute(&temp.0, &host, Command::Restore, 1002, "unused", true).unwrap();
    assert_eq!(
        *host.0.lock().unwrap(),
        before,
        "restore must preserve absent values, not replace them with defaults"
    );
}

#[test]
fn incomplete_environment_commit_cannot_silently_launch_an_unguarded_ai_browser() {
    let temp = Temp::new();
    let host = FakeBrowserHost::default();
    env::execute(
        &temp.0,
        &host,
        Command::Preview {
            options: Options::default(),
        },
        1,
        "commit",
        false,
    )
    .unwrap();
    env::execute(
        &temp.0,
        &host,
        Command::Apply {
            plan_id: "commit".into(),
        },
        2,
        "unused",
        true,
    )
    .unwrap();
    std::fs::remove_file(temp.0.join("browser-environment.json")).unwrap();
    assert_eq!(
        env::launch_flags(&temp.0).unwrap_err(),
        "BROWSER_RECOVERY_REQUIRED"
    );
    env::execute(&temp.0, &host, Command::Restore, 3, "unused", true).unwrap();
    assert!(env::launch_flags(&temp.0).unwrap().is_empty());
}

#[test]
fn confirmed_ai_environment_reaches_the_real_launcher_without_duplicate_language_or_profile_override(
) {
    let temp = Temp::new();
    let host = FakeBrowserHost::default();
    env::execute(
        &temp.0,
        &host,
        Command::Preview {
            options: Options {
                language: "zh-CN".into(),
                ..Options::default()
            },
        },
        1,
        "launch",
        false,
    )
    .unwrap();
    env::execute(
        &temp.0,
        &host,
        Command::Apply {
            plan_id: "launch".into(),
        },
        2,
        "",
        true,
    )
    .unwrap();
    let profile = aec_core::profile::ProfileSet::default().agent.remove(0);
    let args = aec_core::entries::environment_browser_args(
        &profile,
        &temp.0,
        aec_core::profile::Browser::Edge,
        17893,
        &[],
        None,
    )
    .unwrap();
    assert_eq!(
        args.iter()
            .filter(|arg| arg.starts_with("--lang="))
            .collect::<Vec<_>>(),
        vec!["--lang=zh-CN"]
    );
    assert!(args.contains(&"--disable-quic".into()));
    for extra in [
        "--user-data-dir=C:/fingerprint",
        "--proxy-server=direct://",
        "--host-resolver-rules=MAP * 127.0.0.1",
        "--enable-quic",
    ] {
        assert!(aec_core::entries::environment_browser_args(
            &profile,
            &temp.0,
            aec_core::profile::Browser::Edge,
            17893,
            &[extra.into()],
            None
        )
        .is_err());
    }
}

struct Panel {
    temp: Temp,
    host: FakeBrowserHost,
    serial: Mutex<()>,
}

#[test]
fn stale_preview_and_external_browser_policy_changes_are_never_overwritten() {
    let temp = Temp::new();
    let host = FakeBrowserHost::default();
    let options = Options {
        scope: Scope::Global,
        ..Options::default()
    };
    env::execute(
        &temp.0,
        &host,
        Command::Preview {
            options: options.clone(),
        },
        1,
        "old",
        false,
    )
    .unwrap();
    env::execute(
        &temp.0,
        &host,
        Command::Preview { options },
        2,
        "new",
        false,
    )
    .unwrap();
    assert!(env::execute(
        &temp.0,
        &host,
        Command::Apply {
            plan_id: "old".into()
        },
        3,
        "",
        true
    )
    .is_err());
    host.write(Policy::QuicAllowed, Some(&Value::Dword(1)))
        .unwrap();
    let changed = host.0.lock().unwrap().clone();
    assert_eq!(
        env::execute(
            &temp.0,
            &host,
            Command::Apply {
                plan_id: "new".into()
            },
            3,
            "",
            true
        )
        .unwrap_err(),
        "BROWSER_POLICY_CONFLICT"
    );
    assert_eq!(*host.0.lock().unwrap(), changed);
    assert_eq!(
        env::execute(&temp.0, &host, Command::State, 3, "", false).unwrap()["canRestore"],
        false
    );
    host.0.lock().unwrap().clear();
    env::execute(
        &temp.0,
        &host,
        Command::Apply {
            plan_id: "new".into(),
        },
        4,
        "",
        true,
    )
    .unwrap();
    host.write(
        Policy::ApplicationLocaleValue,
        Some(&Value::Text("ja-JP".into())),
    )
    .unwrap();
    let changed = host.0.lock().unwrap().clone();
    assert_eq!(
        env::execute(&temp.0, &host, Command::Restore, 5, "", true).unwrap_err(),
        "BROWSER_RESTORE_CONFLICT"
    );
    assert_eq!(*host.0.lock().unwrap(), changed);
    assert_eq!(
        env::execute(&temp.0, &host, Command::State, 5, "", false).unwrap()["canRestore"],
        true
    );
}

#[test]
fn machine_policy_blocks_global_plan_and_partial_write_failure_recovers_original_values() {
    struct FailingHost {
        inner: FakeBrowserHost,
        fail: std::sync::atomic::AtomicBool,
    }
    impl Host for FailingHost {
        fn read(&self, policy: Policy, machine: bool) -> Result<Option<Value>, &'static str> {
            self.inner.read(policy, machine)
        }
        fn write(&self, policy: Policy, value: Option<&Value>) -> Result<(), &'static str> {
            if policy == Policy::NetworkPredictionOptions
                && self.fail.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err("BROWSER_POLICY_WRITE_FAILED");
            }
            self.inner.write(policy, value)
        }
    }
    let temp = Temp::new();
    let host = FailingHost {
        inner: FakeBrowserHost::default(),
        fail: std::sync::atomic::AtomicBool::new(true),
    };
    let options = Options {
        scope: Scope::Global,
        ..Options::default()
    };
    host.inner
        .0
        .lock()
        .unwrap()
        .insert((Policy::QuicAllowed, true), Value::Dword(1));
    assert_eq!(
        env::execute(
            &temp.0,
            &host,
            Command::Preview {
                options: options.clone()
            },
            1,
            "machine",
            false
        )
        .unwrap_err(),
        "BROWSER_MACHINE_POLICY_CONFLICT"
    );
    host.inner.0.lock().unwrap().clear();
    host.inner
        .write(
            Policy::ApplicationLocaleValue,
            Some(&Value::Text("zh-CN".into())),
        )
        .unwrap();
    let before = host.inner.0.lock().unwrap().clone();
    env::execute(
        &temp.0,
        &host,
        Command::Preview { options },
        1,
        "failure",
        false,
    )
    .unwrap();
    assert_eq!(
        env::execute(
            &temp.0,
            &host,
            Command::Apply {
                plan_id: "failure".into()
            },
            2,
            "",
            true
        )
        .unwrap_err(),
        "BROWSER_POLICY_WRITE_FAILED"
    );
    assert_eq!(*host.inner.0.lock().unwrap(), before);
    let state = env::execute(&temp.0, &host, Command::State, 3, "", false).unwrap();
    assert_eq!(state["canRestore"], false);
    assert!(env::launch_flags(&temp.0).unwrap().is_empty());
}

#[test]
fn agent_can_read_a_durable_browser_failure_reason_after_the_dialog_is_closed() {
    let temp = Temp::new();
    let host = FakeBrowserHost::default();
    assert!(env::execute(
        &temp.0,
        &host,
        Command::Apply {
            plan_id: "missing".into()
        },
        42,
        "",
        true
    )
    .is_err());
    let state = env::execute(&temp.0, &host, Command::State, 43, "", false).unwrap();
    assert_eq!(state["recentError"]["code"], "BROWSER_PREVIEW_REQUIRED");
    assert_eq!(state["recentError"]["recordedAtUnixMs"], 42);
    assert_eq!(state["recentError"]["action"], "apply");
}
impl aec_core::setup::SetupBackend for Panel {
    fn view(&self) -> aec_core::setup::SetupView {
        panic!("not used")
    }
    fn save(&self, _: aec_core::setup::ExitForm) -> Result<(), Vec<aec_core::profile::FieldError>> {
        panic!("not used")
    }
    fn browser_environment(
        &self,
        command: Command,
        local_confirmation: bool,
    ) -> Result<serde_json::Value, Vec<aec_core::profile::FieldError>> {
        let _serial = self.serial.lock().unwrap();
        env::execute(
            &self.temp.0,
            &self.host,
            command,
            100,
            "http-plan",
            local_confirmation,
        )
        .map_err(|code| {
            vec![aec_core::profile::FieldError {
                path: "browserEnvironment".into(),
                message: code.into(),
            }]
        })
    }
}
#[test]
fn panel_confirmation_uses_authenticated_origin_and_agent_cannot_apply_a_plan() {
    use std::io::{Read, Write};
    let backend = std::sync::Arc::new(Panel {
        temp: Temp::new(),
        host: FakeBrowserHost::default(),
        serial: Mutex::new(()),
    });
    let server =
        aec_core::setup::SetupServer::start(0, "browser-test".into(), backend.clone()).unwrap();
    let send = |token: &str, origin: bool, body: &str| {
        let port = server.port();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let origin = if origin {
            format!("Origin: http://127.0.0.1:{port}\r\n")
        } else {
            String::new()
        };
        write!(stream,"POST /browser-environment?t={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{origin}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    };
    let body = serde_json::json!({"action":"preview","options":Options::default()}).to_string();
    assert!(send("wrong", true, &body).starts_with("HTTP/1.1 403"));
    assert!(send("browser-test", false, &body).starts_with("HTTP/1.1 403"));
    assert!(env::launch_flags(&backend.temp.0).unwrap().is_empty());
    let reply = send("browser-test", true, &body);
    assert!(reply.contains("\"planId\":\"http-plan\""), "{reply}");
    let call = |name: &str, arguments| {
        aec_core::agent::call(
            backend.as_ref(),
            aec_core::agent::Call {
                name: name.into(),
                arguments,
            },
        )
    };
    let state = call("aec_browser_environment", serde_json::json!({})).unwrap();
    assert_eq!(state["canRestore"], false);
    assert!(call(
        "aec_browser_environment_apply",
        serde_json::json!({"planId":"http-plan"})
    )
    .is_err());
    assert!(call("aec_browser_environment_preview", serde_json::json!({"scope":"ai_only","language":"en-US","preventWebrtc":true,"disableQuic":true,"remoteDnsOnly":true,"confirm":true})).is_err());
    assert!(send(
        "browser-test",
        true,
        r#"{"action":"apply","planId":"http-plan","force":true}"#
    )
    .contains("\"ok\":false"));
    assert!(send(
        "browser-test",
        true,
        r#"{"action":"apply","planId":"http-plan"}"#
    )
    .contains("\"ok\":true"));
    assert!(!env::launch_flags(&backend.temp.0).unwrap().is_empty());
    let result = call("aec_browser_environment", serde_json::json!({})).unwrap();
    assert_eq!(result["canRestore"], true);
    assert_eq!(result["ednsEcs"], "unknown");
}
