//! Read-only source-table acceptance. Never alters source services or system proxy/timezone.
//! Usage: cargo run -p aec-dev --example verify_mihomo_import -- source.yaml mihomo.exe [geodata-dir] [source-proxy-port]
//! Optional source port also runs trace equivalence against a separate temporary owned core.
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let run = || -> Result<(), String> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() < 2 {
            return Err("need source YAML and pinned mihomo executable".into());
        }
        let text = std::fs::read_to_string(&args[0]).map_err(|_| "cannot read source YAML")?;
        let plan = aec_core::import::parse(&text, "verification")?;
        plan.verify(&text)?;
        let binary = PathBuf::from(&args[1])
            .canonicalize()
            .map_err(|_| "cannot find mihomo")?;
        if aec_core::crypto::sha256_file(&binary).map_err(|_| "cannot hash mihomo")?
            != aec_core::core::pin::EXE_SHA256
        {
            return Err("mihomo checksum does not match the pinned version".into());
        }
        let dir = std::env::temp_dir().join(format!(
            "aec-verify-{}",
            aec_core::crypto::random_hex(16).map_err(|_| "cannot create temp name")?
        ));
        std::fs::create_dir(&dir).map_err(|_| "cannot create verification directory")?;
        let result = (|| -> Result<(), String> {
            if plan.routing.uses_geodata() {
                let source = args
                    .get(2)
                    .ok_or("geodata directory is required for this table")?;
                for file in aec_core::core::process::GEODATA_FILES {
                    std::fs::copy(PathBuf::from(source).join(file), dir.join(file))
                        .map_err(|_| "cannot copy supplied geodata")?;
                }
            }
            let config = plan.validation_config()?;
            aec_core::store::write_atomic(&dir.join("config.yaml"), config.as_bytes())
                .map_err(|_| "cannot write transient validation config")?;
            let out = Command::new(&binary)
                .arg("-t")
                .arg("-d")
                .arg(&dir)
                .output()
                .map_err(|_| "cannot run validation")?;
            if !out.status.success() {
                return Err("mihomo -t rejected the configuration (diagnostic output withheld to protect credentials)".into());
            }
            println!("[verified] {} source outbounds preserved (+ built-in relay), {} groups identical, {} rules identical in order; pinned mihomo -t exited 0; no running services or global settings changed",
                plan.routing.outbounds.len(), plan.routing.groups.len(), plan.routing.rules.len()+1);
            if let Some(port) = args.get(3) {
                use aec_core::core::{process::CoreManager, CorePorts};
                use aec_core::http::{HttpClient, Request, Route, WinHttpClient};
                use aec_core::windows::WindowsHost;
                let port: u16 = port
                    .to_str()
                    .and_then(|s| s.parse().ok())
                    .ok_or("invalid source proxy port")?;
                let own_bin = dir.join("aec-mihomo.exe");
                std::fs::copy(&binary, &own_bin)
                    .map_err(|_| "cannot copy pinned binary for isolated check")?;
                let manager = CoreManager {
                    data_dir: dir.join("data"),
                    bin: own_bin,
                    relay_bin: PathBuf::new(),
                    ports: CorePorts {
                        mixed: 31893,
                        controller: 31909,
                        dns: 31553,
                        relay: 31325,
                        tunnel: 31326,
                    },
                };
                manager
                    .up(&WindowsHost, |p, s| plan.validation_config_on(p, s))
                    .map_err(|_| "isolated core startup failed")?;
                let network = (|| -> Result<(), String> {
                    let trace = |port| -> Result<String, String> {
                        let request =
                            Request::get("https://claude.ai/cdn-cgi/trace", Route::Proxy(port));
                        let response = WinHttpClient
                            .send(&request)
                            .map_err(|_| "trace request failed")?;
                        if response.status != 200 {
                            return Err(format!("trace returned HTTP {}", response.status));
                        }
                        response
                            .body
                            .lines()
                            .find_map(|l| l.strip_prefix("ip="))
                            .map(str::to_string)
                            .ok_or("trace has no IP".into())
                    };
                    let source_ip = trace(port)?;
                    let imported_ip = trace(manager.ports.mixed)?;
                    if source_ip != imported_ip {
                        return Err("source/import trace exit addresses differ".into());
                    }
                    println!("[verified] claude.ai/cdn-cgi/trace returned the same exit through source and isolated imported core (IP withheld)");
                    Ok(())
                })();
                let stop = manager.down(&WindowsHost);
                stop.map_err(|_| "isolated core cleanup failed; recover temporary owned core before deleting its directory")?;
                network?;
            }
            Ok(())
        })();
        if dir.join("data").join("core").join("owned.json").exists() {
            return Err(format!(
                "temporary owned core needs recovery; files preserved at {}",
                dir.display()
            ));
        }
        let cleanup = std::fs::remove_dir_all(&dir);
        result?;
        cleanup.map_err(|_| "verification succeeded but transient cleanup failed")?;
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("verification failed: {e}");
            ExitCode::FAILURE
        }
    }
}
