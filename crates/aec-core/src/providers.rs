//! Detection service contract (SPEC 4.6, ADR-0007): which endpoints are called, why, and how their
//! answers are read. The list is a user-editable file; a missing file means the built-in defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::{write_atomic, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parser {
    CfTrace,
    Ipwhois,
    IpsbGeoip,
    /// Not fetched by this application (e.g. a STUN server the browser contacts itself).
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// Address seen without any proxy, to prove the proxy changes something.
    DirectIp,
    /// Exit of traffic that is not a target (R9).
    OtherExit,
    /// ASN and country of the exit address (R1).
    Geo,
    /// STUN server the Agent browser's check page uses to learn what WebRTC reveals (R3).
    Stun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub purpose: Vec<Purpose>,
    /// `{ip}` is replaced by the exit address for geo lookups.
    pub url: String,
    pub parser: Parser,
    pub connect_timeout_ms: u32,
    pub total_timeout_ms: u32,
    pub docs_url: String,
    /// What the request reveals, in words, shown next to the result.
    pub sends: String,
    pub enabled: bool,
    pub third_party: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualLink {
    pub id: String,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSet {
    pub schema_version: u32,
    pub providers: Vec<Provider>,
    pub manual_links: Vec<ManualLink>,
}

impl Default for ProviderSet {
    fn default() -> Self {
        let p = |id: &str,
                 name: &str,
                 purpose: Vec<Purpose>,
                 url: &str,
                 parser,
                 docs: &str,
                 sends: &str| Provider {
            id: id.into(),
            name: name.into(),
            purpose,
            url: url.into(),
            parser,
            connect_timeout_ms: 3000,
            total_timeout_ms: 8000,
            docs_url: docs.into(),
            sends: sends.into(),
            enabled: true,
            third_party: true,
        };
        let link = |id: &str, name: &str, url: &str| ManualLink {
            id: id.into(),
            name: name.into(),
            url: url.into(),
        };
        Self {
            schema_version: 1,
            providers: vec![
                p(
                    "cf-trace",
                    "Cloudflare trace",
                    vec![Purpose::DirectIp, Purpose::OtherExit],
                    "https://www.cloudflare.com/cdn-cgi/trace",
                    Parser::CfTrace,
                    "https://developers.cloudflare.com/fundamentals/reference/cdn-cgi-endpoint/",
                    "only the request itself",
                ),
                p(
                    "ipwhois",
                    "ipwho.is",
                    vec![Purpose::Geo],
                    "https://ipwho.is/{ip}",
                    Parser::Ipwhois,
                    "https://ipwhois.io/documentation",
                    "the exit address in the path",
                ),
                p(
                    "ipsb",
                    "IP.SB",
                    vec![Purpose::Geo],
                    "https://api.ip.sb/geoip/{ip}",
                    Parser::IpsbGeoip,
                    "https://ip.sb/api/",
                    "the exit address in the path",
                ),
                p(
                    "stun-google",
                    "Google STUN",
                    vec![Purpose::Stun],
                    "stun:stun.l.google.com:19302",
                    Parser::None,
                    "https://webrtc.org/getting-started/peer-connections",
                    "a STUN binding request from the Agent browser over UDP, outside the proxy: the server sees the address WebRTC would reveal",
                ),
            ],
            manual_links: vec![
                link("netcoffee", "ip.net.coffee", "https://ip.net.coffee/"),
                link(
                    "netcoffee-claude",
                    "ip.net.coffee Claude",
                    "https://ip.net.coffee/claude/",
                ),
                link(
                    "netcoffee-dns",
                    "ip.net.coffee DNS",
                    "https://ip.net.coffee/dns/",
                ),
                link(
                    "netcoffee-webrtc",
                    "ip.net.coffee WebRTC",
                    "https://ip.net.coffee/webrtc/",
                ),
            ],
        }
    }
}

impl ProviderSet {
    pub fn usable(&self, purpose: Purpose, local_only: bool) -> impl Iterator<Item = &Provider> {
        self.providers.iter().filter(move |p| {
            p.enabled && p.purpose.contains(&purpose) && !(local_only && p.third_party)
        })
    }
}

pub struct ProviderStore {
    path: PathBuf,
}

impl ProviderStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("providers.json"),
        }
    }

    /// The stored list, or the defaults when there is no file yet.
    pub fn load(&self) -> Result<ProviderSet, StoreError> {
        match std::fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| StoreError::Parse {
                path: self.path.clone(),
                message: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProviderSet::default()),
            Err(e) => Err(StoreError::Io {
                path: self.path.clone(),
                source: e,
            }),
        }
    }

    pub fn save(&self, set: &ProviderSet) -> Result<(), StoreError> {
        write_atomic(
            &self.path,
            &serde_json::to_vec_pretty(set).expect("serializes"),
        )
    }
}

// ---- parsers -------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    pub ip: String,
    pub loc: Option<String>,
    pub colo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geo {
    pub asn: Option<u32>,
    pub org: Option<String>,
    pub country_code: Option<String>,
}

