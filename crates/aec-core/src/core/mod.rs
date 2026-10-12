//! The proxy core: an independent mihomo instance owned by this application (SPEC 4.2, ticket 03).

pub mod config;
pub mod connlog;
pub mod pin;
pub mod preflight;
#[cfg(windows)]
pub mod process;

/// Ports of this application's core, all bound to 127.0.0.1. Distinct from the legacy chain
/// (7893/9099/10553/12325) so both can run side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorePorts {
    pub mixed: u16,
    pub controller: u16,
    pub dns: u16,
    /// The relay's SOCKS5 listener; only used when the core runs a profile (not the DIRECT test).
    #[serde(default = "default_relay_port")]
    pub relay: u16,
    /// The core's loopback entrance to the tunnel; only listened on when a profile enables one.
    #[serde(default = "default_tunnel_port")]
    pub tunnel: u16,
}

fn default_relay_port() -> u16 {
    17325
}

fn default_tunnel_port() -> u16 {
    17326
}

impl Default for CorePorts {
    fn default() -> Self {
        Self {
            mixed: 17893,
            controller: 17909,
            dns: 17553,
            relay: default_relay_port(),
            tunnel: default_tunnel_port(),
        }
    }
}

impl CorePorts {
    pub fn all(&self) -> [u16; 3] {
        [self.mixed, self.controller, self.dns]
    }
}
