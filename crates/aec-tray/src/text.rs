//! Chinese wording for the tray: mode names, check names and the readable report file.

use aec_core::checks::{CheckReport, CheckResult, Kind, Status};
use aec_core::controller::{Mode, RestoreReport, RestoreStatus};
use aec_core::profile::Entry;

pub fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Daily => "日常模式",
        Mode::Entering => "正在进入 Agent 模式",
        Mode::AgentReady => "Agent 模式 · 检测通过",
        Mode::AgentUnverified => "Agent 模式 · 检测未全部通过",
        Mode::Exiting => "正在退出",
        Mode::RestoreIncomplete => "恢复未完成",
    }
}

pub fn status_label(s: Status) -> &'static str {
    match s {
        Status::Pass => "通过",
        Status::Warn => "注意",
        Status::Fail => "未通过",
        Status::Unknown => "未知",
    }
}

/// What a check id means, in one line.
pub fn check_label(id: &str) -> String {
    let (base, arg) = id.split_once(':').unwrap_or((id, ""));
    match base {
        "G1" => "出口 IP 与本机直连不同".into(),
        "G2" => format!("目标站点 {arg} 可达且走同一出口"),
        "G3" => "DNS 查询由内核代办（不经本机 DNS）".into(),
        "G4" => "系统时区与浏览器时区符合设置".into(),
        "G5" => format!("浏览器 {arg} 的语言符合设置"),
        "G6" => "系统代理指向本程序".into(),
        "R1" => "出口 IP 的网络类型（ASN）".into(),
        "R2" => "经出口的延迟".into(),
        "R3" => format!("浏览器 {arg} 的 WebRTC 不暴露本机 IP"),
        "R4" => format!("浏览器 {arg} 的 Canvas/WebGL 信息"),
        "R5" => format!("浏览器 {arg} 的 Cookie 开关"),
        "R9" => "非目标网站流量的出口".into(),
        "R10" => "时区由谁设置".into(),
        _ => id.into(),
    }
}

pub fn entry_label(e: &Entry) -> String {
    match e {
        Entry::Browser { browser, .. } => match browser {
            aec_core::profile::Browser::Edge => "打开 Edge".into(),
            aec_core::profile::Browser::Chrome => "打开 Chrome".into(),
        },
        Entry::Cli { command, .. } => match command.as_str() {
            "claude" => "打开 Claude Code（新终端）".into(),
            "codex" => "打开 Codex（新终端）".into(),
            other => format!("打开 {other}（新终端）"),
        },
    }
}

fn line(r: &CheckResult, out: &mut String) {
    out.push_str(&format!(
        "[{}] {}  {}{}\n",
        status_label(r.status),
        r.id,
        check_label(&r.id),
        r.reason
            .as_deref()
            .map(|x| format!("（{x}）"))
            .unwrap_or_default()
    ));
    for e in &r.evidence {
        out.push_str(&format!("      {}: {}\n", e.source, e.detail));
    }
}

/// One-line summary for a notification.
pub fn summary(report: &CheckReport) -> String {
    let hard: Vec<&CheckResult> = report
        .results
        .iter()
        .filter(|r| r.kind == Kind::Hard)
        .collect();
    let bad: Vec<String> = hard
        .iter()
        .filter(|r| r.status != Status::Pass)
        .map(|r| format!("{} {}", r.id, status_label(r.status)))
        .collect();
    if bad.is_empty() {
        format!("硬门槛 {} 项全部通过", hard.len())
    } else {
        format!("没通过：{}", bad.join("，"))
    }
}

/// The report file the tray opens.
pub fn report(mode: Mode, report: &CheckReport) -> String {
    let mut out = format!(
        "Agent 出口控制器 · 检测结果\n状态：{}\n检测时间（UTC）：{}\n出口 IP：{}\n代理端口：127.0.0.1:{}\n\n",
        mode_label(mode),
        report.generated_at,
        report.exit_ip.as_deref().unwrap_or("未确定"),
        report.proxy_port
    );
    out.push_str("硬门槛（全部通过才算就绪）\n");
    for r in report.results.iter().filter(|r| r.kind == Kind::Hard) {
        line(r, &mut out);
    }
    out.push_str("\n只读信号（供参考，不影响就绪）\n");
    for r in report.results.iter().filter(|r| r.kind != Kind::Hard) {
        line(r, &mut out);
    }
    out.push_str("\n看不懂哪一项，可以在托盘菜单点「复制排查提示词」，粘贴给任意 AI 助手。\n");
    out
}

pub fn restore_summary(r: &RestoreReport) -> String {
    let bad: Vec<String> = r
        .items
        .iter()
        .filter(|i| matches!(i.status, RestoreStatus::Failed | RestoreStatus::Conflict))
        .map(|i| format!("{}：{}", i.item, i.detail))
        .collect();
    let left: Vec<String> = r
        .items
        .iter()
        .filter(|i| i.status == RestoreStatus::LeftOpen && i.detail.contains("still open"))
        .map(|i| i.item.trim_start_matches("Entry:").to_string())
        .collect();
    let mut s = if bad.is_empty() {
        "已回到日常模式，改过的设置都已恢复。".to_string()
    } else {
        format!("有 {} 项没恢复：{}", bad.len(), bad.join("；"))
    };
    if !left.is_empty() {
        s.push_str(&format!(
            " 终端 {} 仍开着，请自己关闭（它已经连不上出口）。",
            left.join("、")
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::checks::Evidence;
    use aec_core::profile::DetectionMode;

    fn r(id: &str, kind: Kind, status: Status) -> CheckResult {
        CheckResult {
            id: id.into(),
            kind,
            status,
            reason: (status != Status::Pass).then(|| "why".into()),
            evidence: vec![Evidence {
                source: "src".into(),
                detail: "d".into(),
            }],
        }
    }

    #[test]
    fn report_groups_hard_and_read_only() {
        let rep = CheckReport {
            generated_at: "2026-10-11T00:00:00Z".into(),
            mode: DetectionMode::Standard,
            proxy_port: 17893,
            results: vec![
                r("G1", Kind::Hard, Status::Pass),
                r("G2:claude.ai", Kind::Hard, Status::Fail),
                r("R2", Kind::ReadOnly, Status::Warn),
            ],
            exit_ip: Some("198.51.100.7".into()),
        };
        let t = report(Mode::AgentUnverified, &rep);
        let hard_at = t.find("硬门槛").unwrap();
        let soft_at = t.find("只读信号").unwrap();
        assert!(t.find("[通过] G1").unwrap() > hard_at);
        assert!(t.find("[未通过] G2:claude.ai  目标站点 claude.ai").unwrap() < soft_at);
        assert!(t.find("[注意] R2").unwrap() > soft_at);
        assert_eq!(summary(&rep), "没通过：G2:claude.ai 未通过");
    }
}
