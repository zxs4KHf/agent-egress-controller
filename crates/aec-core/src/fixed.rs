//! Fixed exit identity is independent of the first hop. No probe uses the system proxy.
use crate::http::{HttpClient, Request, Route};
use crate::profile::AgentProfile;

pub const TRANSPORT: &str = "aec-relay-transport";
pub const FIRST_HOP: &str = "aec-first-hop";
pub const PROBE_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";

pub fn probe_port(ports: &crate::core::CorePorts) -> Option<u16> {
    ports
        .tunnel
        .checked_add(1)
        .filter(|p| !ports.all().contains(p) && *p != ports.relay)
}

pub fn path_member(profile: &AgentProfile, path: &str) -> Option<String> {
    if path == "direct" {
        return Some("DIRECT".into());
    }
    let t = profile.egress.tunnel.as_ref().filter(|t| t.enabled)?;
    if path == "tunnel-auto" {
        return Some(crate::core::config::TUNNEL_GROUP.into());
    }
    let n: usize = path.strip_prefix("tunnel-")?.parse().ok()?;
    (n > 0 && n <= t.servers.len()).then(|| format!("aec-tunnel-{n}"))
}

pub fn protects(profile: &AgentProfile, host: &str) -> bool {
    profile.egress.fixed_exit
        && profile.targets.iter().any(|t| {
            host.eq_ignore_ascii_case(&t.domain)
                || host
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", t.domain.to_ascii_lowercase()))
        })
}

/// Error messages deliberately omit response bodies and addresses.
pub fn verify(client: &dyn HttpClient, port: u16, expected: Option<&str>) -> Result<(), String> {
    let expected = expected
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        .ok_or("未配置精确出口 IP；保持阻断")?;
    let response = client
        .send(&Request::get(PROBE_URL, Route::Proxy(port)))
        .map_err(|_| "出口检测无证据；保持阻断")?;
    if response.status != 200 {
        return Err("出口检测未成功；保持阻断".into());
    }
    let actual = crate::providers::parse_cf_trace(&response.body)
        .ok()
        .and_then(|t| t.ip.parse::<std::net::IpAddr>().ok())
        .ok_or("出口检测无有效 IP；保持阻断")?;
    if actual != expected {
        return Err("出口 IP 与固定值不同；保持阻断".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpError, Response};
    struct Client(Result<Response, HttpError>);
    impl HttpClient for Client {
        fn send(&self, r: &Request) -> Result<Response, HttpError> {
            assert_eq!(r.route, Route::Proxy(17327));
            self.0.clone()
        }
    }
    #[test]
    fn only_exact_evidence_opens_gate() {
        for (body, pass) in [
            ("ip=198.51.100.7\n", true),
            ("ip=198.51.100.8\n", false),
            ("error password=secret", false),
        ] {
            let c = Client(Ok(Response {
                status: 200,
                body: body.into(),
                elapsed: std::time::Duration::ZERO,
            }));
            assert_eq!(verify(&c, 17327, Some("198.51.100.7")).is_ok(), pass);
        }
        assert!(verify(
            &Client(Err(HttpError::Timeout)),
            17327,
            Some("198.51.100.7")
        )
        .is_err());
    }
    #[test]
    fn probe_port_never_wraps_or_collides() {
        let mut p = crate::core::CorePorts {
            tunnel: u16::MAX,
            ..Default::default()
        };
        assert!(probe_port(&p).is_none());
        p.tunnel = p.relay - 1;
        assert!(probe_port(&p).is_none());
    }
}
