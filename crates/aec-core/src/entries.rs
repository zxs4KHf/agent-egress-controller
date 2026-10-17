//! Browser entries (SPEC 4.3, ticket 08): which installed browser to start, with which arguments,
//! and whether an instance already running on our user-data-dir can take the new window.
//!
//! Nothing is downloaded: a browser that is not installed is reported as ENTRY_NOT_INSTALLED.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::profile::{AgentProfile, Browser};

pub const CHECK_PAGE_PORT: u16 = 17980;
/// Written into the user-data-dir by every launch, so a later launch can tell whether a running
/// instance carries the same arguments.
pub const LAUNCH_RECORD: &str = "aec-launch.json";

pub fn browser_name(b: Browser) -> &'static str {
    match b {
        Browser::Edge => "edge",
        Browser::Chrome => "chrome",
    }
}

/// Standard install locations, most specific first. `App Paths` registry entries are checked by
/// the Windows layer before these.
pub fn standard_locations(b: Browser, env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let roots = ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"];
    let rel = match b {
        Browser::Edge => r"Microsoft\Edge\Application\msedge.exe",
        Browser::Chrome => r"Google\Chrome\Application\chrome.exe",
    };
    for r in roots {
        if let Some(base) = env(r) {
            v.push(Path::new(&base).join(rel));
        }
    }
    v
}

pub fn user_data_dir(data_dir: &Path, b: Browser) -> PathBuf {
    data_dir.join("browser-profiles").join(browser_name(b))
}

/// Arguments for an Agent browser. The order is fixed so that two launches with the same profile
/// produce identical lists (compared through [`LaunchRecord`]).
pub fn browser_args(
    profile: &AgentProfile,
    user_data_dir: &Path,
    proxy_port: u16,
    extra: &[String],
    first_url: Option<&str>,
) -> Vec<String> {
    let mut a = vec![
        format!("--user-data-dir={}", user_data_dir.display()),
        format!("--proxy-server=http://127.0.0.1:{proxy_port}"),
        format!(
            "--proxy-bypass-list={}",
            profile.proxy_scope.bypass.join(";")
        ),
        format!("--lang={}", profile.locale.browser_lang),
        format!("--accept-lang={}", profile.locale.accept_language),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ];
    a.extend(extra.iter().cloned());
    if let Some(u) = first_url {
        a.push(u.to_string());
    }
    a
}

/// Only AEC-owned profile directories. Refuse flags which could override an explicitly approved
/// environment rather than silently launching a different proxy/profile/DNS configuration.
pub fn environment_browser_args(
    profile: &AgentProfile,
    data: &Path,
    browser: Browser,
    proxy_port: u16,
    extra: &[String],
    first_url: Option<&str>,
) -> Result<Vec<String>, &'static str> {
    let flags = crate::browser_environment::launch_flags(data)?;
    if !flags.is_empty()
        && extra.iter().any(|arg| {
            [
                "--user-data-dir",
                "--profile-directory",
                "--proxy-server",
                "--proxy-bypass-list",
                "--proxy-pac-url",
                "--no-proxy-server",
                "--host-resolver-rules",
                "--lang",
                "--accept-lang",
                "--enable-quic",
                "--force-webrtc-ip-handling-policy",
                "--webrtc-ip-handling-policy",
            ]
            .iter()
            .any(|key| arg == key || arg.starts_with(&format!("{key}=")))
        })
    {
        return Err("BROWSER_LAUNCH_ARGUMENT_CONFLICT");
    }
    let mut args = browser_args(
        profile,
        &user_data_dir(data, browser),
        proxy_port,
        extra,
        None,
    );
    if !flags.is_empty() {
        args.retain(|arg| !arg.starts_with("--lang=") && !arg.starts_with("--accept-lang="));
        args.extend(flags);
    }
    if let Some(url) = first_url {
        args.push(url.into());
    }
    Ok(args)
}

/// The settings a running instance was started with. The first URL is not part of it: a new
/// window with another URL is fine, other proxy or language flags are not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRecord {
    pub exe: String,
    pub args: Vec<String>,
    pub pid: u32,
    pub created: u64,
}

impl LaunchRecord {
    pub fn settings(args: &[String]) -> Vec<String> {
        args.iter()
            .filter(|a| a.starts_with("--"))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceDecision {
    /// Nothing runs on this user-data-dir: start a new instance.
    Start,
    /// An instance started by us with the same settings runs: a new launch only opens a window.
    OpenWindow,
    /// Something else, or other settings, run on this user-data-dir. Starting now would silently
    /// drop our flags (legacy finding), so the user has to close it first.
    AskUserToClose { reason: String },
}

pub fn decide(running: bool, record: Option<&LaunchRecord>, wanted: &[String]) -> InstanceDecision {
    match (running, record) {
        (false, _) => InstanceDecision::Start,
        (true, Some(r)) if LaunchRecord::settings(&r.args) == LaunchRecord::settings(wanted) => {
            InstanceDecision::OpenWindow
        }
        (true, Some(_)) => InstanceDecision::AskUserToClose {
            reason: "the running instance was started with other proxy or language settings".into(),
        },
        (true, None) => InstanceDecision::AskUserToClose {
            reason: "an instance this application did not start uses the profile directory".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProfileSet;

    fn profile() -> AgentProfile {
        ProfileSet::default().agent.remove(0)
    }

    #[test]
    fn args_carry_proxy_language_and_profile() {
        let a = browser_args(
            &profile(),
            Path::new(r"D:\d\browser-profiles\edge"),
            17893,
            &["--x".into()],
            Some("http://127.0.0.1:17980/check?t=1"),
        );
        assert_eq!(
            a,
            vec![
                r"--user-data-dir=D:\d\browser-profiles\edge",
                "--proxy-server=http://127.0.0.1:17893",
                "--proxy-bypass-list=<-loopback>;127.0.0.1;localhost",
                "--lang=en-US",
                "--accept-lang=en-US,en",
                "--no-first-run",
                "--no-default-browser-check",
                "--x",
                "http://127.0.0.1:17980/check?t=1",
            ]
        );
    }

    #[test]
    fn decides_on_running_instances() {
        let p = profile();
        let wanted = browser_args(&p, Path::new("u"), 17893, &[], Some("http://a"));
        let same = LaunchRecord {
            exe: "e".into(),
            args: browser_args(&p, Path::new("u"), 17893, &[], Some("http://other")),
            pid: 1,
            created: 1,
        };
        let other = LaunchRecord {
            args: browser_args(&p, Path::new("u"), 7893, &[], None),
            ..same.clone()
        };
        assert_eq!(
            decide(false, Some(&other), &wanted),
            InstanceDecision::Start
        );
        assert_eq!(
            decide(true, Some(&same), &wanted),
            InstanceDecision::OpenWindow,
            "a different first URL is fine"
        );
        assert!(matches!(
            decide(true, Some(&other), &wanted),
            InstanceDecision::AskUserToClose { .. }
        ));
        assert!(matches!(
            decide(true, None, &wanted),
            InstanceDecision::AskUserToClose { .. }
        ));
    }

    #[test]
    fn locations_follow_the_environment() {
        let env = |k: &str| (k == "ProgramFiles(x86)").then(|| r"C:\PF86".to_string());
        assert_eq!(
            standard_locations(Browser::Edge, &env),
            vec![PathBuf::from(
                r"C:\PF86\Microsoft\Edge\Application\msedge.exe"
            )]
        );
        assert!(standard_locations(Browser::Chrome, &|_| None).is_empty());
    }
}
