//! Local proxy management uses existing routing data, never a replacement routing preset.
use crate::{core::CorePorts, profile::*};
use serde::{Deserialize, Serialize};

pub const GROUP: &str = "本机代理上游";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Save {
        name: String,
        host: String,
        port: u16,
        protocol: UpstreamProto,
    },
    Select {
        name: String,
    },
    Delete {
        name: String,
    },
    Probe {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Endpoint {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub protocol: UpstreamProto,
}

#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub entries: Vec<Endpoint>,
    pub selected: Option<String>,
    pub managed: bool,
    pub bound: bool,
}

fn local(o: &RoutingOutbound) -> bool {
    matches!(o.server.as_str(), "127.0.0.1" | "localhost" | "::1")
        && matches!(
            o.kind,
            RoutingOutboundKind::Http | RoutingOutboundKind::Socks5
        )
        && o.credential_ref.is_none()
        && o.dialer_proxy.is_none()
}

fn eligible(r: &Routing, o: &RoutingOutbound) -> bool {
    fn includes(r: &Routing, root: &str, name: &str, depth: usize) -> bool {
        root == name
            || (depth <= r.groups.len()
                && r.groups
                    .iter()
                    .find(|g| g.name == root)
                    .is_some_and(|g| g.members.iter().any(|m| includes(r, m, name, depth + 1))))
    }
    local(o)
        && !r
            .outbounds
            .iter()
            .filter_map(|other| other.dialer_proxy.as_deref())
            .any(|root| includes(r, root, &o.name, 0))
}

fn referenced(r: &Routing, name: &str) -> bool {
    r.final_target == name
        || r.rules.iter().any(|r| r.target == name)
        || r.groups
            .iter()
            .filter(|g| g.name != GROUP)
            .any(|g| g.members.iter().any(|m| m == name))
}

pub fn view(set: &ProfileSet) -> View {
    let r = &set.agent[0].routing;
    let entries: Vec<_> = r
        .outbounds
        .iter()
        .filter(|o| eligible(r, o))
        .map(|o| Endpoint {
            name: o.name.clone(),
            host: o.server.clone(),
            port: o.port,
            protocol: if o.kind == RoutingOutboundKind::Http {
                UpstreamProto::Http
            } else {
                UpstreamProto::Socks5
            },
        })
        .collect();
    let g = r.groups.iter().find(|g| g.name == GROUP);
    let referenced: Vec<_> = entries.iter().filter(|e| referenced(r, &e.name)).collect();
    View {
        selected: g
            .and_then(|g| g.selected.clone().or_else(|| g.members.first().cloned()))
            .or_else(|| (referenced.len() == 1).then(|| referenced[0].name.clone())),
        managed: g.is_some(),
        bound: if g.is_some() {
            self::referenced(r, GROUP)
        } else {
            !referenced.is_empty()
        },
        entries,
    }
}

fn ensure_group(set: &mut ProfileSet) -> Result<(), String> {
    let v = view(set);
    let r = &mut set.agent[0].routing;
    if let Some(g) = r.groups.iter().find(|g| g.name == GROUP) {
        if g.kind != RoutingGroupKind::Select
            || !g.switchable
            || g.members
                .iter()
                .any(|m| !v.entries.iter().any(|e| &e.name == m))
        {
            return Err("UPSTREAM_GROUP_CONFLICT: 已存在不兼容的本机上游组".into());
        }
        return Ok(());
    }
    if r.outbounds.iter().any(|o| o.name == GROUP) {
        return Err("UPSTREAM_GROUP_CONFLICT: 上游组名称已被占用".into());
    }
    let refs: Vec<_> = v
        .entries
        .iter()
        .filter(|e| referenced(r, &e.name))
        .collect();
    if refs.len() > 1 {
        return Err("UPSTREAM_AMBIGUOUS: 现有规则分别引用多个本机上游；请先在规则面板统一普通上游，避免合并不同用途".into());
    }
    let selected = refs
        .first()
        .or(v.entries.first().as_ref())
        .map(|e| e.name.clone());
    if let Some(old) = refs.first().map(|e| &e.name) {
        for rule in &mut r.rules {
            if &rule.target == old {
                rule.target = GROUP.into();
            }
        }
        if &r.final_target == old {
            r.final_target = GROUP.into();
        }
        for group in &mut r.groups {
            for member in &mut group.members {
                if member == old {
                    *member = GROUP.into();
                }
            }
            if group.selected.as_ref() == Some(old) {
                group.selected = Some(GROUP.into());
            }
        }
    }
    r.groups.push(RoutingGroup {
        name: GROUP.into(),
        kind: RoutingGroupKind::Select,
        check: None,
        members: v.entries.iter().map(|e| e.name.clone()).collect(),
        switchable: true,
        selected,
    });
    Ok(())
}

