//! Port preflight (SPEC 4.2): a port is usable when it is free, or when the listener is the core
//! process this application recorded. Anything else is somebody else's, and is left alone.

use serde::{Deserialize, Serialize};

use crate::model::ListenerOwner;
use crate::probe::{ProbeResult, WriterEvidence};

/// A process this application started and is responsible for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedProcess {
    pub role: String,
    pub pid: u32,
    pub exe: String,
    /// Process creation time as a FILETIME value; together with pid it rules out pid reuse.
    pub created: u64,
    pub started_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortState {
    Free,
    Ours,
    Foreign(ListenerOwner),
}

fn same_exe(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

pub fn check(
    evidence: &dyn WriterEvidence,
    ports: &[u16],
    owned: Option<&OwnedProcess>,
) -> ProbeResult<Vec<(u16, PortState)>> {
    ports
        .iter()
        .map(|&port| {
            let state = match evidence.listener_owner(port)? {
                None => PortState::Free,
                Some(o) => match owned {
                    Some(mine)
                        if mine.pid == o.pid
                            && o.exe.as_deref().is_some_and(|e| same_exe(e, &mine.exe)) =>
                    {
                        PortState::Ours
                    }
                    _ => PortState::Foreign(o),
                },
            };
            Ok((port, state))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    fn owned() -> OwnedProcess {
        OwnedProcess {
            role: "core".into(),
            pid: 10,
            exe: r"C:\d\bin\aec-mihomo.exe".into(),
            created: 1,
            started_at_unix_ms: 1,
        }
    }

    #[test]
    fn classifies_free_ours_and_foreign() {
        let mut host = FakeHost::default();
        host.listeners.insert(
            2,
            ListenerOwner {
                port: 2,
                pid: 10,
                exe: Some(r"c:\D\BIN\aec-mihomo.exe".into()),
            },
        );
        host.listeners.insert(
            3,
            ListenerOwner {
                port: 3,
                pid: 11,
                exe: Some(r"C:\x\mihomo.exe".into()),
            },
        );
        host.listeners.insert(
            4,
            ListenerOwner {
                port: 4,
                pid: 10,
                exe: None,
            },
        );
        let r = check(&host, &[1, 2, 3, 4], Some(&owned())).unwrap();
        assert_eq!(r[0].1, PortState::Free);
        assert_eq!(r[1].1, PortState::Ours);
        assert!(matches!(r[2].1, PortState::Foreign(_)));
        assert!(
            matches!(r[3].1, PortState::Foreign(_)),
            "an unreadable path is not proof of ownership"
        );
        let r = check(&host, &[2], None).unwrap();
        assert!(
            matches!(r[0].1, PortState::Foreign(_)),
            "without a record nothing is ours"
        );
    }
}
