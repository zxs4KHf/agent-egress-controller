//! Import a routing table, never the source's global settings, listeners, or executable paths.
//! Parsing and validation are pure. Credentials are committed only after the entire table passes.

use crate::profile::{
    self, ProfileSet, Routing, RoutingCheck, RoutingGroup, RoutingGroupKind, RoutingOutbound,
    RoutingOutboundKind, RoutingRule, RoutingRuleKind,
};
use crate::store::ProfileStore;
use serde::Deserialize;

#[derive(Deserialize)]
struct Source {
    #[serde(default)]
    proxies: Vec<Node>,
    #[serde(default, rename = "proxy-groups")]
    groups: Vec<Group>,
    rules: Vec<String>,
    #[serde(default, rename = "proxy-providers")]
    proxy_providers: Option<serde_yaml_ng::Value>,
    #[serde(default, rename = "rule-providers")]
    rule_providers: Option<serde_yaml_ng::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    name: String,
    #[serde(rename = "type")]
    kind: RoutingOutboundKind,
    server: String,
    port: u16,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    cipher: Option<String>,
    #[serde(default, rename = "dialer-proxy")]
    dialer_proxy: Option<String>,
    #[serde(default)]
    udp: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    name: String,
    #[serde(rename = "type")]
    kind: RoutingGroupKind,
    proxies: Vec<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    interval: Option<u32>,
    #[serde(default)]
    tolerance: u32,
}

struct Secret {
    reference: String,
    user: String,
    pass: String,
}

pub struct ImportPlan {
    pub routing: Routing,
    secrets: Vec<Secret>,
}

impl std::fmt::Debug for ImportPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportPlan")
            .field("routing", &self.routing)
            .field("credential_count", &self.secrets.len())
            .finish()
    }
}

pub trait CredentialSink {
    fn store(&self, reference: &str, user: &str, pass: &str) -> Result<(), String>;
    fn delete(&self, reference: &str) -> Result<(), String>;
}

#[cfg(windows)]
impl CredentialSink for crate::credentials::WindowsCredentials {
    fn store(&self, reference: &str, user: &str, pass: &str) -> Result<(), String> {
        self.store(reference, user, pass)
    }
    fn delete(&self, reference: &str) -> Result<(), String> {
        self.delete(reference).map(|_| ())
    }
}

/// Keep conventional block-list rule comments, including quoted values containing '#'. A flow
/// list has no per-rule comment association and is parsed normally without inventing notes.
fn rule_notes(yaml: &str, count: usize) -> Vec<Option<String>> {
    let mut notes = Vec::new();
    let mut pending = Vec::new();
    let mut active = false;
    for line in yaml.lines() {
        let t = line.trim();
        if !active {
            if t == "rules:" {
                active = true;
            }
            continue;
        }
        if !t.is_empty() && !line.starts_with(char::is_whitespace) && !t.starts_with(['-', '#']) {
            break;
        }
        if let Some(c) = t.strip_prefix('#') {
            pending.push(c.trim().to_string());
        } else if t.starts_with("- ") {
            // A comment begins only outside quotes and after whitespace.
            let (mut single, mut double, mut escape) = (false, false, false);
            let chars: Vec<_> = t.chars().collect();
            for (i, c) in chars.iter().copied().enumerate() {
                if escape {
                    escape = false;
                    continue;
                }
                if double && c == '\\' {
                    escape = true;
                    continue;
                }
                if c == '\'' && !double {
                    single = !single;
                }
                if c == '"' && !single {
                    double = !double;
                }
                if c == '#' && !single && !double && i > 0 && chars[i - 1].is_whitespace() {
                    pending.push(chars[i + 1..].iter().collect::<String>().trim().to_string());
                    break;
                }
            }
            notes.push(if pending.is_empty() {
                None
            } else {
                Some(pending.join("\n"))
            });
            pending.clear();
        }
    }
    if notes.len() == count {
        notes
    } else {
        vec![None; count]
    }
}

