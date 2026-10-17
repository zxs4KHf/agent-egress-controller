//! Evidence is deliberately separated from persistence. The mihomo API cannot prove a
//! HTTP node's endpoint or read back DNS policy. Missing evidence never becomes a pass.
use super::{Selection, OUTBOUND};
use serde_json::{json, Value};
pub trait RuntimeHost {
    /// Only existing authenticated loopback controller reads; no arbitrary URL or mutations.
    fn get(&self, path: &str) -> Result<Value, &'static str>;
}

/// Live entry evidence is distinct from AEC's internal identity and DNS probes.
/// This API cannot prove the third-party node endpoint/DNS policy, so a matching
/// entry/ruleset remains unknown rather than turning the whole environment green.
pub fn scope_check(
    host: &impl RuntimeHost,
    selection: &Selection,
    system: &crate::model::ProxySettings,
) -> crate::checks::CheckResult {
    use crate::checks::{CheckResult, Kind, Status};
    let result = (|| {
        if !system.proxy_enabled() || system.pac_enabled() || system.auto_detect() {
            return Err((
                Status::Fail,
                "cooperation_system_proxy_disabled_or_automatic",
            ));
        }
        let config = host
            .get("/configs")
            .map_err(|_| (Status::Unknown, "cooperation_controller_unavailable"))?;
        let port = config["mixed-port"]
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .filter(|n| *n != 0)
            .ok_or((Status::Unknown, "cooperation_listener_unknown"))?;
        let expected = format!("127.0.0.1:{port}");
        let server = system.proxy_server.as_deref().unwrap_or("");
        if server != expected && server != format!("http={expected};https={expected}") {
            return Err((Status::Fail, "cooperation_system_proxy_bypassed"));
        }
        // CVR normally installs private-address bypasses. Arbitrary hostname patterns
        // need separate evidence; never ignore a bypass that might skip selected sites.
        if system.bypass.as_deref().is_some_and(|b| {
            b.split(';').any(|s| {
                let s = s.trim();
                !s.is_empty()
                    && !matches!(
                        s,
                        "<local>"
                            | "<-loopback>"
                            | "localhost"
                            | "127.*"
                            | "10.*"
                            | "192.168.*"
                            | "::1"
                            | "[::1]"
                    )
                    && s.parse::<std::net::IpAddr>().is_err()
                    && !(16..=31).any(|n| s == format!("172.{n}.*"))
            })
        }) {
            return Err((Status::Unknown, "cooperation_proxy_bypass_unverified"));
        }
        let evidence = verify_runtime(host, selection);
        if evidence["rules"] != "pass" {
            return Err(
                if matches!(
                    evidence["reason"].as_str(),
                    Some(
                        "COOP_CLIENT_NOT_RULE_MODE"
                            | "COOP_RUNTIME_NODE_MISSING"
                            | "COOP_RUNTIME_RULES_MISMATCH"
                    )
                ) {
                    (Status::Fail, "cooperation_rules_missing_or_changed")
                } else {
                    (Status::Unknown, "cooperation_controller_unavailable")
                },
            );
        }
        Ok(())
    })();
    match result {
        Err((status, reason)) => CheckResult::new("G7", Kind::Hard, status, Some(reason)),
        Ok(()) => CheckResult::new(
            "G7",
            Kind::Hard,
            Status::Unknown,
            Some("cooperation_endpoint_dns_unverified"),
        )
        .with(
            "system entry",
            "当前系统代理指向客户端的本机入口；当前规则模式与选定域名规则顺序匹配",
        )
        .with(
            "scope",
            "仅覆盖遵循该入口的新连接；关闭系统代理、Global/Direct、旧连接及独立代理实例不在此范围",
        )
        .with(
            "missing evidence",
            "客户端 API 不回显实际 HTTP 节点端点与 DNS policy；AEC 内部固定 IP 通过不代替这两项",
        ),
    }
}
pub fn verify_runtime(host: &impl RuntimeHost, selection: &Selection) -> Value {
    let result = (|| {
        let config = host.get("/configs")?;
        if config["mode"].as_str() != Some("rule") {
            return Err("COOP_CLIENT_NOT_RULE_MODE");
        }
        let proxies = host.get("/proxies")?;
        if proxies["proxies"][OUTBOUND]["type"].as_str() != Some("Http") {
            return Err("COOP_RUNTIME_NODE_MISSING");
        }
        let rules = host.get("/rules")?;
        let rules = rules["rules"]
            .as_array()
            .ok_or("COOP_RUNTIME_RULES_UNKNOWN")?;
        let n = selection.domains.len();
        if rules.len() < n * 2 || n == 0 {
            return Err("COOP_RUNTIME_RULES_MISMATCH");
        }
        for (i, domain) in selection.domains.iter().enumerate() {
            let udp = &rules[i];
            let tcp = &rules[i + n];
            let expected = format!("((DomainSuffix,{domain}) && (Network,udp))");
            if udp["type"].as_str() != Some("AND")
                || udp["payload"].as_str() != Some(&expected)
                || udp["proxy"].as_str() != Some("REJECT")
                || udp["extra"]["disabled"].as_bool() == Some(true)
                || tcp["type"].as_str() != Some("DomainSuffix")
                || tcp["payload"].as_str() != Some(domain)
                || tcp["proxy"].as_str() != Some(OUTBOUND)
                || tcp["extra"]["disabled"].as_bool() == Some(true)
            {
                return Err("COOP_RUNTIME_RULES_MISMATCH");
            }
        }
        Ok::<(), &'static str>(())
    })();
    json!({"rules":if result.is_ok(){"pass"}else{"unknown"},"reason":result.err(),"runtimeVerified":false,
        "endpoint":"unknown","fixedIdentity":"unknown","dnsPolicy":"unknown","coverage":"current_client_rule_mode_new_connections",
        "message":"仅核对有序规则与 HTTP 节点类型；API 不回显节点地址或 DNS 策略，不能据此声称整机或浏览器已保护。"})
}

