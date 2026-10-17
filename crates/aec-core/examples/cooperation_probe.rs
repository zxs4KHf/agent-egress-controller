//! Read-only CVR compatibility diagnostic. No activation, files, probes or service changes.
#[cfg(windows)]
fn main() {
    use aec_core::{
        cooperation::{self, RuntimeHost, Selection},
        probe::SystemProxyReader,
    };
    let result = (|| {
        let root = cooperation::windows::client_root()?;
        let runtime = cooperation::windows::ClientRuntime::open(&root)?;
        let selection = Selection {
            domains: aec_core::fixed::CLAUDE_DOMAINS
                .into_iter()
                .map(str::to_string)
                .collect(),
            fixed_port: 17328,
        };
        let config = runtime.get("/configs")?;
        let system = aec_core::windows::WindowsHost
            .read()
            .map_err(|_| "SYSTEM_PROXY_UNKNOWN")?;
        Ok::<_, &'static str>(serde_json::json!({
            "readOnly":true,"clientMode":config["mode"],
            "controllerCompatible":true,
            "rules":cooperation::verify_runtime(&runtime, &selection),
            "entry":cooperation::scope_check(&runtime, &selection, &system)
        }))
    })();
    match result {
        Ok(value) => println!("{value}"),
        Err(code) => {
            println!(
                "{}",
                serde_json::json!({"readOnly":true,"status":"unknown","code":code})
            );
            std::process::exit(1);
        }
    }
}
#[cfg(not(windows))]
fn main() {}
