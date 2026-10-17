//! Export synthetic script data for an isolated JavaScript contract test.
fn main() {
    let script = aec_core::cooperation::client_script(&aec_core::cooperation::Selection {
        domains: vec!["claude.ai".into(), "anthropic.com".into()],
        fixed_port: 17328,
    })
    .expect("synthetic selection");
    println!("{}", serde_json::json!({"script":script}));
}
