//! Scoped client cooperation. Planning never writes client files or system proxy settings.
use serde_yaml_ng::Value;

pub const OUTBOUND: &str = "AEC-MANAGED-FIXED";

#[cfg(windows)]
#[path = "cooperation/client.rs"]
mod client;
#[cfg(windows)]
pub use client::{record_failure, Manager};
#[derive(Clone, serde::Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Command {
    State,
    Preview,
    Install {
        #[serde(rename = "planId")]
        plan_id: String,
    },
    Restore,
    Verify,
    DnsProbe {
        consent: bool,
    },
}
impl<'de> serde::Deserialize<'de> for Command {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        let invalid = || D::Error::custom("COOP_COMMAND_INVALID");
        let object = value.as_object().ok_or_else(invalid)?;
        let action = object
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        let keys: &[&str] = match action {
            "state" | "preview" | "restore" | "verify" => &["action"],
            "install" => &["action", "planId"],
            "dns_probe" => &["action", "consent"],
            _ => return Err(invalid()),
        };
        if object.len() != keys.len() || object.keys().any(|k| !keys.contains(&k.as_str())) {
            return Err(invalid());
        }
        Ok(match action {
            "state" => Self::State,
            "preview" => Self::Preview,
            "restore" => Self::Restore,
            "verify" => Self::Verify,
            "install" => Self::Install {
                plan_id: value["planId"].as_str().ok_or_else(invalid)?.into(),
            },
            _ => Self::DnsProbe {
                consent: value["consent"].as_bool().ok_or_else(invalid)?,
            },
        })
    }
}
#[path = "cooperation/evidence.rs"]
mod evidence;
#[cfg(windows)]
#[path = "cooperation/windows.rs"]
pub mod windows;
pub use evidence::{evaluate_dns, probe_fixed_dns, scope_check, verify_runtime, RuntimeHost};

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Selection {
    pub domains: Vec<String>,
    pub fixed_port: u16,
}

impl Selection {
    /// The exact user-approved scope used by the tray, export and runtime checks.
    /// Never inherit internal Claude-family protection or infer additional AI domains.
    pub fn for_profile(profile: &crate::profile::AgentProfile) -> Self {
        let domains: Vec<String> = profile.targets.iter().map(|t| t.domain.clone()).collect();
        Self::for_sites(domains)
    }
    pub fn for_sites(domains: Vec<String>) -> Self {
        Self {
            domains,
            fixed_port: crate::fixed::ingress_port(&crate::core::CorePorts::default())
                .expect("default fixed ingress"),
        }
    }
}

pub struct Plan {
    config: Value,
}

impl Plan {
    /// For the trusted client adapter and isolated validation, never a dashboard response.
    pub fn candidate_yaml(&self) -> String {
        serde_yaml_ng::to_string(&self.config).expect("YAML value serializes")
    }
}

