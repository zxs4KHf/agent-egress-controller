//! Bounded, read-only diagnostics for automation that must not open or focus a window.
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

fn bounded(timeout: Duration, inspect: impl FnOnce() -> Value + Send + 'static) -> Value {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(inspect());
    });
    rx.recv_timeout(timeout).unwrap_or_else(|_| json!({"status":"unknown","reason":"diagnostic_timeout","readOnly":true,"networkReadiness":"unknown"}))
}

/// This command exits after ten seconds even if a filesystem or local API read hangs.
/// It emits only allowlisted facts, never raw state, errors, credentials or network addresses.
pub fn collect(data: PathBuf) -> Value {
    bounded(Duration::from_secs(10), move || {
        let profile_valid = crate::store::ProfileStore::new(&data)
            .load()
            .ok()
            .map(|set| crate::profile::validate(&set).is_empty());
        let persisted: Option<Value> = std::fs::read(data.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let access = crate::agent::enabled(&data);
        #[cfg(windows)]
        let live = access
            .then(|| {
                crate::agent::local_call(&data, &json!({"name":"aec_state","arguments":{}})).ok()
            })
            .flatten();
        #[cfg(not(windows))]
        let live: Option<Value> = None;
        let source = live.as_ref().or(persisted.as_ref());
        let mode = source.and_then(|s| s["mode"].as_str()).filter(|m| {
            matches!(
                *m,
                "daily"
                    | "agent_ready"
                    | "agent_unverified"
                    | "entering"
                    | "exiting"
                    | "restore_incomplete"
            )
        });
        json!({"status":if live.is_some(){"observed"}else{"unknown"},"readOnly":true,"version":env!("CARGO_PKG_VERSION"),"source":if live.is_some(){"live_local_api"}else{"saved_state_only"},"mode":mode,"profileValid":profile_valid,"agentAccess":access,"apiResponded":live.is_some(),"busy":live.as_ref().and_then(|s|s["busy"].as_bool()),"fixedBlocked":live.as_ref().and_then(|s|s["blocked"].as_bool()),"networkReadiness":"unknown"})
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_hung_reader_returns_unknown_without_waiting_for_the_reader() {
        let start = std::time::Instant::now();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let result = bounded(Duration::from_millis(30), move || {
            let _ = wait.recv();
            json!({"status":"pass"})
        });
        assert_eq!(result["reason"], "diagnostic_timeout");
        assert_eq!(result["networkReadiness"], "unknown");
        assert!(start.elapsed() < Duration::from_secs(2));
        let _ = release.send(());
    }
    #[test]
    fn fresh_install_diagnosis_does_not_create_files_or_claim_readiness() {
        let missing =
            std::env::temp_dir().join(format!("aec-diagnose-missing-{}", std::process::id()));
        assert!(!missing.exists());
        let result = collect(missing.clone());
        assert_eq!(result["status"], "unknown");
        assert_eq!(result["profileValid"], Value::Null);
        assert_eq!(result["networkReadiness"], "unknown");
        assert!(!missing.exists());
    }
}
