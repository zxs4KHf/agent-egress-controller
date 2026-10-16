//! Explicit application protection, independent of ordinary upstream/service selections.
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const APP_IDS: [&str; 4] = ["claude_desktop", "edge", "chrome", "codex"];
pub const POLICY_FILE: &str = "protection.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub schema_version: u32,
    pub app_guard: bool,
    pub ipv4_only: bool,
    pub apps: Vec<String>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            schema_version: 1,
            app_guard: false,
            ipv4_only: true,
            apps: vec!["claude_desktop".into(), "edge".into(), "chrome".into()],
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || self.apps.len() > APP_IDS.len()
            || self.apps.iter().any(|id| !APP_IDS.contains(&id.as_str()))
            || self
                .apps
                .iter()
                .enumerate()
                .any(|(i, id)| self.apps[..i].contains(id))
            || (self.app_guard && self.apps.is_empty())
        {
            return Err("PROTECTION_POLICY_INVALID".into());
        }
        Ok(())
    }
    pub fn load(data: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(data.join(POLICY_FILE)) {
            Ok(bytes) if bytes.len() <= 4096 => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            _ => return Err("PROTECTION_POLICY_UNREADABLE".into()),
        };
        let policy: Self =
            serde_json::from_slice(&bytes).map_err(|_| "PROTECTION_POLICY_INVALID")?;
        policy.validate()?;
        Ok(policy)
    }
    pub fn save(&self, data: &Path) -> Result<(), String> {
        self.validate()?;
        crate::store::write_atomic(
            &data.join(POLICY_FILE),
            &serde_json::to_vec_pretty(self).unwrap(),
        )
        .map_err(|_| "PROTECTION_POLICY_WRITE_FAILED".into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Save {
        #[serde(rename = "appGuard")]
        app_guard: bool,
        #[serde(rename = "ipv4Only")]
        ipv4_only: bool,
        apps: Vec<String>,
    },
    Install,
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuardRequest {
    Status,
    Apply { port: u16, apps: Vec<String> },
    Release,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuardStatus {
    pub installed: bool,
    pub active: bool,
    pub filters: usize,
    pub apps: Vec<String>,
    pub port: u16,
    pub code: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protection_options_are_explicit_strict_and_do_not_accept_commands_or_paths() {
        assert!(serde_json::from_str::<Command>(
            r#"{"action":"save","appGuard":true,"ipv4Only":true,"apps":["edge"],"force":true}"#
        )
        .is_err());
        let mut policy = Policy {
            apps: vec!["powershell.exe".into()],
            ..Default::default()
        };
        assert!(policy.validate().is_err());
        policy.apps = vec!["edge".into()];
        policy.app_guard = true;
        assert!(policy.validate().is_ok());
        assert!(serde_json::from_str::<GuardRequest>(
            r#"{"command":"apply","port":7893,"apps":[],"script":"anything"}"#
        )
        .is_err());
    }
}