pub fn plan(set: &ProfileSet, command: &Command, ports: CorePorts) -> Result<ProfileSet, String> {
    if set.agent.is_empty() {
        return Err("UPSTREAM_PROFILE_MISSING".into());
    }
    let mut next = set.clone();
    ensure_group(&mut next)?;
    let r = &mut next.agent[0].routing;
    match command {
        Command::Save {
            name,
            host,
            port,
            protocol,
        } => {
            if name.is_empty()
                || name.len() > 64
                || name.trim() != name
                || name
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '<' | '>' | '&' | '\"' | '\''))
                || [GROUP, "relay", "DIRECT", "REJECT"].contains(&name.as_str())
                || name.starts_with("aec-")
                || r.groups.iter().any(|g| &g.name == name)
            {
                return Err(
                    "UPSTREAM_NAME: 名称需为 1–64 字节且不与现有组或系统保留名称重复".into(),
                );
            }
            if !["127.0.0.1", "localhost", "::1"].contains(&host.as_str()) {
                return Err("UPSTREAM_HOST: 仅允许本机 127.0.0.1、localhost 或 ::1".into());
            }
            if *port == 0
                || [
                    ports.mixed,
                    ports.controller,
                    ports.dns,
                    ports.relay,
                    ports.tunnel,
                    17980,
                ]
                .contains(port)
                || crate::fixed::probe_port(&ports) == Some(*port)
                || crate::fixed::ingress_port(&ports) == Some(*port)
            {
                return Err("UPSTREAM_PORT: 端口不能为 0 或 AEC 自己的入口、控制、DNS、relay、隧道及检测端口".into());
            }
            let index = r.outbounds.iter().position(|o| &o.name == name);
            if index.is_some_and(|i| !eligible(r, &r.outbounds[i])) {
                return Err("UPSTREAM_OWNERSHIP: 不能覆盖远程、认证或链路传输出站".into());
            }
            if r.outbounds.iter().any(|o| {
                &o.name != name
                    && local(o)
                    && o.port == *port
                    && (o.server == *host || (o.server != "::1" && host != "::1"))
            }) {
                return Err(
                    "UPSTREAM_DUPLICATE: 同一个本机端口已存在，无需重复添加 HTTP/SOCKS5 别名"
                        .into(),
                );
            }
            let o = RoutingOutbound {
                name: name.clone(),
                server: if host == "localhost" {
                    "127.0.0.1".into()
                } else {
                    host.clone()
                },
                port: *port,
                kind: if *protocol == UpstreamProto::Http {
                    RoutingOutboundKind::Http
                } else {
                    RoutingOutboundKind::Socks5
                },
                credential_ref: None,
                cipher: None,
                dialer_proxy: None,
            };
            if let Some(i) = index {
                r.outbounds[i] = o;
            } else {
                if r.groups
                    .iter()
                    .find(|g| g.name == GROUP)
                    .unwrap()
                    .members
                    .len()
                    >= 16
                {
                    return Err("UPSTREAM_LIMIT: 最多 16 个本机上游".into());
                }
                r.outbounds.push(o);
            }
            let g = r.groups.iter_mut().find(|g| g.name == GROUP).unwrap();
            if !g.members.contains(name) {
                g.members.push(name.clone());
            }
            if g.selected.is_none() {
                g.selected = Some(name.clone());
            }
        }
        Command::Select { name } => {
            let g = r.groups.iter_mut().find(|g| g.name == GROUP).unwrap();
            if !g.members.contains(name) {
                return Err("UPSTREAM_NOT_FOUND: 请选择已保存的本机上游".into());
            }
            g.selected = Some(name.clone());
        }
        Command::Delete { name } => {
            let g = r.groups.iter().find(|g| g.name == GROUP).unwrap();
            if g.selected.as_ref().or(g.members.first()) == Some(name) || referenced(r, name) {
                return Err("UPSTREAM_IN_USE: 请先切换到其他上游，并移除直接引用后再删除".into());
            }
            if !g.members.contains(name) {
                return Err("UPSTREAM_NOT_FOUND".into());
            }
            r.outbounds.retain(|o| &o.name != name);
            r.groups
                .iter_mut()
                .find(|g| g.name == GROUP)
                .unwrap()
                .members
                .retain(|m| m != name);
        }
        Command::Probe { .. } => return Err("UPSTREAM_READ_ONLY: 检测不修改配置".into()),
    }
    if crate::profile::validate(&next)
        .iter()
        .any(|e| e.path.contains("routing"))
    {
        return Err("UPSTREAM_INVALID: 路由校验失败，原配置未修改".into());
    }
    if view(&next).entries.iter().any(|e| {
        e.port == 0
            || [
                ports.mixed,
                ports.controller,
                ports.dns,
                ports.relay,
                ports.tunnel,
                ports.tunnel.saturating_add(1),
                17980,
            ]
            .contains(&e.port)
    }) {
        return Err("UPSTREAM_PORT: 存在与 AEC 当前端口冲突的上游，请先修改端口".into());
    }
    Ok(next)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub status: &'static str,
    pub code: &'static str,
    pub message: &'static str,
    pub checked_at_unix_ms: u128,
    pub elapsed_ms: u128,
    pub http_status: u16,
}

