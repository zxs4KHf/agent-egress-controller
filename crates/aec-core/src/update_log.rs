//! Bounded offline update evidence. Never echo arbitrary on-disk text to an Agent.
use serde_json::{json, Value};
use std::{io::Read, path::Path, time::SystemTime};

const STAGES: &[&str] = &[
    "initializing",
    "discover",
    "download_manifest",
    "verify_manifest",
    "plan",
    "confirm_download",
    "download_components",
    "verify_components",
    "download_full",
    "verify_full",
    "plan_install",
    "confirm_core",
    "stop_core",
    "install",
    "restart",
    "rollback",
    "complete",
];

pub fn collect(data: std::path::PathBuf) -> Value {
    crate::diagnostics::bounded(std::time::Duration::from_secs(10), move || read(&data))
}

fn advice(code: &str) -> Option<&'static str> {
    Some(match code {
        "none" => "No error at this event; this is not proof of network readiness.",
        "network_timeout" | "network_failed" => "Check GitHub release downloads through the existing route; retry download without approving shutdown.",
        "dns_failed" => "Check GitHub DNS. Do not change the configured fixed exit to bypass this failure.",
        "proxy_failed" => "Check the existing local proxy port and proxy host resolution; do not enable a competing system proxy.",
        "tls_failed" => "Check clock, trusted certificates and TLS interception. Never disable certificate validation.",
        "http_failed" => "Inspect httpStatus: 403/429 may be rate limiting, 404 a missing asset, 5xx transient. Do not expose tokens or retry installation blindly.",
        "asset_missing" | "asset_invalid" | "manifest_invalid" | "component_invalid" => "Invalid release metadata/package. Report version and stage; never bypass validation or use an unverified mirror.",
        "hash_mismatch" | "download_size_mismatch" => "Bytes differ from release metadata. Do not install. Retry a fresh download; report repeated mismatches.",
        "access_denied" => "Check installation/data directory permissions. Do not elevate automatically.",
        "file_locked" => "Check the file owner and normal shutdown state. Do not force-kill processes.",
        "disk_full" => "Free space for downloads, staging and rollback backups before retrying.",
        "io_failed" => "Check disk, permissions and nativeCode. Preserve rollback backups.",
        "operation_failed" => "Inspect stage/nativeCode. After stop/install/restart/rollback failure, check tray recovery and backups before retrying. Do not assume recovery succeeded.",
        "invalid_invocation" => "Start the updater from this installation's tray. Do not invent a PID or version.",
        "update_busy" => "Another updater owns the update lock. Finish or cancel its dialog; do not force-kill it or start parallel updates.",
        "unsupported_version" => "Release version format is unsupported. Report it to the maintainer; do not bypass version checks.",
        "identity_mismatch" | "install_missing" => "Verify the selected installation and tray identity before retrying. No foreign process should be stopped.",
        "shutdown_timeout" | "children_running" | "control_running" => "Normal application shutdown did not complete. Check tray/control-center state; do not force-kill or overwrite locked binaries.",
        "restore_incomplete" => "System restoration is incomplete or unacknowledged. Open the existing tray recovery interface before considering an update.",
        "restart_failed" => "New tray launch failed; old-file rollback/relaunch was attempted. Inspect rollback events and actual tray recovery before retrying.",
        "already_current" => "Selected files or release version already current; no installation required.",
        "full_fallback" => "Release has no incremental manifest. A full ZIP download requires confirmation.",
        "hot_updated" => "UI/documents updated without stopping networking. Save drafts then refresh UI manually.",
        "core_updated" => "Files replaced and tray launch observed; verify actual service readiness separately.",
        _ => return None,
    })
}