fn nonce_valid(nonce: &str) -> bool {
    nonce.len() == 8 && nonce.bytes().all(|b| b.is_ascii_hexdigit())
}
/// Decode bounded DNS TXT presentation strings, including quoted chunks and decimal escapes.
fn txt(input: &str) -> Option<String> {
    let mut bytes = input.as_bytes().iter().copied().peekable();
    let mut out = Vec::new();
    while bytes.peek().is_some() {
        while bytes.peek().is_some_and(u8::is_ascii_whitespace) {
            bytes.next();
        }
        if bytes.peek().is_none() {
            break;
        }
        if bytes.next() != Some(b'"') {
            return None;
        }
        loop {
            match bytes.next()? {
                b'"' => break,
                b'\\' => {
                    let b = bytes.next()?;
                    if b.is_ascii_digit() {
                        let c = bytes.next()?;
                        let d = bytes.next()?;
                        if !c.is_ascii_digit() || !d.is_ascii_digit() {
                            return None;
                        }
                        let n =
                            (b - b'0') as u16 * 100 + (c - b'0') as u16 * 10 + (d - b'0') as u16;
                        out.push(u8::try_from(n).ok()?);
                    } else {
                        out.push(b);
                    }
                }
                b => out.push(b),
            }
            if out.len() > 2048 {
                return None;
            }
        }
    }
    String::from_utf8(out).ok()
}
/// dnscheck.tools TXT is an authority-generated diagnostic, NOT a browser-wide safety score.
pub fn evaluate_dns(reply: &Value, nonce: &str, elapsed_ms: u64) -> Value {
    let scope = "single_authority_txt_query";
    let unknown =
        || json!({"status":"unknown","code":"DNS_AUTHORITY_EVIDENCE_UNAVAILABLE","scope":scope});
    if !nonce_valid(nonce)
        || elapsed_ms > 10_000
        || reply["Status"].as_u64() != Some(0)
        || reply["TC"].as_bool() != Some(false)
    {
        return unknown();
    }
    let expected = format!("{}.test.dnscheck.tools.", nonce.to_ascii_lowercase());
    let Some(questions) = reply["Question"].as_array().filter(|q| q.len() == 1) else {
        return unknown();
    };
    let q = &questions[0];
    let (name, kind) = if q.get("Name").is_some() {
        (&q["Name"], &q["Qtype"])
    } else {
        (&q["name"], &q["type"])
    };
    if !name
        .as_str()
        .is_some_and(|n| n.eq_ignore_ascii_case(&expected))
        || kind.as_u64() != Some(16)
    {
        return unknown();
    }
    let Some(answers) = reply["Answer"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 32)
    else {
        return unknown();
    };
    let mut from = 0;
    let mut id = 0;
    let mut edns = false;
    let mut ecs = None;
    for a in answers {
        if a["type"].as_u64() != Some(16) {
            continue;
        }
        if !a["name"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(&expected))
            || a["TTL"].as_u64() != Some(1)
        {
            return unknown();
        }
        let Some(text) = a["data"].as_str().and_then(txt) else {
            return unknown();
        };
        if let Some(value) = text.strip_prefix("FROM: ") {
            let Some(endpoint) = value.split_whitespace().next() else {
                return unknown();
            };
            let Some((ip, port)) = endpoint.rsplit_once('#') else {
                return unknown();
            };
            if ip.parse::<std::net::IpAddr>().is_err() || port.parse::<u16>().is_err() {
                return unknown();
            }
            from += 1;
        } else if let Some(value) = text.strip_prefix("ID: ") {
            if value.parse::<u16>().is_err() {
                return unknown();
            }
            id += 1;
        } else if text.starts_with("EDNS: version: ") {
            edns = true;
        } else if let Some(value) = text.strip_prefix("ECS: ") {
            let Some((ip, bits)) = value
                .split_whitespace()
                .next()
                .and_then(|v| v.split_once('/'))
            else {
                return unknown();
            };
            let Ok(ip) = ip.parse::<std::net::IpAddr>() else {
                return unknown();
            };
            let Ok(bits) = bits.parse::<u8>() else {
                return unknown();
            };
            if bits > if ip.is_ipv4() { 32 } else { 128 } || ecs.is_some() {
                return unknown();
            }
            ecs = Some(bits);
        }
    }
    if from != 1 || id != 1 {
        return unknown();
    }
    json!({"status":if ecs.is_some_and(|bits|bits>0){"warn"}else{"pass"},"scope":scope,"authoritySourceObserved":true,
        "ednsObserved":edns,"ecsObserved":ecs.is_some(),"ecsPrefixLength":ecs,"elapsedMs":elapsed_ms,
        "code":if ecs.is_some_and(|bits|bits>0){"DNS_ECS_SUBNET_OBSERVED"}else{"DNS_QUERY_NO_IDENTIFYING_ECS"},
        "limitations":"仅第三方权威 TXT 对此次随机查询的回证；不是所有 Claude DNS、浏览器、IPv6 或整机无泄露证明。递归器地址不等于 ISP 出口。"})
}
pub fn probe_fixed_dns(http: &impl crate::http::HttpClient, port: u16, nonce: &str) -> Value {
    let unknown = || json!({"status":"unknown","code":"DNS_AUTHORITY_EVIDENCE_UNAVAILABLE","scope":"fixed_ingress_explicit_google_doh"});
    if !nonce_valid(nonce) || port == 0 {
        return unknown();
    }
    let request=crate::http::Request::get(format!("https://dns.google/resolve?name={nonce}.test.dnscheck.tools&type=TXT&edns_client_subnet=0.0.0.0/0"),crate::http::Route::Proxy(port));
    let Ok(response) = http.send(&request) else {
        return unknown();
    };
    if response.status != 200 {
        return unknown();
    }
    let Ok(reply) = serde_json::from_str(&response.body) else {
        return unknown();
    };
    let mut evidence = evaluate_dns(&reply, nonce, response.elapsed.as_millis() as u64);
    evidence["scope"] = "fixed_ingress_explicit_google_doh".into();
    evidence
}
