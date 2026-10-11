//! Read-only access to machine state. Write paths arrive with the adapters in later tickets and
//! will extend these traits with `plan`/`apply`/`verify` (SPEC 4.7).

use std::fmt;

use crate::model::{ListenerOwner, ProxySettings, RegistryProxy, StartupItem, TimezoneState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    pub what: String,
    pub code: Option<u32>,
}

impl ProbeError {
    pub fn new(what: impl Into<String>, code: Option<u32>) -> Self {
        Self {
            what: what.into(),
            code,
        }
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(code) => write!(f, "{} (code {})", self.what, code),
            None => f.write_str(&self.what),
        }
    }
}

impl std::error::Error for ProbeError {}

pub type ProbeResult<T> = Result<T, ProbeError>;

pub trait SystemProxyReader {
    /// The proxy the system actually uses, from WinINet.
    fn read(&self) -> ProbeResult<ProxySettings>;
    /// The raw registry values behind it.
    fn read_registry(&self) -> ProbeResult<RegistryProxy>;
}

pub trait TimezoneReader {
    fn read(&self) -> ProbeResult<TimezoneState>;
}

/// Evidence sources for foreign writers (SPEC 4.7, takeover gate condition 2).
pub trait WriterEvidence {
    /// Whether a named mutex exists. Never creates it.
    fn mutex_exists(&self, name: &str) -> ProbeResult<bool>;
    fn startup_items(&self) -> ProbeResult<Vec<StartupItem>>;
    /// Owner of an IPv4 TCP listener on `port`, if any.
    fn listener_owner(&self, port: u16) -> ProbeResult<Option<ListenerOwner>>;
}
