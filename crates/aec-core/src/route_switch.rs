//! A tray/panel/Agent selection changes only the domains explicitly assigned to that group.
use crate::profile::{self, ProfileSet, Routing, RoutingRuleKind, Target};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Form {
    pub group: String,
    pub member: String,
}

pub struct Plan {
    pub profiles: ProfileSet,
    pub fixed: bool,
    pub domains: Vec<String>,
}

pub fn fixed_member(routing: &Routing, name: &str) -> bool {
    fn visit(routing: &Routing, name: &str, depth: usize) -> bool {
        if name == "relay" {
            return true;
        }
        if depth > routing.groups.len() {
            return false;
        }
        routing
            .groups
            .iter()
            .find(|g| g.name == name)
            .is_some_and(|g| {
                !g.members.is_empty() && g.members.iter().all(|m| visit(routing, m, depth + 1))
            })
    }
    visit(routing, name, 0)
}

fn contains(parent: &str, child: &str) -> bool {
    child.eq_ignore_ascii_case(parent)
        || child
            .to_ascii_lowercase()
            .ends_with(&format!(".{}", parent.to_ascii_lowercase()))
}

pub fn plan(set: &ProfileSet, form: &Form) -> Result<Plan, String> {
    if form.group == crate::local_upstream::GROUP {
        return Ok(Plan {
            profiles: crate::local_upstream::plan(
                set,
                &crate::local_upstream::Command::Select {
                    name: form.member.clone(),
                },
                crate::core::CorePorts::default(),
            )?,
            fixed: false,
            domains: Vec::new(),
        });
    }
    let p = set.agent.first().ok_or("缺少 Agent 档案")?;
    let index = p
        .routing
        .groups
        .iter()
        .position(|g| g.name == form.group && g.switchable)
        .ok_or("路由组不可切换")?;
    let group = &p.routing.groups[index];
    if !group.members.contains(&form.member) {
        return Err("所选出站不属于这个组".into());
    }
    let fixed = fixed_member(&p.routing, &form.member);
    let mut domains = Vec::new();
    for (position, rule) in p
        .routing
        .rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.target == form.group && r.kind == RoutingRuleKind::DomainSuffix)
    {
        if p.routing.target_for_domain(&rule.value) != Some(form.group.as_str()) {
            return Err("该组的域名规则被更早的规则覆盖，请先整理规则顺序".into());
        }
        let domain = rule.value.to_ascii_lowercase();
        // Matching the parent alone does not prove that its children have no
        // earlier exception. Refuse ambiguity rather than changing another group.
        if p.egress.fixed_exit
            && p.routing.rules[..position].iter().any(|earlier| {
                earlier.target != form.group
                    && match earlier.kind {
                        RoutingRuleKind::Domain | RoutingRuleKind::DomainSuffix => {
                            contains(&domain, &earlier.value)
                        }
                        RoutingRuleKind::DomainKeyword => true,
                        _ => false,
                    }
            })
        {
            return Err("该域名范围含更早的子域或关键词例外，请先在看板明确保护范围".into());
        }
        if !domains.contains(&domain) {
            domains.push(domain);
        }
    }
    let mut next = set.clone();
    let next_p = &mut next.agent[0];
    next_p.routing.groups[index].selected = Some(form.member.clone());
    if p.egress.fixed_exit && !domains.is_empty() {
        // A subgroup is never allowed to remove a broader, separately protected parent.
        if !fixed
            && p.targets.iter().any(|t| {
                domains
                    .iter()
                    .any(|d| contains(&t.domain, d) && !contains(d, &t.domain))
            })
        {
            return Err("所选域名仍受更大范围的父域名保护，请在看板明确调整范围".into());
        }
        if fixed {
            for domain in &domains {
                if !next_p
                    .targets
                    .iter()
                    .any(|t| t.domain.eq_ignore_ascii_case(domain))
                {
                    // Do not add new profile fields: older binaries must still load
                    // this profile after rollback. A site is not assumed to expose
                    // Cloudflare trace; exact ISP evidence uses the dedicated probe.
                    next_p.targets.push(Target {
                        domain: domain.clone(),
                        trace: false,
                    });
                }
            }
        } else {
            next_p
                .targets
                .retain(|t| !domains.iter().any(|d| contains(d, &t.domain)));
        }
    }
    if !profile::validate(&next).is_empty() {
        return Err(
            "路由切换后的配置无效；至少保留一个受保护检测站点，并检查出口与路由设置".into(),
        );
    }
    Ok(Plan {
        profiles: next,
        fixed,
        domains,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{config, CorePorts},
        profile::*,
    };

    fn configured() -> ProfileSet {
        let mut set = ProfileSet::default();
        let p = &mut set.agent[0];
        p.egress.fixed_exit = true;
        p.egress.expected.ip = Some("198.51.100.7".into());
        p.egress.candidates = vec![Candidate {
            id: "test".into(),
            host: "198.51.100.7".into(),
            port: 1080,
            proto: UpstreamProto::Socks5,
            credential_ref: None,
        }];
        p.targets = vec![
            Target {
                domain: "chatgpt.com".into(),
                trace: true,
            },
            Target {
                domain: "claude.ai".into(),
                trace: true,
            },
        ];
        p.routing = serde_json::from_value(serde_json::json!({
            "groups":[{"name":"GPT","type":"select","members":["ISP","DIRECT"],"switchable":true}, {"name":"ISP","type":"fallback","members":["relay"],"check":{"url":"https://example.com/","intervalSec":30,"toleranceMs":0}}],
            "rules":[{"kind":"domain_suffix","value":"chatgpt.com","target":"GPT"}, {"kind":"domain_suffix","value":"openai.com","target":"GPT"}],"final":"DIRECT"
        })).unwrap();
        set
    }
    fn selection(member: &str) -> Form {
        Form {
            group: "GPT".into(),
            member: member.into(),
        }
    }
    struct NoSecrets;
    impl config::CredentialSource for NoSecrets {
        fn get(&self, _: &str) -> Option<(String, String)> {
            None
        }
    }

    #[test]
    fn fixed_mode_quick_switch_changes_only_its_domain_scope_and_round_trips() {
        let set = configured();
        let exit = set.agent[0].egress.clone();
        let ordinary = plan(&set, &selection("DIRECT")).unwrap();
        assert!(!ordinary.fixed);
        assert_eq!(ordinary.profiles.agent[0].egress, exit);
        assert_eq!(
            ordinary.profiles.agent[0].targets,
            vec![set.agent[0].targets[1].clone()]
        );
        let yaml = config::from_profile(
            &ordinary.profiles.agent[0],
            &CorePorts::default(),
            "test",
            &NoSecrets,
        )
        .unwrap();
        assert!(!yaml.contains("DOMAIN-SUFFIX,chatgpt.com,relay"));
        assert!(yaml.contains("DOMAIN-SUFFIX,chatgpt.com,GPT"));
        assert!(yaml.contains("DOMAIN-SUFFIX,claude.ai,relay"));
        let fixed = plan(&ordinary.profiles, &selection("ISP")).unwrap();
        assert!(fixed.fixed);
        assert!(fixed.profiles.agent[0]
            .targets
            .iter()
            .any(|t| t.domain == "chatgpt.com" && !t.trace));
        assert!(fixed.profiles.agent[0]
            .targets
            .iter()
            .any(|t| t.domain == "openai.com"));
        let yaml = config::from_profile(
            &fixed.profiles.agent[0],
            &CorePorts::default(),
            "test",
            &NoSecrets,
        )
        .unwrap();
        assert!(yaml.contains("DOMAIN-SUFFIX,chatgpt.com,relay"));
    }

    #[test]
    fn overlapping_parent_protection_cannot_be_removed_by_a_child_group() {
        let mut set = configured();
        set.agent[0].targets.push(Target {
            domain: "example.com".into(),
            trace: false,
        });
        set.agent[0].routing.rules[0].value = "child.example.com".into();
        assert!(plan(&set, &selection("DIRECT")).is_err());
    }

    #[test]
    fn invalid_members_and_shadowed_scopes_are_rejected_without_changing_profiles() {
        let mut set = configured();
        let before = set.clone();
        assert!(plan(&set, &selection("unknown")).is_err());
        assert_eq!(set, before);
        set.agent[0].routing.rules.insert(
            0,
            RoutingRule {
                kind: RoutingRuleKind::DomainSuffix,
                value: "chatgpt.com".into(),
                target: "REJECT".into(),
                no_resolve: false,
                note: None,
            },
        );
        assert!(plan(&set, &selection("DIRECT")).is_err());
    }

    #[test]
    fn a_child_exception_must_not_lose_protection_when_switching_its_parent_group() {
        let mut set = configured();
        set.agent[0].targets.push(Target {
            domain: "child.chatgpt.com".into(),
            trace: false,
        });
        set.agent[0].routing.rules.insert(
            0,
            RoutingRule {
                kind: RoutingRuleKind::DomainSuffix,
                value: "child.chatgpt.com".into(),
                target: "ISP".into(),
                no_resolve: false,
                note: None,
            },
        );
        assert!(
            plan(&set, &selection("DIRECT")).is_err(),
            "a different group's child protection must not be removed"
        );
        assert!(
            plan(&set, &selection("ISP")).is_err(),
            "fixed mode must not shadow child exceptions either"
        );
    }
}
