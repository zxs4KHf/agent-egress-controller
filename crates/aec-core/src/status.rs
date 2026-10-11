//! One read-only picture of the environment: what `aec-dev status` prints and what the snapshot in
//! ticket 02 will be built from.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::{ProxySettings, RegistryProxy, TimezoneState};
use crate::probe::{ProbeResult, SystemProxyReader, TimezoneReader, WriterEvidence};
use crate::writers::{self, WriterScan};

/// A value that was either read or could not be read. Unreadable is shown, never defaulted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reading<T> {
    Ok { value: T },
    Unreadable { error: String },
}

impl<T> Reading<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Reading::Ok { value } => Some(value),
            Reading::Unreadable { .. } => None,
        }
    }
}

impl<T> From<ProbeResult<T>> for Reading<T> {
    fn from(r: ProbeResult<T>) -> Self {
        match r {
            Ok(value) => Reading::Ok { value },
            Err(e) => Reading::Unreadable {
                error: e.to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentStatus {
    pub collected_at_unix: u64,
    pub system_proxy: Reading<ProxySettings>,
    pub system_proxy_registry: Reading<RegistryProxy>,
    pub timezone: Reading<TimezoneState>,
    pub foreign_writers: WriterScan,
}

pub trait Host: SystemProxyReader + TimezoneReader + WriterEvidence {}
impl<T: SystemProxyReader + TimezoneReader + WriterEvidence> Host for T {}

pub fn collect(host: &dyn Host, own_root: Option<&Path>) -> EnvironmentStatus {
    let system_proxy: Reading<ProxySettings> = SystemProxyReader::read(host).into();
    let foreign_writers = writers::scan(host, system_proxy.value(), own_root);
    EnvironmentStatus {
        collected_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        system_proxy_registry: host.read_registry().into(),
        timezone: TimezoneReader::read(host).into(),
        system_proxy,
        foreign_writers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::ProbeError;
    use crate::testing::FakeHost;

    #[test]
    fn unreadable_values_stay_unreadable() {
        let host = FakeHost {
            fail_proxy: Some(ProbeError::new("InternetQueryOptionW", Some(12018))),
            fail_timezone: Some(ProbeError::new("GetDynamicTimeZoneInformation", None)),
            ..Default::default()
        };
        let s = collect(&host, None);
        assert!(matches!(s.system_proxy, Reading::Unreadable { .. }));
        assert!(matches!(s.timezone, Reading::Unreadable { .. }));
        assert!(
            !s.foreign_writers.is_clear(),
            "an unreadable proxy must leave the writer scan incomplete"
        );
    }

    #[test]
    fn reads_values_through() {
        let host = FakeHost::default();
        let s = collect(&host, None);
        assert_eq!(
            s.timezone.value().unwrap().windows_id,
            "China Standard Time"
        );
        assert!(s.foreign_writers.is_clear());
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["timezone"]["state"], "ok");
    }
}
