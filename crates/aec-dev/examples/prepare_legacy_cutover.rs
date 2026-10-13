//! Maintainer-only, explicit, isolated preparation. Does not stop legacy or touch global settings.
//! Usage: cargo run -p aec-dev --example prepare_legacy_cutover -- <old config.yaml> <old upstreams.txt> <old core dir> <new empty portable dir> <old mixed port>
//! Never prints source YAML, credentials, addresses, control tokens or mihomo diagnostics.
use std::collections::HashSet;
use std::net::IpAddr;
use std::path::Path;
use std::process::ExitCode;

use aec_core::core::{config::CredentialSource, process::CoreManager, CorePorts};
use aec_core::credentials::WindowsCredentials;
use aec_core::http::{HttpClient, Request, Route, WinHttpClient};
use aec_core::profile::{self, Candidate, ProfileSet, ProxyScopeMode, Tunnel, UpstreamProto};
use aec_core::store::ProfileStore;
use aec_core::windows::WindowsHost;

fn source_candidates(text: &str) -> Result<(String, Vec<Candidate>, String, String), String> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut host: Option<String> = None;
    let mut credential: Option<(String, String)> = None;
    let mut unique = HashSet::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let token = line.split_whitespace().next().ok_or("invalid candidate")?;
        let parts: Vec<_> = token.split(':').collect();
        if parts.len() != 5 {
            return Err("unsupported candidate shape: expected authenticated IPv4 host:port:proto:user:pass".into());
        }
        let addr: IpAddr = parts[0].parse().map_err(|_| "source exit is not one IP")?;
        if !addr.is_ipv4() {
            return Err("IPv6 source candidates need manual migration".into());
        }
        if host.as_deref().is_some_and(|h| h != parts[0]) {
            return Err("multiple exit hosts in candidate pool".into());
        }
        host = Some(parts[0].into());
        let port: u16 = parts[1].parse().map_err(|_| "bad candidate port")?;
        if port == 0 {
            return Err("zero candidate port".into());
        }
        let proto = match parts[2] {
            "http" => UpstreamProto::Http,
            "socks5" => UpstreamProto::Socks5,
            _ => return Err("unsupported candidate protocol".into()),
        };
        if parts[3].is_empty() || parts[4].is_empty() {
            return Err("source candidate has no login".into());
        }
        if credential
            .as_ref()
            .is_some_and(|c| c.0 != parts[3] || c.1 != parts[4])
        {
            return Err("source candidates have different credentials".into());
        }
        credential = Some((parts[3].into(), parts[4].into()));
        if unique.insert((port, parts[2].to_string())) {
            candidates.push(Candidate {
                id: format!("source-{}", candidates.len() + 1),
                host: parts[0].into(),
                port,
                proto,
                credential_ref: Some("exit".into()),
            });
        }
    }
    let (user, pass) = credential.ok_or("no source candidates")?;
    Ok((host.ok_or("no source host")?, candidates, user, pass))
}

fn observed_ip(port: u16) -> Result<String, String> {
    let response = WinHttpClient
        .send(&Request::get(
            "https://claude.ai/cdn-cgi/trace",
            Route::Proxy(port),
        ))
        .map_err(|_| "source trace unavailable")?;
    if response.status != 200 {
        return Err("source trace did not return success".into());
    }
    let ip = aec_core::providers::parse_cf_trace(&response.body)
        .map_err(|_| "source trace contains no valid IP")?
        .ip;
    Ok(ip)
}