/// A subscription script contains only the owned rules, DNS policies and loopback endpoint.
/// It does not contain the client's config, node credentials, controller secret or choices.
/// Registration/activation and runtime readback are separate operations, never implied here.
pub fn client_script(selection: &Selection) -> Result<String, &'static str> {
    let clean = "mode: rule\ndns:\n  enable: true\nproxies: []\nrules: []\n";
    let plan = prepare(clean, selection)?;
    let block = serde_json::json!({
        "proxy": plan.config["proxies"][0],
        "rules": plan.config["rules"],
        "policies": plan.config["dns"]["nameserver-policy"],
    });
    Ok(format!("/* AEC-owned site forwarding. Install last in the selected subscription, not as a whole config. */\nconst AEC_BLOCK = {block};\n{}", r#"
function aecApplyFixedSites(config) {
  if (!config || typeof config !== 'object' || (config.mode && config.mode.toLowerCase() !== 'rule'))
    throw new Error('COOP_CLIENT_NOT_RULE_MODE');
  if (!config.dns || config.dns.enable !== true) throw new Error('COOP_CLIENT_DNS_DISABLED');
  const port = AEC_BLOCK.proxy.port;
  if (['mixed-port','port','socks-port','redir-port','tproxy-port'].some(k => config[k] === port)
      || (config.listeners || []).some(l => l.port === port)) throw new Error('COOP_PROXY_LOOP');
  const same = (a,b) => JSON.stringify(a) === JSON.stringify(b);
  const existing = (config.proxies || []).filter(p => p.name === AEC_BLOCK.proxy.name);
  if (existing.some(p => Object.keys(p).length !== 4 ||
      ['name','type','server','port'].some(k => p[k] !== AEC_BLOCK.proxy[k]))
      || (config['proxy-groups'] || []).some(g => g.name === AEC_BLOCK.proxy.name))
    throw new Error('COOP_OWNERSHIP_CONFLICT');
  const policies = {...(config.dns['nameserver-policy'] || {})};
  for (const [domain, servers] of Object.entries(AEC_BLOCK.policies)) {
    if (policies[domain] && !same(policies[domain],servers)) throw new Error('COOP_DNS_POLICY_CONFLICT');
    policies[domain] = servers;
  }
  return {...config,
    proxies: [...(config.proxies || []).filter(p => p.name !== AEC_BLOCK.proxy.name), {...AEC_BLOCK.proxy}],
    rules: [...AEC_BLOCK.rules, ...(config.rules || []).filter(r => !AEC_BLOCK.rules.includes(r))],
    dns: {...config.dns, 'nameserver-policy': policies}
  };
}
function main(config, profileName) {
  try { return aecApplyFixedSites(config); }
  catch (error) {
    // CVR falls back to the original config on thrown errors. For supported rule-mode
    // inputs, return explicit selected-site rejection instead; preserve ordinary rules.
    // Global/Direct mode cannot be protected by domain rules. Never claim otherwise.
    if (!config || typeof config !== 'object' || (config.mode && config.mode.toLowerCase() !== 'rule')) throw error;
    const rejects = AEC_BLOCK.rules.filter(r => r.startsWith('DOMAIN-SUFFIX,'))
      .map(r => r.substring(0,r.lastIndexOf(',')) + ',REJECT');
    return {...config, rules:[...rejects,...(config.rules || [])]};
  }
}
"#))
}

pub fn prepare(current: &str, selection: &Selection) -> Result<Plan, &'static str> {
    let domains: Vec<_> = selection
        .domains
        .iter()
        .map(|domain| domain.trim().trim_end_matches('.').to_ascii_lowercase())
        .collect();
    if domains.is_empty()
        || domains.len() > 128
        || domains.iter().any(|domain| {
            domain.len() > 253
                || !domain.contains('.')
                || domain.parse::<std::net::IpAddr>().is_ok()
                || domain.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || label.starts_with('-')
                        || label.ends_with('-')
                        || !label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
        })
    {
        return Err("COOP_DOMAIN_INVALID");
    }
    let mut config: Value = serde_yaml_ng::from_str(current).map_err(|_| "COOP_CONFIG_INVALID")?;
    if config
        .get("mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| !mode.eq_ignore_ascii_case("rule"))
    {
        return Err("COOP_CLIENT_NOT_RULE_MODE");
    }
    if ["proxies", "proxy-groups"].iter().any(|key| {
        config
            .get(*key)
            .and_then(Value::as_sequence)
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.get("name").and_then(Value::as_str) == Some(OUTBOUND))
            })
    }) {
        return Err("COOP_OWNERSHIP_CONFLICT");
    }
    if selection.fixed_port == 0
        || [
            "mixed-port",
            "port",
            "socks-port",
            "redir-port",
            "tproxy-port",
        ]
        .iter()
        .any(|key| {
            config.get(*key).and_then(Value::as_u64) == Some(u64::from(selection.fixed_port))
        })
        || config
            .get("listeners")
            .and_then(Value::as_sequence)
            .is_some_and(|listeners| {
                listeners.iter().any(|listener| {
                    listener.get("port").and_then(Value::as_u64)
                        == Some(u64::from(selection.fixed_port))
                })
            })
    {
        return Err("COOP_PROXY_LOOP");
    }
    if config
        .get("dns")
        .and_then(|v| v.get("enable"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err("COOP_CLIENT_DNS_DISABLED");
    }
    let root = config.as_mapping_mut().ok_or("COOP_CONFIG_INVALID")?;
    let proxies = root
        .entry(Value::from("proxies"))
        .or_insert_with(|| Value::Sequence(Vec::new()))
        .as_sequence_mut()
        .ok_or("COOP_CONFIG_INVALID")?;
    proxies.push(
        serde_yaml_ng::from_str(&format!(
            "name: {OUTBOUND}\ntype: http\nserver: 127.0.0.1\nport: {}\n",
            selection.fixed_port
        ))
        .expect("fixed proxy schema"),
    );
    let rules = root
        .entry(Value::from("rules"))
        .or_insert_with(|| Value::Sequence(Vec::new()))
        .as_sequence_mut()
        .ok_or("COOP_CONFIG_INVALID")?;
    let mut managed: Vec<_> = domains
        .iter()
        .map(|domain| {
            Value::from(format!(
                "AND,((DOMAIN-SUFFIX,{domain}),(NETWORK,UDP)),REJECT"
            ))
        })
        .collect();
    managed.extend(
        domains
            .iter()
            .map(|domain| Value::from(format!("DOMAIN-SUFFIX,{domain},{OUTBOUND}"))),
    );
    managed.append(rules);
    *rules = managed;
    let dns = root
        .get_mut(Value::from("dns"))
        .and_then(Value::as_mapping_mut)
        .ok_or("COOP_CONFIG_INVALID")?;
    let policies = dns
        .entry(Value::from("nameserver-policy"))
        .or_insert_with(|| Value::Mapping(Default::default()))
        .as_mapping_mut()
        .ok_or("COOP_CONFIG_INVALID")?;
    for domain in &domains {
        let key = Value::from(format!("+.{domain}"));
        let desired = Value::Sequence(vec![Value::from(format!(
            "https://1.1.1.1/dns-query#{OUTBOUND}&ecs=0.0.0.0/0&ecs-override=true"
        ))]);
        if policies
            .get(&key)
            .is_some_and(|existing| existing != &desired)
        {
            return Err("COOP_DNS_POLICY_CONFLICT");
        }
        policies.insert(key, desired);
    }
    Ok(Plan { config })
}
