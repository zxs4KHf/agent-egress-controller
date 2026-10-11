//! Plain data read from the machine. No behaviour that touches the system lives here.

use serde::{Deserialize, Serialize};

/// WinINet per-connection proxy flags (`PROXY_TYPE_*`).
pub mod proxy_flags {
    pub const DIRECT: u32 = 0x1;
    pub const PROXY: u32 = 0x2;
    pub const AUTO_PROXY_URL: u32 = 0x4;
    pub const AUTO_DETECT: u32 = 0x8;
}

/// System proxy as WinINet reports it for the default (LAN) connection.
/// These are the five values an environment snapshot must restore (SPEC 4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySettings {
    pub flags: u32,
    pub proxy_server: Option<String>,
    pub bypass: Option<String>,
    pub auto_config_url: Option<String>,
}

impl ProxySettings {
    pub fn proxy_enabled(&self) -> bool {
        self.flags & proxy_flags::PROXY != 0
    }
    pub fn auto_detect(&self) -> bool {
        self.flags & proxy_flags::AUTO_DETECT != 0
    }
    pub fn pac_enabled(&self) -> bool {
        self.flags & proxy_flags::AUTO_PROXY_URL != 0
    }
}

/// The same settings as stored in HKCU `Internet Settings`. Kept next to the WinINet view so the
/// difference between the two can be observed (SPEC 4.3, open item).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RegistryProxy {
    pub proxy_enable: Option<u32>,
    pub proxy_server: Option<String>,
    pub proxy_override: Option<String>,
    pub auto_config_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimezoneState {
    /// Windows time zone key name, the value `tzutil /g` prints, e.g. `Eastern Standard Time`.
    pub windows_id: String,
    pub dynamic_daylight_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupLocation {
    StartupFolder,
    HkcuRun,
}

/// An entry that starts something at logon: a file in the Startup folder or an HKCU Run value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupItem {
    pub location: StartupLocation,
    pub name: String,
    /// Script text for script files, the command line for Run values, `None` for binaries such as
    /// shortcuts.
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerOwner {
    pub port: u16,
    pub pid: u32,
    /// `None` when this token cannot open the process (for example an elevated process).
    pub exe: Option<String>,
}
