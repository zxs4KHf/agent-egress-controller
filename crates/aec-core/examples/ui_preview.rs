//! Isolated presentation QA: no real proxy, credentials, processes or global state.
use aec_core::{
    dashboard,
    profile::{Candidate, FieldError, ProfileSet, RoutingRule, RoutingRuleKind, UpstreamProto},
    setup::{self, ExitForm, SetupBackend, SetupView, ViewInput},
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
struct Preview {
    environment_data: std::path::PathBuf,
    environment_host: MemoryBrowserHost,
    environment_serial: Mutex<()>,
    profiles: Mutex<ProfileSet>,
    enabled: AtomicBool,
    active: AtomicBool,
    cooperation_enabled: AtomicBool,
}
#[derive(Default)]
struct MemoryBrowserHost(
    Mutex<
        std::collections::BTreeMap<
            (aec_core::browser_environment::Policy, bool),
            aec_core::browser_environment::Value,
        >,
    >,
);
impl aec_core::browser_environment::Host for MemoryBrowserHost {
    fn read(
        &self,
        policy: aec_core::browser_environment::Policy,
        machine: bool,
    ) -> Result<Option<aec_core::browser_environment::Value>, &'static str> {
        Ok(self.0.lock().unwrap().get(&(policy, machine)).cloned())
    }
    fn write(
        &self,
        policy: aec_core::browser_environment::Policy,
        value: Option<&aec_core::browser_environment::Value>,
    ) -> Result<(), &'static str> {
        let mut map = self.0.lock().unwrap();
        if let Some(value) = value {
            map.insert((policy, false), value.clone());
        } else {
            map.remove(&(policy, false));
        }
        Ok(())
    }
}
impl Drop for Preview {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.environment_data);
    }
}
impl SetupBackend for Preview {
    fn cooperation_command(
        &self,
        command: aec_core::cooperation::Command,
    ) -> Result<serde_json::Value, Vec<FieldError>> {
        let _serial = self.environment_serial.lock().unwrap();
        let selection = aec_core::cooperation::Selection {
            domains: self.profiles.lock().unwrap().agent[0]
                .targets
                .iter()
                .map(|t| t.domain.clone())
                .collect(),
            fixed_port: 17328,
        };
        aec_core::cooperation::Manager::new(
            &self.environment_data,
            &self.environment_data.join("synthetic-client"),
        )
        .execute(command, &selection)
        .map_err(|code| {
            vec![FieldError {
                path: "cooperation".into(),
                message: code.into(),
            }]
        })
    }
    fn browser_environment(
        &self,
        command: aec_core::browser_environment::Command,
        local_confirmation: bool,
    ) -> Result<serde_json::Value, Vec<FieldError>> {
        let _serial = self.environment_serial.lock().unwrap();
        let nonce = aec_core::crypto::random_hex(16).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        aec_core::browser_environment::execute(
            &self.environment_data,
            &self.environment_host,
            command,
            now,
            &nonce,
            local_confirmation,
        )
        .map_err(|code| {
            vec![FieldError {
                path: "browserEnvironment".into(),
                message: code.into(),
            }]
        })
    }
    fn upstream(
        &self,
        command: aec_core::local_upstream::Command,
    ) -> Result<serde_json::Value, Vec<FieldError>> {
        let mut set = self.profiles.lock().unwrap();
        if let aec_core::local_upstream::Command::Probe { .. } = command {
            return Ok(
                serde_json::json!({"probe":{"status":"unknown","code":"PREVIEW_NO_NETWORK","message":"隔离预览不访问外部网络"}}),
            );
        }
        *set = aec_core::local_upstream::plan(&set, &command, aec_core::core::CorePorts::default())
            .map_err(|message| {
                vec![FieldError {
                    path: "upstream".into(),
                    message,
                }]
            })?;
        Ok(serde_json::json!({"applied":true,"upstreams":aec_core::local_upstream::view(&set)}))
    }
    fn route(&self, form: aec_core::route_switch::Form) -> Result<(), Vec<FieldError>> {
        let mut set = self.profiles.lock().unwrap();
        *set = aec_core::route_switch::plan(&set, &form)
            .map_err(|message| {
                vec![FieldError {
                    path: "route".into(),
                    message,
                }]
            })?
            .profiles;
        Ok(())
    }
    fn view(&self) -> SetupView {
        setup::view(
            &self.profiles.lock().unwrap(),
            &ViewInput {
                core_mixed_port: None,
                stored_user: None,
                tunnel_password_stored: false,
                takeover_enabled: false,
                client_cooperation: self.cooperation_enabled.load(Ordering::Relaxed),
                locked: false,
            },
        )
    }
    fn save(&self, form: ExitForm) -> Result<(), Vec<FieldError>> {
        let mut profiles = self.profiles.lock().unwrap();
        let settings = setup::apply_settings(
            &form,
            aec_core::controller::Settings {
                client_cooperation: self.cooperation_enabled.load(Ordering::Relaxed),
                ..Default::default()
            },
        )?;
        setup::apply(&mut profiles, &form, None, false)?;
        self.cooperation_enabled
            .store(settings.client_cooperation, Ordering::Relaxed);
        Ok(())
    }
    fn dashboard(&self) -> Option<dashboard::View> {
        let mut view = dashboard::view(
            &self.profiles.lock().unwrap(),
            if self.active.load(Ordering::Relaxed) {
                "agent_unverified"
            } else {
                "daily"
            },
            false,
            false,
            Some("隔离界面演示，未运行真实网络服务".into()),
        );
        view.client_cooperation = self.cooperation_enabled.load(Ordering::Relaxed);
        Some(view)
    }
    fn agent_info(&self) -> serde_json::Value {
        serde_json::json!({"available":true,"enabled":self.enabled.load(Ordering::Relaxed),"mcpConfig":{"mcpServers":{"agent-egress-controller":{"command":"C:\\Example\\aec-agent.exe","args":["mcp"]}}}})
    }
    fn agent_access(&self, enabled: bool) -> Result<(), Vec<FieldError>> {
        self.enabled.store(enabled, Ordering::Relaxed);
        Ok(())
    }
    fn action(&self, action: &str) -> Result<(), Vec<FieldError>> {
        if action == "enter" {
            self.active.store(true, Ordering::Relaxed);
        }
        if action == "exit" {
            self.active.store(false, Ordering::Relaxed);
        }
        Ok(())
    }
    fn path(&self, path: String) -> Result<(), Vec<FieldError>> {
        self.profiles.lock().unwrap().agent[0]
            .egress
            .connection_path = Some(path);
        Ok(())
    }
    fn rules(&self, form: dashboard::RulesForm) -> Result<(), Vec<FieldError>> {
        dashboard::apply(&mut self.profiles.lock().unwrap(), form)
    }
    fn startup(&self, _enabled: bool) -> Result<(), Vec<FieldError>> {
        Ok(())
    }
    fn shortcuts(&self) -> Result<(), Vec<FieldError>> {
        Ok(())
    }
    fn update(&self, _ui_only: bool) -> Result<(), Vec<FieldError>> {
        Ok(())
    }
}
fn main() {
    let mut profiles = ProfileSet::default();
    let agent = &mut profiles.agent[0];
    agent.egress.candidates.push(Candidate {
        id: "preview".into(),
        host: "192.0.2.1".into(),
        port: 1080,
        proto: UpstreamProto::Socks5,
        credential_ref: None,
    });
    agent.routing.rules = (0..75)
        .map(|i| RoutingRule {
            kind: RoutingRuleKind::DomainSuffix,
            value: format!("example{i}.test"),
            target: match i % 3 {
                0 => "relay",
                1 => "DIRECT",
                _ => "REJECT",
            }
            .into(),
            no_resolve: false,
            note: None,
        })
        .collect();
    agent.egress.fixed_exit = true;
    agent.routing.outbounds.push(
        serde_json::from_value(
            serde_json::json!({"name":"xxyun", "kind":"http", "server":"127.0.0.1", "port":7892}),
        )
        .unwrap(),
    );
    agent.routing.final_target = "xxyun".into();
    agent.egress.expected.ip = Some("192.0.2.1".into());
    agent.routing.groups.push(serde_json::from_value(serde_json::json!({"name":"Preview GPT","type":"select","members":["relay","DIRECT"],"switchable":true})).unwrap());
    agent.routing.rules.push(serde_json::from_value(serde_json::json!({"kind":"domain_suffix","value":"chatgpt.com","target":"Preview GPT"})).unwrap());
    let environment_data = std::env::temp_dir().join(format!(
        "aec-ui-browser-env-{}",
        aec_core::crypto::random_hex(8).unwrap()
    ));
    std::fs::create_dir_all(&environment_data).unwrap();
    let client = environment_data.join("synthetic-client");
    std::fs::create_dir_all(client.join("profiles")).unwrap();
    std::fs::write(client.join("profiles.yaml"),"current: remote1\nitems:\n- uid: remote1\n  type: remote\n  option: {script: script1}\n- uid: script1\n  type: script\n  file: script1.js\n").unwrap();
    std::fs::write(client.join("verge.yaml"), "enable_dns_settings: false\n").unwrap();
    std::fs::write(client.join("profiles/script1.js"),"// Define main function (script entry)\n\nfunction main(config, profileName) {\n  return config;\n}\n").unwrap();
    let backend = Arc::new(Preview {
        environment_data,
        environment_host: MemoryBrowserHost::default(),
        environment_serial: Mutex::new(()),
        profiles: Mutex::new(profiles),
        enabled: AtomicBool::new(false),
        active: AtomicBool::new(false),
        cooperation_enabled: AtomicBool::new(false),
    });
    let server = setup::SetupServer::start(0, "preview".into(), backend).unwrap();
    println!("{}", server.control_tab_url("dashboard"));
    loop {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim() == "quit" {
            break;
        }
    }
}
