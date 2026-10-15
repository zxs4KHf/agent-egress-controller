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
    profiles: Mutex<ProfileSet>,
    enabled: AtomicBool,
    active: AtomicBool,
}
impl SetupBackend for Preview {
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
                locked: false,
            },
        )
    }
    fn save(&self, _form: ExitForm) -> Result<(), Vec<FieldError>> {
        Ok(())
    }
    fn dashboard(&self) -> Option<dashboard::View> {
        Some(dashboard::view(
            &self.profiles.lock().unwrap(),
            if self.active.load(Ordering::Relaxed) {
                "agent_unverified"
            } else {
                "daily"
            },
            false,
            false,
            Some("隔离界面演示，未运行真实网络服务".into()),
        ))
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
    agent.egress.expected.ip = Some("192.0.2.1".into());
    agent.routing.groups.push(serde_json::from_value(serde_json::json!({"name":"Preview GPT","type":"select","members":["relay","DIRECT"],"switchable":true})).unwrap());
    agent.routing.rules.push(serde_json::from_value(serde_json::json!({"kind":"domain_suffix","value":"chatgpt.com","target":"Preview GPT"})).unwrap());
    let backend = Arc::new(Preview {
        profiles: Mutex::new(profiles),
        enabled: AtomicBool::new(false),
        active: AtomicBool::new(false),
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
