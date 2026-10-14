//! Environment snapshot: the recorded state that leaving Agent mode restores (SPEC 4.3).

use serde::{Deserialize, Serialize};

use crate::controller::GlobalItem;
use crate::model::{ProxySettings, RegistryProxy, TimezoneState};
use crate::status::{EnvironmentStatus, Reading};
use crate::writers::{WriterKind, WriterScan};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateState {
    Open,
    Closed,
}

impl Default for GateState {
    /// A gate that was never evaluated is closed.
    fn default() -> Self {
        GateState::Closed
    }
}

const TIMEZONE_OFF: &str = "changing the system time zone is not enabled in settings";
const SYSTEM_PROXY_OFF: &str =
    "system-level routing (taking over the system proxy) is not enabled in settings";
const NOT_PERSISTED: &str = "entry snapshot is not persisted yet";

/// What the user switched on, per global item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Enabled {
    pub system_proxy: bool,
    pub timezone: bool,
}

/// One global item's own gate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ItemGate {
    pub state: GateState,
    pub reasons: Vec<String>,
}

impl ItemGate {
    fn from_reasons(reasons: Vec<String>) -> Self {
        let state = if reasons.is_empty() {
            GateState::Open
        } else {
            GateState::Closed
        };
        Self { state, reasons }
    }
}

/// Which global items a foreign writer can be expected to rewrite.
///
/// The legacy chain's mutexes and its startup items drive both the system proxy and the time zone,
/// and keep doing so while they run, so they hold back both.
///
/// A process that merely holds the port the system proxy points at (a Clash-style client with its
/// own "system proxy" switch) blocks nothing (ADR-0010). Its setting is what the snapshot records
/// and what leaving Agent mode puts back; while Agent mode runs, the controller keeps the system
/// proxy on the core and backs off if that client keeps rewriting it. Treating it as a blocker
/// made system-level routing impossible for exactly the users who have such a client.
pub(crate) fn writer_items(kind: WriterKind) -> &'static [GlobalItem] {
    match kind {
        WriterKind::ProxyListener => &[],
        WriterKind::LegacyGuardMutex | WriterKind::LegacyTrayMutex | WriterKind::StartupItem => {
            &[GlobalItem::SystemProxy, GlobalItem::Timezone]
        }
    }
}

/// Whether global state may be written for real (SPEC 4.7), decided per global item. Every unmet
/// condition of every item is listed in `reasons`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TakeoverGate {
    /// Open only when every item's gate is open; `allows` is what the controller uses.
    pub state: GateState,
    pub reasons: Vec<String>,
    #[serde(default)]
    pub system_proxy: ItemGate,
    #[serde(default)]
    pub timezone: ItemGate,
}

impl TakeoverGate {
    /// `enabled`: what the user switched on, per item. `snapshot_persisted`: the entry snapshot is
    /// on disk. The writer scan must be complete and hold no writer that blocks the item.
    pub fn evaluate(enabled: Enabled, scan: &WriterScan, snapshot_persisted: bool) -> Self {
        let (mut proxy, mut tz): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        {
            let mut push = |item: GlobalItem, reason: String| match item {
                GlobalItem::SystemProxy => proxy.push(reason),
                GlobalItem::Timezone => tz.push(reason),
            };
            if !enabled.system_proxy {
                push(GlobalItem::SystemProxy, SYSTEM_PROXY_OFF.to_string());
            }
            if !enabled.timezone {
                push(GlobalItem::Timezone, TIMEZONE_OFF.to_string());
            }
            for w in &scan.writers {
                for item in writer_items(w.kind) {
                    push(*item, format!("foreign writer: {}", w.evidence));
                }
            }
            // A probe that did not run says nothing about either item, so it holds back both.
            for e in &scan.errors {
                let reason = format!("foreign writer scan incomplete: {e}");
                push(GlobalItem::SystemProxy, reason.clone());
                push(GlobalItem::Timezone, reason);
            }
            if !snapshot_persisted {
                for item in [GlobalItem::SystemProxy, GlobalItem::Timezone] {
                    push(item, NOT_PERSISTED.to_string());
                }
            }
        }
        let system_proxy = ItemGate::from_reasons(proxy);
        let timezone = ItemGate::from_reasons(tz);
        let mut reasons = system_proxy.reasons.clone();
        for r in &timezone.reasons {
            if !reasons.contains(r) {
                reasons.push(r.clone());
            }
        }
        let state = if system_proxy.state == GateState::Open && timezone.state == GateState::Open {
            GateState::Open
        } else {
            GateState::Closed
        };
        Self {
            state,
            reasons,
            system_proxy,
            timezone,
        }
    }

