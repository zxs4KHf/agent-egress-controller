//! Tunnel presets offered on the settings page (ADR-0009).
//!
//! A preset is public information published by an exit provider for its own customers, copied
//! here with its source and the date it was last checked. It is a convenience, not a dependency:
//! the user can always type in another tunnel, and a preset that stops working is reported by the
//! relay like any other failed attempt.

use crate::profile::TunnelServer;

pub struct Preset {
    pub key: &'static str,
    pub label: &'static str,
    /// Where the values come from, shown next to the choice.
    pub source: &'static str,
    /// When the official documentation was checked, not a live availability claim.
    pub checked: &'static str,
    pub servers: &'static [(&'static str, u16)],
    pub cipher: &'static str,
    /// Published by the provider in its own setup guide; not a secret of the user.
    pub password: &'static str,
    /// Shown under the choice: what the user still has to get right.
    pub note: &'static str,
}

impl Preset {
    pub fn servers(&self) -> Vec<TunnelServer> {
        self.servers
            .iter()
            .map(|(h, p)| TunnelServer {
                host: (*h).into(),
                port: *p,
            })
            .collect()
    }
}

/// Official public Shadowrocket relay settings; availability depends on the user's network.
pub const PRESETS: [Preset; 1] = [Preset {
    key: "iproyal-cn",
    label: "IPRoyal 官方大陆隧道",
    source: "https://iproyal.com/integrations/how-to-use-iproyal-proxies-with-shadowrocket/",
    checked: "2026-10-12",
    servers: &[
        ("tt01.virtual-liberator.com", 46472),
        ("tt01.virtual-liberator.com", 46473),
        ("tt02.virtual-liberator.com", 46472),
        ("tt02.virtual-liberator.com", 46473),
        ("tt03.virtual-liberator.com", 46472),
        ("tt03.virtual-liberator.com", 46473),
        ("tt04.virtual-liberator.com", 46472),
        ("tt04.virtual-liberator.com", 46473),
        ("tt05.virtual-liberator.com", 46472),
        ("tt05.virtual-liberator.com", 46473),
        ("tt06.virtual-liberator.com", 46472),
        ("tt06.virtual-liberator.com", 46473),
        ("tt07.virtual-liberator.com", 46472),
        ("tt07.virtual-liberator.com", 46473),
        ("tt08.virtual-liberator.com", 46472),
        ("tt08.virtual-liberator.com", 46473),
        ("tt09.virtual-liberator.com", 46472),
        ("tt09.virtual-liberator.com", 46473),
        ("tt10.virtual-liberator.com", 46472),
        ("tt10.virtual-liberator.com", 46473),
        ("tt11.virtual-liberator.com", 46472),
        ("tt11.virtual-liberator.com", 46473),
        ("tt12.virtual-liberator.com", 46472),
        ("tt12.virtual-liberator.com", 46473),
        ("tt13.virtual-liberator.com", 46472),
        ("tt13.virtual-liberator.com", 46473),
        ("tt14.virtual-liberator.com", 46472),
        ("tt14.virtual-liberator.com", 46473),
        ("tt15.virtual-liberator.com", 46472),
        ("tt15.virtual-liberator.com", 46473),
        ("tt16.virtual-liberator.com", 46472),
        ("tt16.virtual-liberator.com", 46473),
        ("tt17.virtual-liberator.com", 46472),
        ("tt17.virtual-liberator.com", 46473),
        ("tt18.virtual-liberator.com", 46472),
        ("tt18.virtual-liberator.com", 46473),
        ("tt19.virtual-liberator.com", 46472),
        ("tt19.virtual-liberator.com", 46473),
        ("tt20.virtual-liberator.com", 46472),
        ("tt20.virtual-liberator.com", 46473),
        ("tt21.virtual-liberator.com", 46472),
        ("tt21.virtual-liberator.com", 46473),
        ("tt22.virtual-liberator.com", 46472),
        ("tt22.virtual-liberator.com", 46473),
        ("tt23.virtual-liberator.com", 46472),
        ("tt23.virtual-liberator.com", 46473),
        ("tt24.virtual-liberator.com", 46472),
        ("tt24.virtual-liberator.com", 46473),
        ("tt25.virtual-liberator.com", 46472),
        ("tt25.virtual-liberator.com", 46473),
    ],
    cipher: "chacha20-ietf-poly1305",
    password: "PCc740",
    note: "经隧道连接时，IPRoyal 看到的来源是隧道服务器，不是你的 IP 白名单，所以上面必须填出口的用户名和密码。",
}];

pub fn preset(key: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::TUNNEL_CIPHERS;

    #[test]
    fn presets_are_well_formed() {
        for p in &PRESETS {
            assert!(TUNNEL_CIPHERS.contains(&p.cipher), "{}", p.key);
            assert!(!p.servers.is_empty() && !p.password.is_empty());
            assert!(p.servers.iter().all(|(h, port)| !h.is_empty() && *port > 0));
        }
        assert!(preset("iproyal-cn").is_some());
        assert!(preset("nope").is_none());
    }
}
