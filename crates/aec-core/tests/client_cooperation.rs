#![cfg(windows)]
use aec_core::cooperation::{Command, Manager, Selection};
use serde_json::json;
struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> (Temp, std::path::PathBuf, std::path::PathBuf) {
    let dir = Temp(std::env::temp_dir().join(format!(
        "aec-coop-{}",
        aec_core::crypto::random_hex(12).unwrap()
    )));
    let root = dir.0.join("client");
    let data = dir.0.join("aec");
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(root.join("profiles.yaml"), "current: remote1\nitems:\n- uid: remote1\n  type: remote\n  option:\n    script: script1\n- uid: script1\n  type: script\n  file: script1.js\n").unwrap();
    std::fs::write(root.join("verge.yaml"), "enable_dns_settings: false\n").unwrap();
    std::fs::write(
        root.join("profiles/script1.js"),
        "function main(config) {\n  return config;\n}\n",
    )
    .unwrap();
    (dir, root, data)
}
fn selection() -> Selection {
    Selection {
        domains: vec!["claude.ai".into(), "anthropic.com".into()],
        fixed_port: 17328,
    }
}

#[test]
fn registered_subscription_install_is_previewed_persistent_and_restorable_without_reload() {
    let (_dir, root, data) = fixture();
    let manager = Manager::new(&data, &root);
    let original = std::fs::read(root.join("profiles/script1.js")).unwrap();
    let registration = std::fs::read(root.join("profiles.yaml")).unwrap();
    let preview = manager.execute(Command::Preview, &selection()).unwrap();
    assert_eq!(preview["activation"], "manual_client_reload");
    assert_eq!(
        std::fs::read(root.join("profiles/script1.js")).unwrap(),
        original
    );
    let installed = manager
        .execute(
            Command::Install {
                plan_id: preview["planId"].as_str().unwrap().into(),
            },
            &selection(),
        )
        .unwrap();
    assert_eq!(installed["persistentInstalled"], true);
    assert_eq!(installed["runtimeVerified"], false);
    assert_eq!(
        std::fs::read(root.join("profiles.yaml")).unwrap(),
        registration
    );
    assert_eq!(
        manager.execute(Command::State, &selection()).unwrap()["persistentInstalled"],
        true
    );
    assert_eq!(
        manager.execute(Command::Restore, &selection()).unwrap()["persistentInstalled"],
        false
    );
    assert_eq!(
        std::fs::read(root.join("profiles/script1.js")).unwrap(),
        original
    );
    assert_eq!(
        json!(manager.execute(Command::State, &selection()).unwrap())["runtimeVerified"],
        false
    );
}

#[test]
fn official_default_template_installs_but_global_script_registration_is_refused() {
    let (_dir, root, data) = fixture();
    let official = "// Define main function (script entry)\r\n\r\nfunction main(config, profileName) {\r\n  return config;\r\n}\r\n";
    std::fs::write(root.join("profiles/script1.js"), official).unwrap();
    assert!(Manager::new(&data, &root)
        .execute(Command::Preview, &selection())
        .is_ok());
    std::fs::write(root.join("profiles.yaml"), "current: remote1\nitems:\n- uid: remote1\n  type: remote\n  option: {script: Script}\n- uid: Script\n  type: script\n  file: script1.js\n").unwrap();
    assert!(Manager::new(&data, &root)
        .execute(Command::Preview, &selection())
        .is_err());
}

#[test]
fn stale_preview_and_foreign_script_edits_are_never_overwritten_and_errors_are_agent_readable() {
    let (_dir, root, data) = fixture();
    let manager = Manager::new(&data, &root);
    let preview = manager.execute(Command::Preview, &selection()).unwrap();
    std::fs::write(
        root.join("profiles/script1.js"),
        "function main(config) { config.private = true; return config; }\n",
    )
    .unwrap();
    assert_eq!(
        manager
            .execute(
                Command::Install {
                    plan_id: preview["planId"].as_str().unwrap().into()
                },
                &selection()
            )
            .unwrap_err(),
        "COOP_PREVIEW_STALE"
    );
    assert_eq!(
        manager.execute(Command::State, &selection()).unwrap()["recentError"]["code"],
        "COOP_PREVIEW_STALE"
    );
    assert_eq!(
        manager.execute(Command::Preview, &selection()).unwrap_err(),
        "COOP_CUSTOM_SCRIPT_MANUAL_MERGE"
    );
    std::fs::write(
        root.join("profiles/script1.js"),
        "function main(config) { return config; }",
    )
    .unwrap();
    let preview = manager.execute(Command::Preview, &selection()).unwrap();
    manager
        .execute(
            Command::Install {
                plan_id: preview["planId"].as_str().unwrap().into(),
            },
            &selection(),
        )
        .unwrap();
    let owned = std::fs::read_to_string(root.join("profiles/script1.js")).unwrap();
    let edited = format!("{owned}// external edit\n");
    std::fs::write(root.join("profiles/script1.js"), &edited).unwrap();
    assert_eq!(
        manager.execute(Command::State, &selection()).unwrap()["persistentInstalled"],
        false
    );
    assert_eq!(
        manager.execute(Command::Restore, &selection()).unwrap_err(),
        "COOP_EXTERNAL_CHANGE"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("profiles/script1.js")).unwrap(),
        edited
    );
    assert_eq!(
        manager.execute(Command::State, &selection()).unwrap()["canRestore"],
        true
    );
}