fn prepare(args: &[String]) -> Result<(), String> {
    if args.len() != 5 {
        return Err(
            "expected: old-config old-upstreams old-core-dir new-empty-portable-dir old-mixed-port"
                .into(),
        );
    }
    let config = std::fs::read_to_string(&args[0]).map_err(|_| "cannot read old config")?;
    let upstreams = std::fs::read_to_string(&args[1]).map_err(|_| "cannot read old candidates")?;
    let (host, candidates, user, pass) = source_candidates(&upstreams)?;
    let old_port: u16 = args[4].parse().map_err(|_| "bad source port")?;
    let expected = observed_ip(old_port)?;
    if expected != host {
        return Err(
            "observed legacy exit differs from its configured fixed host; manual review required"
                .into(),
        );
    }
    let namespace = aec_core::crypto::random_hex(8).map_err(|_| "no random source")?;
    let mut plan = aec_core::import::parse(&config, &namespace)?;
    plan.verify(&config)?;
    let source: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&config).map_err(|_| "source YAML invalid")?;
    let source_relay = source["proxies"]
        .as_sequence()
        .ok_or("source proxies missing")?
        .iter()
        .filter(|n| n["server"].as_str() == Some("127.0.0.1") && n["port"].as_u64() == Some(12325))
        .count();
    if source_relay != 1 {
        return Err("expected exactly one legacy loopback relay outbound".into());
    }
    let preset = &aec_core::tunnel::PRESETS[0];
    let known: HashSet<(String, u16)> = preset
        .servers
        .iter()
        .map(|(h, p)| (h.to_string(), *p))
        .collect();
    let ss: Vec<_> = source["proxies"]
        .as_sequence()
        .unwrap()
        .iter()
        .filter(|n| n["type"].as_str() == Some("ss"))
        .collect();
    if ss.is_empty()
        || ss.iter().any(|n| {
            let key = (
                n["server"].as_str().unwrap_or("").to_string(),
                n["port"].as_u64().unwrap_or(0) as u16,
            );
            !known.contains(&key)
                || n["cipher"].as_str() != Some(preset.cipher)
                || n["password"].as_str() != Some(preset.password)
        })
    {
        return Err("source SS nodes do not match official preset; manual review required".into());
    }
    let mut routing = plan.routing.clone();
    let fixed: HashSet<String> = routing
        .outbounds
        .iter()
        .filter(|o| o.server == host || o.server == "127.0.0.1" && o.port == 12325)
        .map(|o| o.name.clone())
        .collect();
    if fixed.is_empty() {
        return Err("no fixed outbound to replace".into());
    }
    routing.outbounds.retain(|o| !fixed.contains(&o.name));
    let replace = |name: &mut String| {
        if fixed.contains(name) {
            *name = "relay".into();
        }
    };
    for g in &mut routing.groups {
        for n in &mut g.members {
            replace(n);
        }
        g.members.dedup();
        if let Some(selected) = &mut g.selected {
            replace(selected);
        }
    }
    for o in &mut routing.outbounds {
        if let Some(d) = &mut o.dialer_proxy {
            replace(d);
        }
    }
    for r in &mut routing.rules {
        replace(&mut r.target);
    }
    replace(&mut routing.final_target);
    if !profile::validate_routing(&routing).is_empty() {
        return Err("transformed routing is invalid".into());
    }
    let imported_refs: Vec<String> = plan
        .routing
        .outbounds
        .iter()
        .filter_map(|o| o.credential_ref.clone())
        .collect();
    plan.routing = routing.clone();
    let mut set = ProfileSet::default();
    let p = &mut set.agent[0];
    p.routing = routing;
    p.egress.candidates = candidates;
    p.egress.expected.ip = Some(expected.clone());
    p.egress.fixed_exit = true;
    p.egress.connection_path = Some("tunnel-auto".into());
    p.egress.tunnel = Some(Tunnel {
        enabled: true,
        preset: Some(preset.key.into()),
        servers: preset.servers(),
        cipher: preset.cipher.into(),
        credential_ref: "tunnel".into(),
        direct_fallback: false,
        step_timeout_ms: 2500,
    });
    p.proxy_scope.mode = ProxyScopeMode::SystemAndExplicit;
    // Add identity domains even if an old rule sends them through a nested group or DIRECT.
    p.targets = [
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
    .map(|d| profile::Target {
        domain: d.into(),
        // Only configured trace-capable hosts are used for exit comparison;
        // other protected roots are reachability probes, not fabricated IP evidence.
        trace: matches!(d, "claude.ai" | "chatgpt.com"),
    })
    .collect();
    if !profile::validate(&set).is_empty() {
        return Err("transformed profile is invalid".into());
    }
    // mihomo runs with its core directory as cwd; relative -d/-f paths would resolve twice.
    let stage = std::env::current_dir()
        .map_err(|_| "working directory unavailable")?
        .join(&args[3]);
    if stage.exists() {
        return Err("stage directory exists; supply an empty new path".into());
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|_| "repository unavailable")?;
    let bin = repo.join("runtime/bin/aec-mihomo.exe");
    if aec_core::crypto::sha256_file(&bin).map_err(|_| "cannot hash pinned mihomo")?
        != aec_core::core::pin::EXE_SHA256
    {
        return Err("mihomo pin mismatch".into());
    }
    let relay = repo.join("target/release/aec-relay.exe");
    let tray = repo.join("target/release/aec-tray.exe");
    let dev = repo.join("target/release/aec-dev.exe");
    if !relay.is_file() || !tray.is_file() || !dev.is_file() {
        return Err("build release binaries first".into());
    }
    let manager = CoreManager {
        data_dir: stage.join("data"),
        bin: stage.join("bin/aec-mihomo.exe"),
        relay_bin: stage.join("aec-relay.exe"),
        ports: CorePorts::default(),
    };
    let creds = WindowsCredentials;
    let mut fresh: Vec<String> = Vec::new();
    for (reference, u, pwd) in [
        ("exit", user.as_str(), pass.as_str()),
        ("tunnel", "", preset.password),
    ] {
        match creds.get(reference) {
            Some((old_u, old_p)) if old_u == u && old_p == pwd => {}
            Some(_) => {
                return Err("existing credential conflicts; no credential overwritten".into())
            }
            None => fresh.push(reference.into()),
        }
    }
    let staged = (|| -> Result<(), String> {
        std::fs::create_dir(&stage).map_err(|_| "cannot create stage")?;
        std::fs::create_dir(stage.join("bin")).map_err(|_| "cannot create bin")?;
        for (from, to) in [
            (bin, manager.bin.clone()),
            (relay, manager.relay_bin.clone()),
            (tray, stage.join("aec-tray.exe")),
            (dev, stage.join("aec-dev.exe")),
        ] {
            std::fs::copy(from, to).map_err(|_| "cannot stage binary")?;
        }
        for file in aec_core::core::process::GEODATA_FILES {
            std::fs::copy(Path::new(&args[2]).join(file), stage.join("bin").join(file))
                .map_err(|_| "cannot stage trusted geodata")?;
        }
        std::fs::write(stage.join(aec_core::paths::PORTABLE_FLAG), b"")
            .map_err(|_| "cannot mark portable stage")?;
        manager
            .preflight(&WindowsHost)
            .map_err(|_| "new core ports unavailable")?;
        for reference in &fresh {
            let (u, pwd) = if reference == "exit" {
                (user.as_str(), pass.as_str())
            } else {
                ("", preset.password)
            };
            creds
                .store(reference, u, pwd)
                .map_err(|_| "cannot store credential")?;
        }
        let store = ProfileStore::new(&manager.data_dir);
        plan.commit(set, &store, &creds)
            .map_err(|_| "cannot commit routing credentials or profile")?;
        let p = store
            .load()
            .map_err(|_| "cannot load staged profile")?
            .agent
            .remove(0);
        let cfg = aec_core::core::config::relay_config(&p, &manager.ports, &creds)
            .map_err(|_| "staged relay config invalid")?;
        manager
            .up_with_relay(&WindowsHost, &cfg, |ports, s| {
                aec_core::core::config::from_profile(&p, ports, s, &creds)
                    .map_err(|_| "staged core config invalid".into())
            })
            .map_err(|_| "isolated core startup failed")?;
        let verify = (|| -> Result<(), String> {
            manager
                .select(
                    &WindowsHost,
                    aec_core::fixed::FIRST_HOP,
                    aec_core::core::config::TUNNEL_GROUP,
                )
                .map_err(|_| "cannot select tunnel path")?;
            aec_core::fixed::verify(
                &WinHttpClient,
                aec_core::fixed::probe_port(&manager.ports).unwrap(),
                Some(&expected),
            )?;
            Ok(())
        })();
        manager
            .down(&WindowsHost)
            .map_err(|_| "isolated core cleanup failed; leave stage intact")?;
        verify?;
        // Verification used isolated default ports. Only after stopping it, persist the old
        // client entrance for the eventual cutover; never rewrite users' proxy variables.
        aec_core::controller::Settings {
            takeover_enabled: true,
            core_mixed_port: Some(old_port),
        }
        .save(&manager.data_dir)
        .map_err(|_| "cannot save cutover entrance port")?;
        println!("[verified] staged profile: {} preserved ordered rules, {} candidate protocols, official tunnel first hop, exact exit evidence matched; private addresses withheld; global state untouched",p.routing.rules.len(),p.egress.candidates.len());
        Ok(())
    })();
    if staged.is_err() && manager.owned().is_none() && manager.relay_owned().is_none() {
        // Never remove any pre-existing credential. Imported unused credentials need later
        // inspection; fresh common references are removed if preparation did not complete.
        for reference in &fresh {
            let _ = creds.delete(reference);
        }
        for reference in &imported_refs {
            let _ = creds.delete(reference);
        }
    }
    // Import-plan credentials intentionally remain in Credential Manager when staging succeeds.
    // Removed legacy leaf nodes cannot be referenced in the staged profile.
    staged
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match prepare(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("preparation stopped: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_mixed_exits_and_never_logs_source_secrets() {
        let text = "198.51.100.7:1080:socks5:u:password\n198.51.100.8:1080:socks5:u:password";
        let e = source_candidates(text).unwrap_err();
        assert!(!e.contains("password") && !e.contains("198.51.100"));
    }
}