/// `key=value` lines from Cloudflare's `/cdn-cgi/trace`.
pub fn parse_cf_trace(body: &str) -> Result<Trace, String> {
    let get = |k: &str| {
        body.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
            .map(|v| v.trim().to_string())
    };
    let ip = get("ip")
        .filter(|ip| ip.parse::<std::net::IpAddr>().is_ok())
        .ok_or("no valid ip= line")?;
    Ok(Trace {
        ip,
        loc: get("loc").filter(|s| !s.is_empty()),
        colo: get("colo").filter(|s| !s.is_empty()),
    })
}

pub fn parse_ipwhois(body: &str) -> Result<Geo, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if v.get("success").and_then(|s| s.as_bool()) == Some(false) {
        return Err(format!(
            "success=false: {}",
            v.get("message").and_then(|m| m.as_str()).unwrap_or("")
        ));
    }
    Ok(Geo {
        asn: v
            .pointer("/connection/asn")
            .and_then(|a| a.as_u64())
            .map(|a| a as u32),
        org: v
            .pointer("/connection/org")
            .or_else(|| v.pointer("/connection/isp"))
            .and_then(|s| s.as_str())
            .map(str::to_string),
        country_code: v
            .get("country_code")
            .and_then(|s| s.as_str())
            .map(|s| s.to_ascii_uppercase()),
    })
}

pub fn parse_ipsb(body: &str) -> Result<Geo, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if v.get("ip").is_none() {
        return Err("no ip field".into());
    }
    Ok(Geo {
        asn: v.get("asn").and_then(|a| a.as_u64()).map(|a| a as u32),
        org: v
            .get("asn_organization")
            .or_else(|| v.get("organization"))
            .and_then(|s| s.as_str())
            .map(str::to_string),
        country_code: v
            .get("country_code")
            .and_then(|s| s.as_str())
            .map(|s| s.to_ascii_uppercase()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Response shapes captured on 2026-10-10 (docs/RESEARCH-MATURE-SOLUTIONS.md), test address 1.1.1.1.
    const CF: &str = "fl=763f75\nh=1.1.1.1\nip=203.0.113.24\nts=1791691178.000\nvisit_scheme=https\ncolo=NRT\nloc=JP\ntls=TLSv1.3\n";
    const IPWHOIS: &str = r#"{"ip":"1.1.1.1","success":true,"type":"IPv4","country_code":"AU","connection":{"asn":13335,"org":"Apnic Research And Development","isp":"Cloudflare, Inc."}}"#;
    const IPSB: &str = r#"{"organization":"Cloudflare","isp":"Cloudflare","asn_organization":"Cloudflare, Inc.","asn":13335,"ip":"1.1.1.1","country_code":"AU"}"#;

    #[test]
    fn parses_cf_trace() {
        let t = parse_cf_trace(CF).unwrap();
        assert_eq!(
            (t.ip.as_str(), t.loc.as_deref(), t.colo.as_deref()),
            ("203.0.113.24", Some("JP"), Some("NRT"))
        );
        assert!(parse_cf_trace("h=x\nloc=US\n").is_err(), "missing ip");
        assert!(parse_cf_trace("ip=not-an-ip\n").is_err());
        assert!(parse_cf_trace("<html>blocked</html>").is_err());
        assert_eq!(parse_cf_trace("ip=2001:db8::1\n").unwrap().loc, None);
    }

    #[test]
    fn parses_geo_services() {
        let a = parse_ipwhois(IPWHOIS).unwrap();
        assert_eq!(
            (a.asn, a.country_code.as_deref()),
            (Some(13335), Some("AU"))
        );
        let b = parse_ipsb(IPSB).unwrap();
        assert_eq!(
            (b.asn, b.org.as_deref(), b.country_code.as_deref()),
            (Some(13335), Some("Cloudflare, Inc."), Some("AU"))
        );
        assert!(parse_ipwhois(r#"{"success":false,"message":"rate limit"}"#)
            .unwrap_err()
            .contains("rate limit"));
        assert!(parse_ipsb("Forbidden").is_err());
        assert!(parse_ipsb(r#"{"error":"x"}"#).is_err());
        let partial = parse_ipwhois(r#"{"success":true,"country_code":"us"}"#).unwrap();
        assert_eq!(
            (partial.asn, partial.country_code.as_deref()),
            (None, Some("US"))
        );
    }

    #[test]
    fn local_only_filters_third_parties() {
        let s = ProviderSet::default();
        assert_eq!(s.usable(Purpose::Geo, false).count(), 2);
        assert_eq!(s.usable(Purpose::Geo, true).count(), 0);
        assert_eq!(s.usable(Purpose::DirectIp, true).count(), 0);
    }

    #[test]
    fn default_set_round_trips_and_rejects_unknown_fields() {
        let s = ProviderSet::default();
        let back: ProviderSet = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let mut v = serde_json::to_value(&s).unwrap();
        v["providers"][0]["cookie"] = "x".into();
        assert!(serde_json::from_value::<ProviderSet>(v).is_err());
    }
}