#[test]
fn shared_scripts_gui_dns_override_and_path_escape_are_refused_without_private_backups() {
    let (_dir, root, data) = fixture();
    let manager = Manager::new(&data, &root);
    std::fs::write(root.join("verge.yaml"), "enable_dns_settings: true\n").unwrap();
    assert_eq!(
        manager.execute(Command::Preview, &selection()).unwrap_err(),
        "COOP_GUI_DNS_OVERRIDE"
    );
    std::fs::write(
        root.join("verge.yaml"),
        "enable_dns_settings: false\nprofile_dns_settings:\n  remote1:\n    enabled: true\n",
    )
    .unwrap();
    assert_eq!(
        manager.execute(Command::Preview, &selection()).unwrap_err(),
        "COOP_GUI_DNS_OVERRIDE"
    );
    std::fs::write(root.join("verge.yaml"), "enable_dns_settings: false\n").unwrap();
    let original = std::fs::read_to_string(root.join("profiles.yaml")).unwrap();
    std::fs::write(
        root.join("profiles.yaml"),
        format!("{original}- uid: remote2\n  type: remote\n  option: {{script: script1}}\n"),
    )
    .unwrap();
    assert_eq!(
        manager.execute(Command::Preview, &selection()).unwrap_err(),
        "COOP_SCRIPT_SHARED"
    );
    std::fs::write(
        root.join("profiles.yaml"),
        original.replace("file: script1.js", "file: ../private.js"),
    )
    .unwrap();
    assert_eq!(
        manager.execute(Command::Preview, &selection()).unwrap_err(),
        "COOP_FILE_UNSAFE"
    );
    assert!(!data.join("cooperation-owned.json").exists());
    assert_eq!(
        manager
            .execute(Command::DnsProbe { consent: false }, &selection())
            .unwrap_err(),
        "COOP_DNS_CONSENT_REQUIRED"
    );
}

#[test]
fn verified_client_defaults_have_dns_override_off_but_malformed_values_are_not_guessed() {
    let (_dir, root, data) = fixture();
    let manager = Manager::new(&data, &root);
    std::fs::write(root.join("verge.yaml"), "enable_dns_settings: null\n").unwrap();
    assert!(manager.execute(Command::Preview, &selection()).is_ok());
    std::fs::write(root.join("verge.yaml"), "enable_dns_settings: 'false'\n").unwrap();
    assert_eq!(
        manager.execute(Command::Preview, &selection()).unwrap_err(),
        "COOP_DNS_OVERRIDE_UNKNOWN"
    );
}

#[test]
fn invalid_or_asi_sensitive_javascript_is_not_mistaken_for_the_default_identity_template() {
    let (_dir, root, data) = fixture();
    let manager = Manager::new(&data, &root);
    for script in [
        "functionmain(config){returnconfig;}",
        "function main(config) { return\nconfig; }",
        "// Define main function (script entry) function main(config) { return config; }",
    ] {
        std::fs::write(root.join("profiles/script1.js"), script).unwrap();
        assert_eq!(
            manager.execute(Command::Preview, &selection()).unwrap_err(),
            "COOP_CUSTOM_SCRIPT_MANUAL_MERGE"
        );
    }
}

struct Panel {
    _temp: Temp,
    root: std::path::PathBuf,
    data: std::path::PathBuf,
    serial: std::sync::Mutex<()>,
}
impl aec_core::setup::SetupBackend for Panel {
    fn view(&self) -> aec_core::setup::SetupView {
        panic!("not used")
    }
    fn save(&self, _: aec_core::setup::ExitForm) -> Result<(), Vec<aec_core::profile::FieldError>> {
        panic!("not used")
    }
    fn cooperation_command(
        &self,
        command: Command,
    ) -> Result<serde_json::Value, Vec<aec_core::profile::FieldError>> {
        let _serial = self.serial.lock().unwrap();
        Manager::new(&self.data, &self.root)
            .execute(command, &selection())
            .map_err(|code| {
                vec![aec_core::profile::FieldError {
                    path: "cooperation".into(),
                    message: code.into(),
                }]
            })
    }
}
#[test]
fn panel_and_agent_use_the_same_preview_install_and_failure_log_with_strict_authentication() {
    use std::io::{Read, Write};
    let (temp, root, data) = fixture();
    let backend = std::sync::Arc::new(Panel {
        _temp: temp,
        root,
        data,
        serial: std::sync::Mutex::new(()),
    });
    let server =
        aec_core::setup::SetupServer::start(0, "coop-test".into(), backend.clone()).unwrap();
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
        write!(stream,"POST /cooperation?t={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{origin}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    };
    assert!(send("wrong", true, r#"{"action":"preview"}"#).starts_with("HTTP/1.1 403"));
    assert!(send("coop-test", false, r#"{"action":"preview"}"#).starts_with("HTTP/1.1 403"));
    let call = |name: &str, arguments| {
        aec_core::agent::call(
            backend.as_ref(),
            aec_core::agent::Call {
                name: name.into(),
                arguments,
            },
        )
    };
    assert!(call("aec_cooperation_preview", json!({"force":true})).is_err());
    let preview = call("aec_cooperation_preview", json!({})).unwrap();
    let installed = send(
        "coop-test",
        true,
        &json!({"action":"install","planId":preview["planId"]}).to_string(),
    );
    assert!(installed.contains("\"persistentInstalled\":true"));
    assert_eq!(
        call("aec_cooperation_state", json!({})).unwrap()["runtimeVerified"],
        false
    );
    assert!(call("aec_cooperation_dns_probe", json!({"consent":false}))
        .unwrap_err()
        .contains("COOP_DNS_CONSENT_REQUIRED"));
    assert_eq!(
        call("aec_cooperation_state", json!({})).unwrap()["recentError"]["code"],
        "COOP_DNS_CONSENT_REQUIRED"
    );
    assert!(
        send("coop-test", true, r#"{"action":"restore","force":true}"#).contains("\"ok\":false")
    );
    assert_eq!(
        call("aec_cooperation_restore", json!({})).unwrap()["persistentInstalled"],
        false
    );
}
