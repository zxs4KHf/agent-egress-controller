//! Troubleshooting prompts (SPEC 4.8, ADR-0005): fixed-template text a user can copy to an agent.
//! Generating never sends anything; the caller only puts the text on the clipboard.

use std::fmt::Write as _;

use crate::checks::{CheckReport, CheckResult, Kind, Status};
use crate::controller::{RestoreItem, RestoreStatus};
use crate::profile::{AgentProfile, FieldError};
use crate::snapshot::TakeoverGate;

/// Evidence longer than this is cut, so a prompt stays readable.
const MAX_EVIDENCE: usize = 2048;

const FORBIDDEN: [&str; 6] = [
    "其他代理或出口工具的目录、进程、启动项和配置（它们不归本应用管）",
    "用户 PATH、开机启动项和桌面快捷方式",
    "本应用的「接管全局状态」设置（由用户决定）",
    "系统级配置（HKLM 注册表、服务、计划任务）",
    "防火墙规则、路由表、虚拟网卡",
    "不要重启或结束本应用之外的代理进程",
];

pub struct PromptContext<'a> {
    pub mode: &'a str,
    /// When the report being explained was produced; the mode may have changed since.
    pub checked_at: Option<&'a str>,
    pub profile: &'a AgentProfile,
    pub data_dir: &'a str,
    /// Reduce IPv4 addresses to /24 and IPv6 to the first three groups.
    pub mask_ip: bool,
}

// ---- redaction ------------------------------------------------------------------------------

