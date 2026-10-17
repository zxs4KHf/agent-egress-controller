//! Installed-browser integration fixture. Own temporary launch settings only, never OS policies.
use aec_core::browser_environment::{self as environment, Command, Host, Options, Policy, Value};
struct NoPolicyWrites;
impl Host for NoPolicyWrites {
    fn read(&self, _: Policy, _: bool) -> Result<Option<Value>, &'static str> {
        Ok(None)
    }
    fn write(&self, _: Policy, _: Option<&Value>) -> Result<(), &'static str> {
        panic!("AI-only fixture cannot write OS policies")
    }
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::path::Path::new(&args[1]);
    let port: u16 = args[2].parse().expect("loopback proxy port");
    environment::execute(
        data,
        &NoPolicyWrites,
        Command::Preview {
            options: Options {
                language: "zh-CN".into(),
                ..Options::default()
            },
        },
        1,
        "fixture",
        false,
    )
    .unwrap();
    environment::execute(
        data,
        &NoPolicyWrites,
        Command::Apply {
            plan_id: "fixture".into(),
        },
        2,
        "",
        true,
    )
    .unwrap();
    let profile = aec_core::profile::ProfileSet::default().agent.remove(0);
    let flags = aec_core::entries::environment_browser_args(
        &profile,
        data,
        aec_core::profile::Browser::Edge,
        port,
        &[],
        None,
    )
    .unwrap();
    println!(
        "{}",
        serde_json::json!({"args":flags,"userDataDir":aec_core::entries::user_data_dir(data,aec_core::profile::Browser::Edge)})
    );
}
