//! Structured dashboard contract; no credentials, raw YAML or controller tokens are exposed.
use crate::profile::{self, FieldError, ProfileSet, RoutingRule, Target};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RulesForm {
    pub sites: Vec<String>,
    pub rules: Vec<RoutingRule>,
    pub final_target: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub upstreams: crate::local_upstream::View,
    pub switches: Vec<RouteChoice>,
    pub autostart: bool,
    pub mode: String,
    pub busy: bool,
    pub fixed_exit: bool,
    pub blocked: bool,
    pub path: String,
    pub paths: Vec<(String, String)>,
    pub targets: Vec<String>,
    pub form: RulesForm,
    pub suggested: Vec<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteChoice {
    pub group: String,
    pub member: String,
    pub selected: bool,
    pub fixed: bool,
}

pub fn view(
    set: &ProfileSet,
    mode: &str,
    busy: bool,
    blocked: bool,
    summary: Option<String>,
) -> View {
    let p = &set.agent[0];
    let mut paths = vec![("direct".into(), "直接连接 ISP 代理".into())];
    if let Some(t) = p.egress.tunnel.as_ref().filter(|t| t.enabled) {
        paths.push(("tunnel-auto".into(), "官方隧道 · 自动选择可用节点".into()));
        for (i, s) in t.servers.iter().enumerate() {
            paths.push((
                format!("tunnel-{}", i + 1),
                format!("隧道节点 {} · {}:{}", i + 1, s.host, s.port),
            ));
        }
    }
    let mut targets = vec!["relay".into(), "DIRECT".into(), "REJECT".into()];
    targets.extend(p.routing.groups.iter().map(|g| g.name.clone()));
    targets.extend(p.routing.outbounds.iter().map(|o| o.name.clone()));
    View {
        upstreams: crate::local_upstream::view(set),
        switches: p
            .routing
            .groups
            .iter()
            .filter(|g| g.switchable)
            .flat_map(|g| {
                g.members.iter().map(|m| RouteChoice {
                    group: g.name.clone(),
                    member: m.clone(),
                    selected: g.selected.as_ref().or(g.members.first()) == Some(m),
                    fixed: crate::route_switch::fixed_member(&p.routing, m),
                })
            })
            .collect(),
        autostart: false,
        mode: mode.into(),
        busy,
        fixed_exit: p.egress.fixed_exit,
        blocked,
        path: p.egress.connection_path.clone().unwrap_or("direct".into()),
        paths,
        targets,
        form: RulesForm {
            sites: p.targets.iter().map(|t| t.domain.clone()).collect(),
            rules: p.routing.rules.clone(),
            final_target: p.routing.final_target.clone(),
        },
        suggested: [
            "claude.ai",
            "anthropic.com",
            "claude.com",
            "claudeusercontent.com",
            "chatgpt.com",
            "openai.com",
            "oaistatic.com",
            "oaiusercontent.com",
            "sora.com",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        summary,
    }
}

/// Atomic validation: errors never modify the original. The page cannot edit exit identity.
pub fn apply(set: &mut ProfileSet, form: RulesForm) -> Result<(), Vec<FieldError>> {
    let err = |message: &str| {
        vec![FieldError {
            path: "rules".into(),
            message: message.into(),
        }]
    };
    if form.rules.len() > 512 || form.sites.len() > 128 {
        return Err(err("最多 512 条规则和 128 个站点"));
    }
    let mut next = set.clone();
    let p = &mut next.agent[0];
    p.targets = form
        .sites
        .into_iter()
        .map(|d| {
            let domain = d.trim().to_ascii_lowercase();
            let trace = p
                .targets
                .iter()
                .any(|t| t.domain.eq_ignore_ascii_case(&domain) && t.trace);
            Target { domain, trace }
        })
        .collect();
    p.routing.rules = form.rules;
    p.routing.final_target = form.final_target;
    let problems = profile::validate(&next);
    if !problems.is_empty() {
        return Err(problems);
    }
    *set = next;
    Ok(())
}

pub fn page(token: &str, view: &View) -> String {
    crate::ui::template(
        "dashboard.html",
        include_str!("../../../ui/revisions/fluent-2/dashboard.html"),
    )
    .replace("__TOKEN__", token)
    .replace(
        "__VIEW__",
        &serde_json::to_string(view)
            .expect("serializes")
            .replace("</", "<\\/"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn configured() -> ProfileSet {
        let mut set = ProfileSet::default();
        set.agent[0].egress.candidates = vec![crate::profile::Candidate {
            id: "fake".into(),
            host: "198.51.100.7".into(),
            port: 1080,
            proto: crate::profile::UpstreamProto::Socks5,
            credential_ref: None,
        }];
        set
    }
    #[test]
    fn edits_preserve_order_and_exit_and_reject_invalid_payloads() {
        let mut set = configured();
        let exit = set.agent[0].egress.clone();
        let mut form = view(&set, "daily", false, false, None).form;
        form.sites = vec![" Example.COM ".into(), "claude.ai".into()];
        form.rules = serde_json::from_value(serde_json::json!([
            {"kind":"domain","value":"blocked.example.com","target":"REJECT"},
            {"kind":"domain_suffix","value":"example.com","target":"relay"}
        ]))
        .unwrap();
        form.final_target = "DIRECT".into();
        apply(&mut set, form).unwrap();
        assert_eq!(set.agent[0].targets[0].domain, "example.com");
        assert_eq!(
            set.agent[0]
                .routing
                .target_for_domain("blocked.example.com"),
            Some("REJECT")
        );
        assert_eq!(set.agent[0].egress, exit);
        let before = set.clone();
        for (kind, value, target) in [
            ("domain", "https://example.com", "relay"),
            ("ip_cidr", "not-a-cidr", "relay"),
            ("domain", "example.com", "aec-relay-transport"),
        ] {
            let mut form = view(&set, "daily", false, false, None).form;
            form.rules = serde_json::from_value(
                serde_json::json!([{"kind":kind,"value":value,"target":target}]),
            )
            .unwrap();
            assert!(apply(&mut set, form).is_err());
            assert_eq!(set, before);
        }
    }
    #[test]
    fn site_edits_preserve_probe_type_and_new_sites_use_reachability() {
        let mut set = configured();
        set.agent[0].targets = vec![
            Target {
                domain: "claude.ai".into(),
                trace: true,
            },
            Target {
                domain: "example.com".into(),
                trace: false,
            },
        ];
        let mut form = view(&set, "daily", false, false, None).form;
        form.sites = vec![
            " CLAUDE.AI ".into(),
            "example.com".into(),
            "new.example.com".into(),
        ];
        apply(&mut set, form).unwrap();
        assert!(set.agent[0].targets[0].trace);
        assert!(!set.agent[0].targets[1].trace);
        assert!(!set.agent[0].targets[2].trace);
    }
    #[test]
    fn invalid_rule_edit_is_atomic_and_cannot_change_exit() {
        let mut set = ProfileSet::default();
        let original = set.clone();
        let mut f = view(&set, "daily", false, true, None).form;
        f.final_target = "missing".into();
        assert!(apply(&mut set, f).is_err());
        assert_eq!(set, original);
    }
    #[test]
    fn page_escapes_script_injection() {
        let mut v = view(&ProfileSet::default(), "daily", false, true, None);
        v.summary = Some("</script><script>alert(1)</script>".into());
        assert!(!page("token", &v).contains("</script><script>alert"));
    }
}