/// `namespace` must be fresh for every import, so rollback never deletes existing credentials.
/// Source-parser errors deliberately exclude YAML excerpts and scalar values (they may be secrets).
pub fn parse(yaml: &str, namespace: &str) -> Result<ImportPlan, String> {
    if yaml.len() > 1024 * 1024 {
        return Err("routing import exceeds 1 MiB".into());
    }
    let source: Source = serde_yaml_ng::from_str(yaml).map_err(|e| {
        e.location()
            .map_or("invalid or unsupported YAML shape".into(), |l| {
                format!(
                    "invalid or unsupported YAML at line {}, column {}; check supported fields",
                    l.line(),
                    l.column()
                )
            })
    })?;
    if source.proxy_providers.is_some() || source.rule_providers.is_some() {
        return Err("providers are unsupported; export a self-contained table first".into());
    }
    if source.groups.len() + source.proxies.len() > 512 || source.rules.len() > 10000 {
        return Err("routing table exceeds supported limits".into());
    }
    let mut outbounds = Vec::new();
    let mut secrets = Vec::new();
    for (i, node) in source.proxies.into_iter().enumerate() {
        if node.udp {
            return Err(format!(
                "proxies[{i}]: UDP is unsupported; export with udp: false"
            ));
        }
        if node.username.is_some() && node.password.is_none() {
            return Err(format!("proxies[{i}]: username requires password"));
        }
        if node.kind == RoutingOutboundKind::Shadowsocks && node.username.is_some() {
            return Err(format!("proxies[{i}]: SS does not use a username"));
        }
        let reference = node.password.map(|pass| {
            let reference = format!("import-{namespace}-{i}");
            secrets.push(Secret {
                reference: reference.clone(),
                user: node.username.unwrap_or_default(),
                pass,
            });
            reference
        });
        outbounds.push(RoutingOutbound {
            name: node.name,
            kind: node.kind,
            server: node.server,
            port: node.port,
            credential_ref: reference,
            cipher: node.cipher,
            dialer_proxy: node.dialer_proxy,
        });
    }
    let mut groups = Vec::new();
    for (i, group) in source.groups.into_iter().enumerate() {
        let check = match (group.url, group.interval) {
            (Some(url), Some(interval_sec)) => Some(RoutingCheck {
                url,
                interval_sec,
                tolerance_ms: group.tolerance,
            }),
            (None, None) if group.tolerance == 0 => None,
            _ => {
                return Err(format!(
                    "proxy-groups[{i}]: health check needs url and interval"
                ))
            }
        };
        groups.push(RoutingGroup {
            name: group.name,
            kind: group.kind,
            check,
            members: group.proxies,
            switchable: group.kind == RoutingGroupKind::Select,
            selected: None,
        });
    }
    let notes = rule_notes(yaml, source.rules.len());
    let mut rules = Vec::new();
    let mut final_target = None;
    for (i, (raw, note)) in source.rules.into_iter().zip(notes).enumerate() {
        if final_target.is_some() {
            return Err(format!("rules[{i}]: rule after MATCH would be unreachable"));
        }
        let fields: Vec<_> = raw.split(',').map(str::trim).collect();
        if fields.len() == 2 && fields[0] == "MATCH" {
            final_target = Some(fields[1].into());
            continue;
        }
        if !(fields.len() == 3 || fields.len() == 4 && fields[3] == "no-resolve") {
            return Err(format!("rules[{i}]: unsupported rule shape"));
        }
        let kind = match fields[0] {
            "DOMAIN" => RoutingRuleKind::Domain,
            "DOMAIN-SUFFIX" => RoutingRuleKind::DomainSuffix,
            "DOMAIN-KEYWORD" => RoutingRuleKind::DomainKeyword,
            "IP-CIDR" | "IP-CIDR6" => RoutingRuleKind::IpCidr,
            "GEOSITE" => RoutingRuleKind::Geosite,
            "GEOIP" => RoutingRuleKind::Geoip,
            _ => return Err(format!("rules[{i}]: unsupported rule type")),
        };
        rules.push(RoutingRule {
            kind,
            value: fields[1].into(),
            target: fields[2].into(),
            note,
            no_resolve: fields.len() == 4,
        });
    }
    let routing = Routing {
        outbounds,
        groups,
        rules,
        final_target: final_target.ok_or("a final MATCH rule is required")?,
    };
    if let Some(e) = profile::validate_routing(&routing).first() {
        return Err(format!("{}: {}", e.path, e.message));
    }
    Ok(ImportPlan { routing, secrets })
}

