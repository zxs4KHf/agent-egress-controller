//! `aec-relay`: runs the egress relay as its own process so it outlives the command that started
//! it (the core manager starts it before the core, like the legacy chain did).
//!
//! Usage: aec-relay --config <relay.json> [--log <file>]
//! The config file holds upstream credentials; it is deleted as soon as it has been read.

use std::io::Write;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use aec_core::relay::{self, RelayConfig};
use aec_core::snapshot::rfc3339_utc;

fn now() -> String {
    rfc3339_utc(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
    let Some(config) = value("--config") else {
        eprintln!("usage: aec-relay --config <relay.json> [--log <file>]");
        return ExitCode::from(2);
    };
    let log: Option<Arc<Mutex<std::fs::File>>> = value("--log").and_then(|p| {
        if let Some(dir) = std::path::Path::new(&p).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
            .map(|f| Arc::new(Mutex::new(f)))
    });
    let sink: relay::LogSink = Arc::new(move |level: &str, msg: &str| {
        let line = format!("{}  [{level}] {msg}\n", now());
        match &log {
            Some(f) => {
                let _ = f.lock().map(|mut f| f.write_all(line.as_bytes()));
            }
            None => eprint!("{line}"),
        }
    });

    let cfg: RelayConfig = match std::fs::read(&config)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(c) => c,
        Err(e) => {
            sink("fatal", &format!("relay config unusable: {e}"));
            return ExitCode::from(2);
        }
    };
    // Credentials do not stay on disk while the relay runs.
    let _ = std::fs::remove_file(&config);

    match relay::start(cfg, sink.clone()) {
        Ok(handle) => {
            let _keep = handle;
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            sink("fatal", &format!("relay did not start: {e}"));
            ExitCode::from(3)
        }
    }
}