    /// Whether this global item may be written for real.
    pub fn allows(&self, item: GlobalItem) -> bool {
        match item {
            GlobalItem::SystemProxy => self.system_proxy.state == GateState::Open,
            GlobalItem::Timezone => self.timezone.state == GateState::Open,
        }
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
        enabled: Enabled,
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
            takeover_gate: TakeoverGate::evaluate(enabled, &status.foreign_writers, false),
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

    const ON: Enabled = Enabled {
        system_proxy: true,
        timezone: true,
    };

    #[test]
    fn gate_lists_every_unmet_condition() {
        let clear = WriterScan::default();
        assert_eq!(
            TakeoverGate::evaluate(ON, &clear, true).state,
            GateState::Open
        );
        let busy = WriterScan {
            writers: vec![ForeignWriter {
                kind: WriterKind::LegacyGuardMutex,
                evidence: "Local\\AIEgressGuard is held".into(),
            }],
            errors: vec!["startup items: denied".into()],
        };
        let g = TakeoverGate::evaluate(Enabled::default(), &busy, false);
        assert_eq!(g.state, GateState::Closed);
        // Two switches off, one writer, one incomplete probe, snapshot not on disk.
        assert_eq!(g.reasons.len(), 5, "{g:?}");
    }

    #[test]
    fn each_item_has_its_own_switch() {
        let clear = WriterScan::default();
        let tz_only = Enabled {
            system_proxy: false,
            timezone: true,
        };
        let g = TakeoverGate::evaluate(tz_only, &clear, true);
        assert!(g.allows(GlobalItem::Timezone));
        assert!(!g.allows(GlobalItem::SystemProxy));
        let proxy_only = Enabled {
            system_proxy: true,
            timezone: false,
        };
        let g = TakeoverGate::evaluate(proxy_only, &clear, true);
        assert!(!g.allows(GlobalItem::Timezone));
        assert!(g.allows(GlobalItem::SystemProxy));
    }

    fn scan_of(kind: WriterKind) -> WriterScan {
        WriterScan {
            writers: vec![ForeignWriter {
                kind,
                evidence: "whoever".into(),
            }],
            errors: Vec::new(),
        }
    }

    /// ADR-0010: a Clash-style client holding the current proxy port is what gets snapshotted and
    /// restored, not a reason to refuse.
    #[test]
    fn a_proxy_listener_blocks_nothing() {
        let g = TakeoverGate::evaluate(ON, &scan_of(WriterKind::ProxyListener), true);
        assert_eq!(g.state, GateState::Open, "{g:?}");
        assert!(g.allows(GlobalItem::SystemProxy));
        assert!(g.allows(GlobalItem::Timezone));
    }

    #[test]
    fn the_legacy_chain_holds_back_both_items() {
        for kind in [
            WriterKind::LegacyGuardMutex,
            WriterKind::LegacyTrayMutex,
            WriterKind::StartupItem,
        ] {
            let g = TakeoverGate::evaluate(ON, &scan_of(kind), true);
            assert!(!g.allows(GlobalItem::SystemProxy), "{kind:?}");
            assert!(!g.allows(GlobalItem::Timezone), "{kind:?}");
        }
    }

    #[test]
    fn an_incomplete_scan_holds_back_both_items() {
        let scan = WriterScan {
            writers: Vec::new(),
            errors: vec!["startup items: denied".into()],
        };
        let g = TakeoverGate::evaluate(ON, &scan, true);
        assert!(!g.allows(GlobalItem::SystemProxy));
        assert!(!g.allows(GlobalItem::Timezone));
        assert_eq!(g.reasons.len(), 1, "the same reason is listed once");
    }

    #[test]
    fn snapshot_from_status_carries_readings() {
        let host = FakeHost::default();
        let s = EnvironmentSnapshot::from_status(
            snapshot_id(1_000, 0),
            1_000,
            &collect(&host, None),
            Some("agent-default".into()),
            Enabled::default(),
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
            Enabled::default(),
        );
        assert!(!s.is_restorable());
    }
}