fn choice(v: &Value, choices: &[&str]) -> Value {
    v.as_str()
        .filter(|s| choices.contains(s))
        .map_or(Value::Null, |s| json!(s))
}
fn version(v: &Value) -> Value {
    v.as_str()
        .filter(|s| {
            s.len() <= 17
                && s.split('.').count() == 3
                && s.split('.')
                    .all(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .map_or(Value::Null, |s| json!(s))
}
fn no_link(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 == 0
    }
    #[cfg(not(windows))]
    {
        !meta.file_type().is_symlink()
    }
}

/// No API token, network request, process access, popup or write is needed.
pub fn read(data: &Path) -> Value {
    let root = data.join("update-logs");
    let unknown = |reason| json!({"status":"unknown","reason":reason,"readOnly":true,"networkReadiness":"unknown","sessions":[]});
    if !root.exists() {
        return unknown("no_update_logs");
    }
    if !no_link(data) || !no_link(&root) {
        return unknown("unsafe_log_directory");
    }
    let Ok(entries) = std::fs::read_dir(&root) else {
        return unknown("logs_unreadable");
    };
    let mut files = entries
        .take(1024)
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.len() == 37
                && name.ends_with(".json")
                && name.as_bytes()[..32].iter().all(|b| b.is_ascii_hexdigit())
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            (meta.is_file() && no_link(&e.path()))
                .then(|| (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), e.path()))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let candidates = files.len().min(20);
    let mut sessions = Vec::new();
    for (modified, path) in files.into_iter().take(20) {
        let mut bytes = Vec::new();
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        if file.take(128 * 1024 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > 128 * 1024 {
            continue;
        }
        let Ok(raw) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if raw["schemaVersion"] != 1 {
            continue;
        }
        let Some(raw_events) = raw["events"].as_array() else {
            continue;
        };
        if raw_events.is_empty() || raw_events.len() > 64 {
            continue;
        }
        let events = raw_events.iter().map(|e| {
            let code = e["code"].as_str().filter(|s| advice(s).is_some()).unwrap_or("unknown");
            let timestamp = e["time"].as_str().filter(|s| s.len() <= 40 && s.bytes().all(|b| b.is_ascii_digit() || b"TtZz-:+.".contains(&b)));
            json!({"time":timestamp,"stage":choice(&e["stage"], STAGES),"status":choice(&e["status"], &["running","awaiting_confirmation","succeeded","failed","cancelled"]),"code":code,"httpStatus":e["httpStatus"].as_u64().filter(|n| *n <= 599),"nativeCode":e["nativeCode"].as_i64().filter(|n| i32::try_from(*n).is_ok()),"nextAction":advice(code)})
        }).collect::<Vec<_>>();
        let last = events.last().unwrap();
        let terminal = matches!(last["status"].as_str(), Some("failed" | "cancelled"))
            || (last["status"] == "succeeded" && last["stage"] == "complete");
        let status = if terminal {
            last["status"].clone()
        } else {
            json!("unknown")
        };
        let number = |key| {
            raw[key]
                .as_u64()
                .filter(|v| *v <= 2 * 1024 * 1024 * 1024u64)
        };
        sessions.push(json!({"id":path.file_stem().unwrap().to_string_lossy(),"currentVersion":version(&raw["currentVersion"]),"targetVersion":version(&raw["targetVersion"]),"uiOnly":raw["uiOnly"].as_bool(),"transferMode":choice(&raw["transferMode"], &["unknown","incremental","full"]),"plannedBytes":number("plannedBytes"),"downloadedBytes":number("downloadedBytes"),"skippedFiles":number("skippedFiles"),"status":status,"terminal":terminal,"ageSeconds":modified.elapsed().ok().map(|d| d.as_secs()),"events":events}));
    }
    if sessions.is_empty() {
        return unknown("no_readable_update_logs");
    }
    let unreadable = candidates.saturating_sub(sessions.len());
    json!({"status":if unreadable == 0 {"observed"} else {"unknown"},"unreadableSessions":unreadable,"readOnly":true,"networkReadiness":"unknown","liveness":"not_checked","sessions":sessions})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failures_survive_offline_and_untrusted_fields_never_reach_agents() {
        let root = std::env::temp_dir().join(format!("aec-update-log-{}", std::process::id()));
        let dir = root.join("update-logs");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("0123456789abcdef0123456789abcdef.json");
        let raw = json!({"schemaVersion":1,"currentVersion":"0.5.2","targetVersion":"0.5.3","exception":"password=SECRET","transferMode":"incremental","downloadedBytes":42,"events":[{"time":"2026-10-15T00:00:00Z","stage":"download_components","status":"failed","code":"network_timeout","httpStatus":0,"nativeCode":-1,"message":"SECRET"}]});
        std::fs::write(&path, raw.to_string()).unwrap();
        let output = read(&root);
        assert_eq!(output["sessions"][0]["status"], "failed");
        assert_eq!(
            output["sessions"][0]["events"][0]["code"],
            "network_timeout"
        );
        assert!(!output.to_string().contains("SECRET"));
        assert_eq!(output["networkReadiness"], "unknown");
        let mut hostile = raw;
        for key in ["stage", "code", "time"] {
            hostile["events"][0][key] = json!("SECRET");
        }
        hostile["events"][0]["status"] = json!("running");
        hostile["targetVersion"] = json!("SECRET");
        std::fs::write(&path, hostile.to_string()).unwrap();
        let output = read(&root);
        assert!(!output.to_string().contains("SECRET"));
        assert_eq!(output["sessions"][0]["status"], "unknown");
        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(read(&root)["status"], "unknown");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_logs_are_unknown_and_do_not_create_anything() {
        let root = std::env::temp_dir().join("aec-update-logs-do-not-create");
        assert!(!root.exists());
        assert_eq!(read(&root)["reason"], "no_update_logs");
        assert!(!root.exists());
    }
}