/// Fixed, cookie-free HTTPS probe. No arbitrary URL, credentials, user curlrc, redirects or direct fallback.
pub fn probe(set: &ProfileSet, name: &str, ports: CorePorts) -> Result<Probe, String> {
    let entry = view(set)
        .entries
        .into_iter()
        .find(|e| e.name == name)
        .ok_or("UPSTREAM_NOT_FOUND")?;
    // Apply the same endpoint checks even to imported profiles before contacting a local service.
    plan(
        set,
        &Command::Save {
            name: entry.name.clone(),
            host: entry.host.clone(),
            port: entry.port,
            protocol: entry.protocol,
        },
        ports,
    )?;
    let started = std::time::Instant::now();
    let host = if entry.host == "::1" {
        "[::1]"
    } else {
        "127.0.0.1"
    };
    let scheme = if entry.protocol == UpstreamProto::Http {
        "http"
    } else {
        "socks5h"
    };
    let mut command = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .args([
            "-q",
            "--socks5-basic",
            "--silent",
            "--noproxy",
            "",
            "--proxy",
            &format!("{scheme}://{host}:{}", entry.port),
            "--connect-timeout",
            "3",
            "--max-time",
            "8",
            "--max-filesize",
            "65536",
            "--output",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
            "--write-out",
            "%{http_code} %{http_connect}",
            "--user-agent",
            crate::http::USER_AGENT,
            "https://www.cloudflare.com/cdn-cgi/trace",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let mut http_status = 0;
    let (status, code, message) = match output {
        Err(_) => (
            "unknown",
            "UPSTREAM_PROBE_UNAVAILABLE",
            "无法启动系统 curl，未取得网络证据",
        ),
        Ok(out) => {
            let body = String::from_utf8_lossy(&out.stdout);
            let mut fields = body.split_whitespace();
            http_status = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let connect_status = fields
                .next()
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(0);
            if connect_status == 407 || http_status == 407 {
                (
                    "fail",
                    "UPSTREAM_AUTH_REQUIRED",
                    "代理要求认证；当前管理器仅支持免认证本机端口",
                )
            } else if out.status.success() && http_status == 200 {
                (
                    "pass",
                    "UPSTREAM_HTTPS_OK",
                    "通过该代理完成测试站 HTTPS 请求；不代表 GPT 登录、所有网站或固定 ISP 已验证",
                )
            } else if out.status.code() == Some(7) {
                (
                    "fail",
                    "UPSTREAM_CONNECT_FAILED",
                    "无法连接本机代理端口，请启动对应客户端并检查端口",
                )
            } else if out.status.code() == Some(28) {
                (
                    "unknown",
                    "UPSTREAM_TIMEOUT",
                    "代理或测试站超时，未取得可用证据；未切换出口",
                )
            } else {
                (
                    "unknown",
                    "UPSTREAM_HTTPS_UNVERIFIED",
                    "代理协议、TLS 或测试站响应未通过；请核对 HTTP/SOCKS5 和客户端节点",
                )
            }
        }
    };
    Ok(Probe {
        status,
        code,
        message,
        http_status,
        elapsed_ms: started.elapsed().as_millis(),
        checked_at_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    })
}