impl ImportPlan {
    /// Regenerate in memory and compare every source outbound and group plus the complete ordered
    /// rule list. The extra built-in relay is expected; source listeners and DNS are outside scope.
    pub fn verify(&self, yaml: &str) -> Result<(), String> {
        let generated = self.validation_config()?;
        let before: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(yaml).map_err(|_| "invalid YAML")?;
        let after: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&generated).map_err(|_| "generated YAML is invalid")?;
        for key in ["rules", "proxy-groups"] {
            if before[key] != after[key] {
                return Err(format!("{key} differs after import/render"));
            }
        }
        let source_nodes = before["proxies"]
            .as_sequence()
            .ok_or("source proxies must be a list")?;
        let rendered_nodes = after["proxies"]
            .as_sequence()
            .ok_or("generated proxies must be a list")?;
        if rendered_nodes.len() != source_nodes.len() + 1 {
            return Err("outbound name set differs".into());
        }
        for (i, node) in source_nodes.iter().enumerate() {
            let rendered = rendered_nodes
                .iter()
                .find(|n| n["name"] == node["name"])
                .ok_or("outbound name missing")?;
            for (key, value) in node.as_mapping().ok_or("invalid proxy shape")? {
                if rendered.get(key) != Some(value) {
                    return Err(format!("proxies[{i}]: field differs after render"));
                }
            }
        }
        Ok(())
    }

    /// Contains transient secrets: feed to mihomo validation only, never print or log this string.
    pub fn validation_config(&self) -> Result<String, String> {
        self.validation_config_on(&crate::core::CorePorts::default(), "validation-only")
    }

    pub fn validation_config_on(
        &self,
        ports: &crate::core::CorePorts,
        secret: &str,
    ) -> Result<String, String> {
        struct MemoryCredentials<'a>(&'a [Secret]);
        impl crate::core::config::CredentialSource for MemoryCredentials<'_> {
            fn get(&self, reference: &str) -> Option<(String, String)> {
                self.0
                    .iter()
                    .find(|s| s.reference == reference)
                    .map(|s| (s.user.clone(), s.pass.clone()))
            }
        }
        let mut profile = ProfileSet::default().agent.remove(0);
        profile.egress.candidates.push(profile::Candidate {
            id: "validation".into(),
            host: "198.51.100.1".into(),
            port: 12324,
            proto: profile::UpstreamProto::Socks5,
            credential_ref: None,
        });
        profile.routing = self.routing.clone();
        crate::core::config::from_profile(
            &profile,
            ports,
            secret,
            &MemoryCredentials(&self.secrets),
        )
        .map_err(|e| e.to_string())
    }

    /// Store fresh credentials, then atomically save the profile. On any failure delete only the
    /// new entries from this attempt. Other profile fields (including the exit) are preserved.
    pub fn commit(
        self,
        mut set: ProfileSet,
        store: &ProfileStore,
        sink: &dyn CredentialSink,
    ) -> Result<(), String> {
        if set.agent.is_empty() {
            return Err("profile has no Agent entry".into());
        }
        let mut written = Vec::new();
        let result = (|| {
            for secret in &self.secrets {
                sink.store(&secret.reference, &secret.user, &secret.pass)
                    .map_err(|_| "credential storage failed".to_string())?;
                written.push(secret.reference.as_str());
            }
            set.agent[0].routing = self.routing;
            store.save(&set).map_err(|e| e.to_string())
        })();
        if let Err(e) = result {
            let mut cleanup_failed = false;
            for reference in written {
                cleanup_failed |= sink.delete(reference).is_err();
            }
            return Err(if cleanup_failed {
                format!("{e}; credential cleanup incomplete: remove import entries in Credential Manager")
            } else {
                e
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{config, CorePorts};
    const FIXTURE: &str = include_str!("../tests/fixtures/routing.yaml");

    #[test]
    fn preserves_names_members_health_order_notes_and_rendered_rule_triples() {
        let plan = parse(FIXTURE, "test").unwrap();
        plan.verify(FIXTURE).unwrap();
        assert_eq!(
            plan.routing
                .outbounds
                .iter()
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>(),
            ["ISP", "local", "encrypted"]
        );
        assert_eq!(plan.routing.groups[0].members, ["ISP", "relay"]);
        assert_eq!(
            plan.routing.groups[1].check.as_ref().unwrap(),
            &RoutingCheck {
                url: "https://example.com/health".into(),
                interval_sec: 30,
                tolerance_ms: 80
            }
        );
        assert_eq!(
            plan.routing.rules[0].note.as_deref(),
            Some("Keep the target on the ISP exit.\nexact match first")
        );
        assert_eq!(plan.routing.rules.len(), 5);
        assert_eq!(plan.routing.final_target, "daily");
        let json = serde_json::to_string(&plan.routing).unwrap();
        assert!(
            !json.contains("fixture-password") && !format!("{plan:?}").contains("fixture-password")
        );
        struct Creds;
        impl config::CredentialSource for Creds {
            fn get(&self, _: &str) -> Option<(String, String)> {
                Some(("fixture-user".into(), "fixture-password".into()))
            }
        }
        let mut p = ProfileSet::default().agent.remove(0);
        p.routing = plan.routing;
        p.egress.candidates.push(profile::Candidate {
            id: "exit".into(),
            host: "198.51.100.1".into(),
            port: 12324,
            proto: profile::UpstreamProto::Socks5,
            credential_ref: None,
        });
        let rendered = config::from_profile(&p, &CorePorts::default(), "secret", &Creds).unwrap();
        let generated: serde_yaml_ng::Value = serde_yaml_ng::from_str(&rendered).unwrap();
        let source: serde_yaml_ng::Value = serde_yaml_ng::from_str(FIXTURE).unwrap();
        assert_eq!(
            generated["rules"], source["rules"],
            "no injected, reordered or dropped rules"
        );
        assert_eq!(generated["proxy-groups"], source["proxy-groups"]);
        assert!(rendered.contains("dialer-proxy: \"transport\""));
        assert!(!config::redact(&rendered).contains("fixture-password"));
    }

    #[test]
    fn refuses_unsupported_or_ambiguous_tables_without_leaking_yaml() {
        for yaml in [
            FIXTURE.replace("type: http", "type: vmess"),
            FIXTURE.replace("udp: false", "udp: true"),
            FIXTURE.replace(
                "MATCH,daily",
                "MATCH,daily\n  - DOMAIN,unreachable.example,DIRECT",
            ),
            FIXTURE.replace("password: fixture-password", "password: [fixture-password"),
            FIXTURE.replace("type: ss", "type: ss\n    plugin: fixture-password"),
        ] {
            let e = parse(&yaml, "test").unwrap_err();
            assert!(!e.contains("fixture-password"));
        }
        let cycle = FIXTURE.replace("proxies: [local, DIRECT]", "proxies: [daily, DIRECT]");
        assert!(parse(&cycle, "test").unwrap_err().contains("cycle"));
    }

    #[test]
    fn failed_save_rolls_back_all_new_credentials_even_if_one_delete_fails() {
        use std::cell::RefCell;
        struct Sink {
            written: RefCell<Vec<String>>,
            deleted: RefCell<Vec<String>>,
        }
        impl CredentialSink for Sink {
            fn store(&self, r: &str, _: &str, _: &str) -> Result<(), String> {
                self.written.borrow_mut().push(r.into());
                Ok(())
            }
            fn delete(&self, r: &str) -> Result<(), String> {
                self.deleted.borrow_mut().push(r.into());
                Err("failure".into())
            }
        }
        let dir = std::env::temp_dir().join(format!("aec-import-rollback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A file blocks creation of the profiles directory.
        std::fs::write(dir.join("profiles"), b"existing-data").unwrap();
        let sink = Sink {
            written: RefCell::new(Vec::new()),
            deleted: RefCell::new(Vec::new()),
        };
        let plan = parse(FIXTURE, "rollback").unwrap();
        let result = plan.commit(ProfileSet::default(), &ProfileStore::new(&dir), &sink);
        assert!(result.unwrap_err().contains("cleanup incomplete"));
        assert_eq!(*sink.written.borrow(), *sink.deleted.borrow());
        assert_eq!(
            std::fs::read(dir.join("profiles")).unwrap(),
            b"existing-data"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
