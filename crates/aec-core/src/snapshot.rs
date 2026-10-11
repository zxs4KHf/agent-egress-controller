//! Environment snapshot: the recorded state that leaving Agent mode restores (SPEC 4.3).

use serde::{Deserialize, Serialize};

use crate::model::{ProxySettings, RegistryProxy, TimezoneState};
use crate::status::{EnvironmentStatus, Reading};
use crate::writers::WriterScan;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateState {
    Open,
    Closed,
}

/// Whether global state may be written for real (SPEC 4.7). Every unmet condition is listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverGate {
    pub state: GateState,
    pub reasons: Vec<String>,
}

impl TakeoverGate {
    /// `enabled`: the user switched "take over global state" on. `snapshot_persisted`: the entry
    /// snapshot is on disk. The writer scan must be complete and empty.
    pub fn evaluate(enabled: bool, scan: &WriterScan, snapshot_persisted: bool) -> Self {
        let mut reasons = Vec::new();
        if !enabled {
            reasons.push("takeover of global state is not enabled in settings".to_string());
        }
        for w in &scan.writers {
            reasons.push(format!("foreign writer: {}", w.evidence));
        }
        for e in &scan.errors {
            reasons.push(format!("foreign writer scan incomplete: {e}"));
        }
        if !snapshot_persisted {
            reasons.push("entry snapshot is not persisted yet".to_string());
        }
        let state = if reasons.is_empty() {
            GateState::Open
        } else {
            GateState::Closed
        };
        Self { state, reasons }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSnapshot {
    pub schema_version: u32,
    pub id: String,
    /// UTC, RFC 3339.
    pub taken_at: String,
    pub taken_at_unix_ms: u64,
    pub app_version: String,
    pub profile_id: Option<String>,
    pub system_proxy: Reading<ProxySettings>,
    pub system_proxy_registry: Reading<RegistryProxy>,
    pub timezone: Reading<TimezoneState>,
    pub foreign_writers: WriterScan,
    /// Gate as evaluated when the snapshot was taken, before it was written.
    pub takeover_gate: TakeoverGate,
}

impl EnvironmentSnapshot {
    pub fn from_status(
        id: String,
        unix_ms: u64,
        status: &EnvironmentStatus,
        profile_id: Option<String>,
        takeover_enabled: bool,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id,
            taken_at: rfc3339_utc(unix_ms / 1000),
            taken_at_unix_ms: unix_ms,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            profile_id,
            system_proxy: status.system_proxy.clone(),
            system_proxy_registry: status.system_proxy_registry.clone(),
            timezone: status.timezone.clone(),
            foreign_writers: status.foreign_writers.clone(),
            takeover_gate: TakeoverGate::evaluate(takeover_enabled, &status.foreign_writers, false),
        }
    }

    /// A snapshot can drive a restore only if the values it would restore were actually read.
    pub fn is_restorable(&self) -> bool {
        self.system_proxy.value().is_some() && self.timezone.value().is_some()
    }
}

/// Snapshot ids sort in time order: 13-digit unix milliseconds, then a 4-digit sequence.
pub fn snapshot_id(unix_ms: u64, seq: u32) -> String {
    format!("{unix_ms:013}-{seq:04}")
}

pub fn is_snapshot_id(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 18 && b[13] == b'-' && b[..13].iter().chain(&b[14..]).all(u8::is_ascii_digit)
}

/// RFC 3339 UTC timestamp from unix seconds (civil-from-days, proleptic Gregorian).
pub fn rfc3339_utc(unix_s: u64) -> String {
    let days = (unix_s / 86_400) as i64;
    let secs = unix_s % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::collect;
    use crate::testing::FakeHost;
    use crate::writers::{ForeignWriter, WriterKind};

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_791_701_261), "2026-10-11T06:47:41Z");
    }

    #[test]
    fn ids_sort_in_time_order() {
        let a = snapshot_id(1_791_701_261_000, 0);
        let b = snapshot_id(1_791_701_261_000, 1);
        let c = snapshot_id(1_791_701_262_000, 0);
        assert!(a < b && b < c);
        assert!(is_snapshot_id(&a));
        assert!(!is_snapshot_id("current"));
        assert!(!is_snapshot_id("1791701261000-00a1"));
    }

    #[test]
    fn gate_lists_every_unmet_condition() {
        let clear = WriterScan::default();
        assert_eq!(
            TakeoverGate::evaluate(true, &clear, true).state,
            GateState::Open
        );
        let busy = WriterScan {
            writers: vec![ForeignWriter {
                kind: WriterKind::LegacyGuardMutex,
                evidence: "Local\\AIEgressGuard is held".into(),
            }],
            errors: vec!["startup items: denied".into()],
        };
        let g = TakeoverGate::evaluate(false, &busy, false);
        assert_eq!(g.state, GateState::Closed);
        assert_eq!(g.reasons.len(), 4, "{g:?}");
    }

    #[test]
    fn snapshot_from_status_carries_readings() {
        let host = FakeHost::default();
        let s = EnvironmentSnapshot::from_status(
            snapshot_id(1_000, 0),
            1_000,
            &collect(&host, None),
            Some("agent-default".into()),
            false,
        );
        assert!(s.is_restorable());
        assert_eq!(s.takeover_gate.state, GateState::Closed);
        let host = FakeHost {
            fail_timezone: Some(crate::probe::ProbeError::new("tz", None)),
            ..Default::default()
        };
        let s = EnvironmentSnapshot::from_status(
            snapshot_id(1_000, 1),
            1_000,
            &collect(&host, None),
            None,
            false,
        );
        assert!(!s.is_restorable());
    }
}
