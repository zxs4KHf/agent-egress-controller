//! Temporary preflight; writes only its own random temporary preview, never client files.
fn main() {
    use aec_core::cooperation::{windows, Command, Manager, RuntimeHost, Selection};
    let result = (|| {
        let root = windows::client_root()?;
        let runtime = windows::ClientRuntime::open(&root)?;
        if runtime.get("/configs")?["mode"] != "rule" {
            return Err("COOP_CLIENT_NOT_RULE_MODE");
        }
        let data = std::env::temp_dir().join(format!("aec-preflight-{}", aec_core::crypto::random_hex(12).map_err(|_| "RANDOM_FAILED")?));
        std::fs::create_dir(&data).map_err(|_| "TEMP_CREATE_FAILED")?;
        let result = Manager::new(&data, &root).execute(Command::Preview, &Selection::for_sites(aec_core::fixed::CLAUDE_DOMAINS.into_iter().map(str::to_owned).collect()));
        let _ = std::fs::remove_file(data.join("cooperation-preview.json"));
        let _ = std::fs::remove_file(data.join("cooperation-error.json"));
        let _ = std::fs::remove_dir(data);
        result.map(|_| ())
    })();
    println!("{}", serde_json::json!({"clientFilesChanged":false,"servicesChanged":false,"installPreflight":if result.is_ok(){"supported"}else{"rejected"},"code":result.err()}));
}