/// Replace `key: value` / `key=value` values of sensitive keys, case-insensitively.
fn redact_keyed(text: &str) -> String {
    const KEYS: [&str; 6] = [
        "secret",
        "password",
        "passwd",
        "token",
        "authorization",
        "credentialref",
    ];
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    'outer: while i < text.len() {
        for key in KEYS {
            if lower[i..].starts_with(key) {
                let mut j = i + key.len();
                let bytes = text.as_bytes();
                while j < text.len() && matches!(bytes[j], b' ' | b'"' | b'\'') {
                    j += 1;
                }
                if j < text.len() && matches!(bytes[j], b':' | b'=') {
                    j += 1;
                    while j < text.len() && matches!(bytes[j], b' ' | b'"' | b'\'') {
                        j += 1;
                    }
                    let end_of_word = |mut k: usize| {
                        while k < text.len()
                            && !matches!(
                                bytes[k],
                                b' ' | b'"' | b'\'' | b',' | b';' | b'\n' | b'&' | b'}'
                            )
                        {
                            k += 1;
                        }
                        k
                    };
                    let mut k = end_of_word(j);
                    // `Authorization: Bearer <token>`: the scheme word is not the secret, the next one is.
                    let scheme = &text[j..k];
                    if scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("basic")
                    {
                        let mut n = k;
                        while n < text.len() && bytes[n] == b' ' {
                            n += 1;
                        }
                        k = end_of_word(n);
                    }
                    out.push_str(&text[i..j]);
                    out.push_str("***");
                    i = k;
                    continue 'outer;
                }
            }
        }
        let c = text[i..].chars().next().expect("in bounds");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// `Bearer abc` → `Bearer ***`; `scheme://user:pass@host` and `user:pass@host` → `***@host`.
fn redact_auth(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut bearer = false;
    for w in text.split(' ') {
        if bearer {
            words.push("***".into());
            bearer = false;
            continue;
        }
        bearer = w.eq_ignore_ascii_case("bearer");
        let w = match w.find('@') {
            Some(at) if w[..at].contains(':') => {
                let start = w.find("://").map_or(0, |s| s + 3);
                if start <= at {
                    format!("{}***{}", &w[..start], &w[at..])
                } else {
                    w.to_string()
                }
            }
            _ => w.to_string(),
        };
        words.push(w);
    }
    words.join(" ")
}

fn mask_ipv4(text: &str) -> String {
    let mut out = String::new();
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() == 4
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
        {
            let _ = write!(out, "{}.{}.{}.x", parts[0], parts[1], parts[2]);
        } else {
            out.push_str(token);
        }
        token.clear();
    };
    for c in text.chars() {
        if c.is_ascii_digit() || c == '.' {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

pub fn redact(text: &str, mask_ip: bool) -> String {
    let t = redact_auth(&redact_keyed(text));
    if mask_ip {
        mask_ipv4(&t)
    } else {
        t
    }
}

// ---- templates ------------------------------------------------------------------------------

fn status_zh(s: Status) -> &'static str {
    match s {
        Status::Pass => "通过",
        Status::Warn => "警告",
        Status::Fail => "失败",
        Status::Unknown => "未知",
    }
}

fn title(id: &str) -> String {
    match id.split_once(':') {
        Some(("G2", domain)) => format!("目标域名 {domain} 可达且出口一致"),
        _ => match id {
            "G1" => "出口生效".into(),
            "G3" => "DNS 路径".into(),
            "G4" => "时区".into(),
            "G5" => "浏览器语言".into(),
            "G6" => "系统代理作用域".into(),
            "R1" => "出口 ASN/地区".into(),
            "R2" => "延迟".into(),
            "R9" => "非目标流量出口".into(),
            "R10" => "时区归属".into(),
            other => other.into(),
        },
    }
}

fn expectation(id: &str, p: &AgentProfile) -> String {
    let group = &p.routing.target_group;
    match id.split_once(':') {
        Some(("G2", d)) => format!("经本应用内核访问 https://{d}/cdn-cgi/trace 能收到响应，并且它看到的出口 IP 与其他目标一致。"),
        _ => match id {
            "G1" => {
                let ip = p.egress.expected.ip.as_deref().map_or("（尚未固定预期出口 IP）".to_string(), |ip| format!("预期出口 IP 为 {ip}"));
                format!("目标站看到的出口来自档案的固定出口，不同于不经代理时的出口；{ip}。")
            }
            "G3" => format!("每个目标域名都以主机名交给内核，并经策略组「{group}」出站；内核 DNS 端口在监听。"),
            "G4" => format!("系统时区为 {}（{}）。", p.locale.timezone.windows_id, p.locale.timezone.iana),
            "R10" => "时区由本应用在本次会话中设置，而不是由其他程序维持。".into(),
            "R1" => "两个查询来源给出的出口 ASN 和国家一致，并与档案预期一致。".into(),
            "R2" => "经出口访问目标的中位延迟不超过 1500 ms。".into(),
            _ => "见 docs/SPEC.md 4.5 对该检查项的定义。".into(),
        },
    }
}

fn allowed(id: &str, data_dir: &str) -> Vec<String> {
    let profiles = format!(r"本应用的档案文件 {data_dir}\profiles\profiles.json（或设置页）");
    match id.split_once(':').map_or(id, |(g, _)| g) {
        "G1" | "G2" | "R1" | "R2" => vec![
            format!("{profiles} 中的出口候选、预期出口和目标域名"),
            "本应用自己的内核（aec-dev core down / up）".into(),
        ],
        "G3" => vec![format!("{profiles} 中的路由组和目标域名")],
        "G4" | "R10" => vec![
            format!("{profiles} 中的时区目标"),
            "不要直接改系统时区；如确需由本应用接管，请把原因告诉用户，由用户决定是否打开接管门"
                .into(),
        ],
        _ => vec![profiles],
    }
}

fn verification(id: &str) -> Vec<String> {
    let mut v = vec![
        "应用内「重新检查」，或运行：aec-dev recheck（Agent 模式中）/ aec-dev check".to_string(),
    ];
    match id.split_once(':').map_or(id, |(g, _)| g) {
        "G3" => v.push(
            "查看本轮内核连接记录：aec-dev logs 50（JSONL）以及数据目录 logs\\core-*.log".into(),
        ),
        "G4" | "R10" => v.push("只读确认：tzutil /g".into()),
        "G1" | "G2" => v.push(
            "只读确认：curl.exe -s -x http://127.0.0.1:17893 https://claude.ai/cdn-cgi/trace"
                .into(),
        ),
        _ => {}
    }
    v
}

fn evidence_block(c: &CheckResult, ctx: &PromptContext) -> String {
    let mut s = String::new();
    for e in &c.evidence {
        let _ = writeln!(s, "- {}: {}", e.source, e.detail);
    }
    if s.is_empty() {
        s.push_str("- （没有附带证据）\n");
    }
    let mut s = redact(&s, ctx.mask_ip);
    if s.len() > MAX_EVIDENCE {
        let mut cut = MAX_EVIDENCE;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n- （证据过长，已截断）\n");
    }
    s
}

fn sections(
    out: &mut String,
    background: &str,
    expect: &str,
    evidence: &str,
    allowed: &[String],
    verify: &[String],
) {
    let _ = writeln!(out, "## 背景\n{background}\n");
    let _ = writeln!(out, "## 期望\n{expect}\n");
    let _ = write!(out, "## 证据\n{evidence}\n");
    out.push_str("## 允许你修改的范围\n");
    for a in allowed {
        let _ = writeln!(out, "- {a}");
    }
    out.push_str("\n## 不允许修改\n");
    for f in FORBIDDEN {
        let _ = writeln!(out, "- {f}");
    }
    out.push_str("\n## 验证方法\n");
    for v in verify {
        let _ = writeln!(out, "- {v}");
    }
    out.push_str("\n## 回滚要求\n- 先说明打算改什么、为什么；改完逐项列出改动，并给出改回去的具体步骤或命令。\n- 只做只读排查时，不要修改任何文件。\n");
}

/// Prompt for one check that is not a plain pass. `None` for a pass.
pub fn for_check(c: &CheckResult, ctx: &PromptContext) -> Option<String> {
    if c.status == Status::Pass {
        return None;
    }
    let kind = if c.kind == Kind::Hard {
        "硬门槛"
    } else {
        "只读信号"
    };
    let background = format!(
        "我在 Windows 本机使用 Agent Egress Controller（Agent 网络与环境控制器），当前模式 {}。{}{kind} {}（{}）的结果是「{}」{}。请帮我排查原因。",
        ctx.mode,
        ctx.checked_at.map(|t| format!("在 {t}（UTC）的检查中，")).unwrap_or_default(),
        c.id,
        title(&c.id),
        status_zh(c.status),
        c.reason.as_deref().map(|r| format!("，原因代码 {r}")).unwrap_or_default()
    );
    let mut out = format!("# 排查：{} {}\n\n", c.id, title(&c.id));
    sections(
        &mut out,
        &background,
        &expectation(&c.id, ctx.profile),
        &evidence_block(c, ctx),
        &allowed(&c.id, ctx.data_dir),
        &verification(&c.id),
    );
    Some(out)
}

/// Prompt for a restore item that failed or conflicted. `None` otherwise.
pub fn for_restore(item: &RestoreItem, ctx: &PromptContext) -> Option<String> {
    let what = match item.status {
        RestoreStatus::Failed => "恢复失败",
        RestoreStatus::Conflict => "恢复冲突：退出时发现该值已被其他程序改过，本应用没有覆盖它",
        RestoreStatus::Restored | RestoreStatus::DryRun | RestoreStatus::LeftOpen => return None,
    };
    let background = format!("我在 Windows 本机使用 Agent Egress Controller，退出 Agent 模式时「{}」{what}。请帮我判断现在应该保留当前值，还是恢复到进入前的值。", item.item);
    let evidence = redact(&format!("- 恢复记录: {}\n", item.detail), ctx.mask_ip);
    let mut out = format!("# 排查：恢复项 {}\n\n", item.item);
    sections(
        &mut out,
        &background,
        "该项回到进入 Agent 模式前快照里的值，或由用户明确确认保留当前值（aec-dev ack）。",
        &evidence,
        &[
            format!(r"只读查看 {}\snapshots 下的快照与 state.json", ctx.data_dir),
            "需要改系统设置时，先把具体命令告诉用户，由用户执行".into(),
        ],
        &[
            "aec-dev exit（重试恢复）或 aec-dev ack（确认保留现状）".into(),
            "aec-dev state 查看剩余的恢复项".into(),
        ],
    );
    Some(out)
}

/// Prompt for profile settings gaps (validation errors). `None` when there are none.
pub fn for_settings(errors: &[FieldError], ctx: &PromptContext) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    let mut ev = String::new();
    for e in errors {
        let _ = writeln!(ev, "- {}: {}", e.path, e.message);
    }
    let mut out = String::from("# 排查：设置缺项\n\n");
    sections(
        &mut out,
        "我在 Windows 本机使用 Agent Egress Controller，档案校验没有通过。请帮我补齐或修正下列字段。",
        "aec-dev profile check 不再报告任何问题。凭据不要写进档案，只能写 credentialRef，由用户在设置页录入到 Windows 凭据管理器。",
        &redact(&ev, ctx.mask_ip),
        &[format!(r"{}\profiles\profiles.json 中报告的字段", ctx.data_dir)],
        &["aec-dev profile check".into()],
    );
    Some(out)
}

/// Prompt explaining a closed takeover gate. `None` when it is open.
pub fn for_gate(gate: &TakeoverGate, ctx: &PromptContext) -> Option<String> {
    if gate.state == crate::snapshot::GateState::Open {
        return None;
    }
    let mut ev = String::new();
    for r in &gate.reasons {
        let _ = writeln!(ev, "- {r}");
    }
    let mut out = String::from("# 排查：接管门关闭\n\n");
    sections(
        &mut out,
        "我在 Windows 本机使用 Agent Egress Controller，「接管全局状态」的接管门是关闭的，所以系统代理和时区只做了 dry-run。请帮我解释每条原因，并说明要打开它需要做什么；是否打开由我决定。",
        "接管门关闭时，本应用不会修改系统代理和时区；只有全部条件满足、并且我同意时才打开。",
        &redact(&ev, ctx.mask_ip),
        &["只读排查，不修改任何东西".into()],
        &["aec-dev status（外部写入者列表）".into()],
    );
    Some(out)
}

/// Every prompt for a report and its context, joined for "copy all".
pub fn all(
    report: Option<&CheckReport>,
    restore: &[RestoreItem],
    settings: &[FieldError],
    gate: Option<&TakeoverGate>,
    ctx: &PromptContext,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.extend(for_settings(settings, ctx));
    if let Some(g) = gate {
        parts.extend(for_gate(g, ctx));
    }
    if let Some(r) = report {
        parts.extend(r.results.iter().filter_map(|c| for_check(c, ctx)));
    }
    parts.extend(restore.iter().filter_map(|i| for_restore(i, ctx)));
    if parts.is_empty() {
        return "没有需要排查的问题。".into();
    }
    parts.join("\n---\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::Evidence;
    use crate::profile::ProfileSet;
    use crate::snapshot::GateState;

    fn ctx(p: &AgentProfile, mask: bool) -> PromptContext<'_> {
        PromptContext {
            mode: "agent_unverified",
            checked_at: None,
            profile: p,
            data_dir: r"D:\data",
            mask_ip: mask,
        }
    }

    fn check(id: &str, status: Status, reason: Option<&str>, ev: &[(&str, &str)]) -> CheckResult {
        CheckResult {
            id: id.into(),
            kind: if id.starts_with('G') {
                Kind::Hard
            } else {
                Kind::ReadOnly
            },
            status,
            reason: reason.map(str::to_string),
            evidence: ev
                .iter()
                .map(|(s, d)| Evidence {
                    source: (*s).into(),
                    detail: (*d).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn golden_g1_same_as_direct() {
        let p = ProfileSet::default().agent.remove(0);
        let c = check(
            "G1",
            Status::Fail,
            Some("same_as_direct"),
            &[
                ("https://claude.ai/cdn-cgi/trace", "exit 203.0.113.9 loc=CN"),
                (
                    "cf-trace",
                    "direct exit is also 203.0.113.9: the proxy changes nothing",
                ),
            ],
        );
        let got = for_check(&c, &ctx(&p, false)).unwrap();
        let want = r"# 排查：G1 出口生效

## 背景
我在 Windows 本机使用 Agent Egress Controller（Agent 网络与环境控制器），当前模式 agent_unverified。硬门槛 G1（出口生效）的结果是「失败」，原因代码 same_as_direct。请帮我排查原因。

## 期望
目标站看到的出口来自档案的固定出口，不同于不经代理时的出口；（尚未固定预期出口 IP）。

## 证据
- https://claude.ai/cdn-cgi/trace: exit 203.0.113.9 loc=CN
- cf-trace: direct exit is also 203.0.113.9: the proxy changes nothing

## 允许你修改的范围
- 本应用的档案文件 D:\data\profiles\profiles.json（或设置页） 中的出口候选、预期出口和目标域名
- 本应用自己的内核（aec-dev core down / up）

## 不允许修改
- 其他代理或出口工具的目录、进程、启动项和配置（它们不归本应用管）
- 用户 PATH、开机启动项和桌面快捷方式
- 本应用的「接管全局状态」设置（由用户决定）
- 系统级配置（HKLM 注册表、服务、计划任务）
- 防火墙规则、路由表、虚拟网卡
- 不要重启或结束本应用之外的代理进程

## 验证方法
- 应用内「重新检查」，或运行：aec-dev recheck（Agent 模式中）/ aec-dev check
- 只读确认：curl.exe -s -x http://127.0.0.1:17893 https://claude.ai/cdn-cgi/trace

## 回滚要求
- 先说明打算改什么、为什么；改完逐项列出改动，并给出改回去的具体步骤或命令。
- 只做只读排查时，不要修改任何文件。
";
        assert_eq!(got, want);
    }

    #[test]
    fn passes_produce_no_prompt_and_every_failure_kind_does() {
        let p = ProfileSet::default().agent.remove(0);
        let c = ctx(&p, false);
        assert!(for_check(&check("G1", Status::Pass, None, &[]), &c).is_none());
        for (id, st, reason) in [
            ("G1", Status::Unknown, "no_target_trace"),
            ("G2:chatgpt.com", Status::Fail, "split_routing"),
            ("G3", Status::Fail, "wrong_group"),
            ("G3", Status::Unknown, "no_core_log"),
            ("G4", Status::Unknown, "global_state_not_taken_over"),
            ("R10", Status::Warn, "maintained_externally"),
            ("R1", Status::Warn, "sources_disagree"),
        ] {
            let text = for_check(&check(id, st, Some(reason), &[]), &c).unwrap();
            for section in [
                "## 背景",
                "## 期望",
                "## 证据",
                "## 允许你修改的范围",
                "## 不允许修改",
                "## 验证方法",
                "## 回滚要求",
            ] {
                assert!(text.contains(section), "{id}: {section}");
            }
            assert!(
                text.contains(reason) && text.contains("其他代理或出口工具"),
                "{id}"
            );
        }
        let g2 = for_check(&check("G2:chatgpt.com", Status::Fail, None, &[]), &c).unwrap();
        assert!(g2.contains("目标域名 chatgpt.com"));
        let dated = PromptContext {
            checked_at: Some("2026-10-11T08:34:38Z"),
            ..ctx(&p, false)
        };
        let t = for_check(&check("G4", Status::Unknown, None, &[]), &dated).unwrap();
        assert!(
            t.contains(
                "当前模式 agent_unverified。在 2026-10-11T08:34:38Z（UTC）的检查中，硬门槛 G4"
            ),
            "{t}"
        );
    }

    #[test]
    fn restore_gate_and_settings_prompts() {
        let p = ProfileSet::default().agent.remove(0);
        let c = ctx(&p, false);
        let conflict = RestoreItem {
            item: "Timezone".into(),
            status: RestoreStatus::Conflict,
            detail: "changed by somebody else to Tokyo Standard Time".into(),
        };
        let t = for_restore(&conflict, &c).unwrap();
        assert!(
            t.contains("恢复冲突")
                && t.contains("Tokyo Standard Time")
                && t.contains("aec-dev ack")
        );
        assert!(for_restore(
            &RestoreItem {
                item: "Core".into(),
                status: RestoreStatus::Restored,
                detail: String::new()
            },
            &c
        )
        .is_none());
        let gate = TakeoverGate {
            state: GateState::Closed,
            reasons: vec!["foreign writer: Local\\AIEgressGuard is held".into()],
            ..Default::default()
        };
        assert!(for_gate(&gate, &c).unwrap().contains("AIEgressGuard"));
        assert!(for_gate(
            &TakeoverGate {
                state: GateState::Open,
                reasons: vec![],
                ..Default::default()
            },
            &c
        )
        .is_none());
        let s = for_settings(
            &[FieldError {
                path: "agent[0].egress.candidates".into(),
                message: "at least one egress candidate is required".into(),
            }],
            &c,
        )
        .unwrap();
        assert!(s.contains("agent[0].egress.candidates") && s.contains("credentialRef"));
    }

    #[test]
    fn copy_all_joins_everything_in_order() {
        let p = ProfileSet::default().agent.remove(0);
        let c = ctx(&p, false);
        let report = CheckReport {
            exit_ip: None,
            generated_at: "t".into(),
            mode: crate::profile::DetectionMode::Standard,
            proxy_port: 1,
            results: vec![
                check("G1", Status::Pass, None, &[]),
                check("G4", Status::Unknown, Some("x"), &[]),
            ],
        };
        let gate = TakeoverGate {
            state: GateState::Closed,
            reasons: vec!["r".into()],
            ..Default::default()
        };
        let text = all(Some(&report), &[], &[], Some(&gate), &c);
        assert_eq!(text.matches("\n---\n").count(), 1);
        assert!(text.find("接管门关闭").unwrap() < text.find("排查：G4").unwrap());
        assert_eq!(all(None, &[], &[], None, &c), "没有需要排查的问题。");
    }

    #[test]
    fn redaction() {
        let raw = r#"secret: "abc123" password=hunter2 token:xyz Authorization: Bearer eyJhbGci socks5://u1:p2@198.51.100.7:12324 user:pw@host credentialRef=c1 controller /version"#;
        let r = redact(raw, false);
        for leak in [
            "abc123", "hunter2", "xyz", "eyJhbGci", "u1:p2", "user:pw", "=c1",
        ] {
            assert!(!r.contains(leak), "{leak} leaked in {r}");
        }
        assert!(
            r.contains("socks5://***@198.51.100.7:12324") && r.contains("***@host"),
            "{r}"
        );
        assert!(
            r.contains("controller /version"),
            "ordinary text survives: {r}"
        );
        assert_eq!(
            redact("exit 198.51.100.186 loc=US, port 7893, v1.19.31", true),
            "exit 198.51.100.x loc=US, port 7893, v1.19.31"
        );
        let p = ProfileSet::default().agent.remove(0);
        let c = check("G1", Status::Fail, None, &[("x", "exit 198.51.100.186")]);
        assert!(for_check(&c, &ctx(&p, true))
            .unwrap()
            .contains("198.51.100.x"));
    }

    #[test]
    fn long_evidence_is_truncated_on_a_char_boundary() {
        let p = ProfileSet::default().agent.remove(0);
        let long = "证".repeat(2000);
        let c = check("G1", Status::Fail, None, &[("x", &long)]);
        let t = for_check(&c, &ctx(&p, false)).unwrap();
        assert!(t.contains("已截断"));
    }
}
