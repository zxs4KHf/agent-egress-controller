//! Start, stop and inspect this application's own mihomo process (Windows).
//!
//! Only a process recorded in `owned.json` and still matching its pid, executable and creation time
//! is ever terminated. Ports held by anything else are reported and left alone.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{
    CloseHandle, SetHandleInformation, FILETIME, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
    WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

use super::config;
use super::pin;
use super::preflight::{self, OwnedProcess, PortState};
use super::CorePorts;
use crate::crypto;
use crate::probe::WriterEvidence;
use crate::snapshot::rfc3339_utc;
use crate::store::write_atomic;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const START_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Error with a code from the SPEC 4.11 families.
#[derive(Debug)]
pub struct CoreError {
    pub code: &'static str,
    pub message: String,
}

impl CoreError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CoreError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinaryState {
    Ok,
    Missing,
    Mismatch { sha256: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreStatus {
    pub binary: BinaryState,
    pub owned: Option<OwnedProcess>,
    pub alive: bool,
    pub ports: Vec<(u16, String)>,
    pub version: Option<String>,
    pub relay: Option<OwnedProcess>,
    pub relay_alive: bool,
}

pub struct CoreManager {
    pub data_dir: PathBuf,
    pub bin: PathBuf,
    /// `aec-relay.exe`, started before the core when a profile (not the DIRECT test) runs.
    pub relay_bin: PathBuf,
    pub ports: CorePorts,
}

/// Domestic-direct data shipped next to the core binary (MetaCubeX meta-rules-dat). mihomo reads
/// them from its working directory and would otherwise try to download them.
pub const GEODATA_FILES: [&str; 2] = ["GeoSite.dat", "GeoIP.dat"];

impl CoreManager {
    /// Mutations require the live process and its controller port to remain ours before and after.
    fn control(
        &self,
        evidence: &dyn WriterEvidence,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<(), CoreError> {
        self.control_body(evidence, method, path, body).map(|_| ())
    }

    /// The same guards, handing the response body back for the calls that have to read it.
    fn control_body(
        &self,
        evidence: &dyn WriterEvidence,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String, CoreError> {
        let owned = self
            .owned()
            .filter(still_ours)
            .ok_or_else(|| CoreError::new("CORE_NOT_RUNNING", "no owned core"))?;
        let verify = || {
            let ports = preflight::check(evidence, &[self.ports.controller], Some(&owned))
                .map_err(|_| {
                    CoreError::new("PORT_PROBE_FAILED", "cannot verify controller owner")
                })?;
            if !still_ours(&owned) || !matches!(ports.first(), Some((_, PortState::Ours))) {
                return Err(CoreError::new(
                    "PORT_IN_USE",
                    "controller does not belong to this session",
                ));
            }
            Ok(())
        };
        verify()?;
        let secret = fs::read_to_string(self.config_path())
            .ok()
            .and_then(|s| secret_from(&s))
            .ok_or_else(|| CoreError::new("CFG_INVALID", "control secret unavailable"))?;
        let result = http_local(self.ports.controller, method, path, Some(&secret), body);
        verify()?;
        match result {
            Ok((200 | 204, body)) => Ok(body),
            Ok((status, _)) => Err(CoreError::new(
                "CORE_CONTROL_FAILED",
                format!("control {method} {path} returned HTTP {status}"),
            )),
            Err(_) => Err(CoreError::new(
                "CORE_CONTROL_FAILED",
                format!("control {method} {path} transport did not complete"),
            )),
        }
    }

    /// Disconnect every connection in our core, including in-flight relay first hops.
    pub fn close_connections(&self, evidence: &dyn WriterEvidence) -> Result<(), CoreError> {
        self.control(evidence, "DELETE", "/connections", "")
    }

    /// Disconnect only the connections that leave through the relay.
    ///
    /// A fixed exit that loses its evidence must cost the protected sites their path and nothing
    /// else. Traffic that never entered the relay did not depend on the exit, and the legacy chain
    /// behaved exactly this way: a broken tunnel took those sites down and left the rest working.
    /// The core names the outbounds behind every live connection, so the ones to cut are known.
    pub fn close_relay_connections(&self, evidence: &dyn WriterEvidence) -> Result<(), CoreError> {
        let report = self.control_body(evidence, "GET", "/connections", "")?;
        for id in relay_connection_ids(&report)? {
            self.control(
                evidence,
                "DELETE",
                &format!("/connections/{}", encode_segment(&id)),
                "",
            )?;
        }
        Ok(())
    }

    /// Reload while the caller holds the fixed-exit gate closed. New configs also default to REJECT.
    pub fn reload(
        &self,
        evidence: &dyn WriterEvidence,
        profile: &crate::profile::AgentProfile,
        creds: &dyn config::CredentialSource,
    ) -> Result<(), CoreError> {
        // Verify ownership before touching runtime files, and close old flows before reload.
        if profile.egress.fixed_exit {
            self.select(evidence, "relay", "REJECT")?;
        }
        self.close_connections(evidence)?;
        let original = fs::read_to_string(self.config_path())
            .map_err(io("CFG_INVALID", &self.config_path()))?;
        let secret = secret_from(&original)
            .ok_or_else(|| CoreError::new("CFG_INVALID", "control secret unavailable"))?;
        let next = config::from_profile(profile, &self.ports, &secret, creds)
            .map_err(|e| CoreError::new("CFG_INVALID", e.to_string()))?;
        if profile.routing.uses_geodata() {
            self.install_geodata()?;
        }
        write_atomic(&self.config_path(), next.as_bytes())
            .map_err(|e| CoreError::new("IO_WRITE", e.to_string()))?;
        if let Err(e) = self.test_config() {
            let _ = write_atomic(&self.config_path(), original.as_bytes());
            return Err(e);
        }
        // Reuse this owned process's startup -f path. Canonicalizing only the file
        // expands Windows 8.3 aliases while mihomo's -d retains the short spelling;
        // its lexical safe-path check then rejects the same file with HTTP 400.
        // The pinned API takes force in the query, not in the JSON object.
        self.control(evidence, "PUT", "/configs?force=true", "{}")?;
        if profile.egress.fixed_exit {
            self.select(evidence, "relay", "REJECT")?;
        }
        self.apply_selections(evidence, &profile.routing)
    }
    fn geodata_source(&self) -> PathBuf {
        self.bin
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// The shipped data files are present (checked before anything changes).
    pub fn geodata_ready(&self) -> Result<(), CoreError> {
        let dir = self.geodata_source();
        let missing: Vec<&str> = GEODATA_FILES
            .iter()
            .copied()
            .filter(|f| !dir.join(f).is_file())
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(CoreError::new(
                "CORE_GEODATA_MISSING",
                format!(
                    "geodata rules need {} next to the core in {}; provide trusted data files manually (never downloaded automatically)",
                    missing.join(" and "),
                    dir.display()
                ),
            ))
        }
    }

    /// Put the data files where the core looks, when the copy there is missing or differs in size.
    fn install_geodata(&self) -> Result<(), CoreError> {
        self.geodata_ready()?;
        let (from, to) = (self.geodata_source(), self.core_dir());
        fs::create_dir_all(&to).map_err(io("IO_WRITE", &to))?;
        for f in GEODATA_FILES {
            let (src, dst) = (from.join(f), to.join(f));
            let same = match (fs::metadata(&src), fs::metadata(&dst)) {
                (Ok(a), Ok(b)) => a.len() == b.len(),
                _ => false,
            };
            if !same {
                fs::copy(&src, &dst).map_err(io("IO_WRITE", &dst))?;
            }
        }
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn io<'a>(code: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> CoreError + 'a {
    move |e| CoreError::new(code, format!("{}: {e}", path.display()))
}

/// Executable path and creation time of a live process, if this token can open it.
pub(crate) fn process_identity(pid: u32) -> Option<(String, u64)> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        let (mut c, mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) =
            std::mem::zeroed();
        let ok_name = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
        let ok_time = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) != 0;
        CloseHandle(h);
        if !(ok_name && ok_time) {
            return None;
        }
        let created = (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime);
        Some((String::from_utf16_lossy(&buf[..len as usize]), created))
    }
}

/// Keep this process's own standard handles out of the long-lived core.
///
/// std spawns with handle inheritance on, so every inheritable handle this process holds goes to
/// the child, including the write end of a pipe our caller reads our output from. The core would
/// then hold that pipe open for its whole life and the caller would wait forever (observed
/// 2026-10-11 with `aec-dev core up --direct | Out-Null`). The child's own stdio is set explicitly,
/// so clearing the flag on ours loses nothing.
pub(crate) fn stop_std_handle_inheritance() {
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// The recorded process is still the same process (no pid reuse).
pub(crate) fn still_ours(o: &OwnedProcess) -> bool {
    matches!(process_identity(o.pid), Some((exe, created)) if exe.eq_ignore_ascii_case(&o.exe) && created == o.created)
}

fn terminate(o: &OwnedProcess) -> Result<(), CoreError> {
    if !still_ours(o) {
        return Ok(());
    }
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, o.pid);
        if h.is_null() {
            return Err(CoreError::new(
                "CORE_STOP_FAILED",
                format!("cannot open pid {}", o.pid),
            ));
        }
        let ok = TerminateProcess(h, 1) != 0;
        let waited = WaitForSingleObject(h, STOP_TIMEOUT.as_millis() as u32);
        CloseHandle(h);
        if !ok || waited != 0 {
            return Err(CoreError::new(
                "CORE_STOP_FAILED",
                format!("pid {} did not exit", o.pid),
            ));
        }
    }
    Ok(())
}

/// Minimal HTTP/1.1 GET against a loopback port; returns status and body.
pub fn http_get_local(
    port: u16,
    path: &str,
    bearer: Option<&str>,
) -> Result<(u16, String), String> {
    http_local(port, "GET", path, bearer, "")
}

fn http_local(
    port: u16,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: &str,
) -> Result<(u16, String), String> {
    if !path.starts_with('/')
        || path.contains(['\r', '\n', ' '])
        || bearer.is_some_and(|s| s.contains(['\r', '\n']))
    {
        return Err("invalid local request".into());
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    let auth = bearer
        .map(|b| format!("Authorization: Bearer {b}\r\n"))
        .unwrap_or_default();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
    )
    .map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.take(1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 1024 * 1024 {
        return Err("local response too large".into());
    }
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("malformed response")?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or("no status code")?;
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_string()
    };
    Ok((status, body))
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        body = rest[n..].trim_start_matches("\r\n");
    }
    out
}

fn secret_from(config_text: &str) -> Option<String> {
    config_text
        .lines()
        .find_map(|l| l.strip_prefix("secret: "))
        .map(|v| v.trim().trim_matches('"').to_string())
}

fn encode_segment(name: &str) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(b as char);
        } else {
            let _ = write!(encoded, "%{b:02X}");
        }
    }
    encoded
}

/// The connections whose outbound chain goes through the relay, taken from the core's own report.
///
/// Unreadable evidence returns an error, never a blanket disconnect or a claim of successful closure.
fn relay_connection_ids(report: &str) -> Result<Vec<String>, CoreError> {
    let invalid = || {
        CoreError::new(
            "CORE_CONTROL_FAILED",
            "connection closure evidence unavailable",
        )
    };
    let parsed: serde_json::Value = serde_json::from_str(report).map_err(|_| invalid())?;
    // The pinned mihomo serializes its empty Go slice as null on a fresh idle core.
    // Require the field to exist: a missing or malformed report is still unavailable evidence.
    let list = match parsed.get("connections") {
        Some(serde_json::Value::Null) => return Ok(Vec::new()),
        Some(serde_json::Value::Array(list)) => list,
        _ => return Err(invalid()),
    };
    let mut ids = Vec::new();
    for connection in list {
        let chains = connection
            .get("chains")
            .and_then(|c| c.as_array())
            .ok_or_else(invalid)?;
        let mut relay = false;
        for name in chains {
            let name = name.as_str().ok_or_else(invalid)?;
            relay |= name == config::RELAY_PROXY || name == crate::fixed::TRANSPORT;
        }
        if relay {
            let id = connection
                .get("id")
                .and_then(|id| id.as_str())
                .filter(|id| !id.is_empty())
                .ok_or_else(invalid)?;
            ids.push(id.to_owned());
        }
    }
    Ok(ids)
}

impl CoreManager {
    /// Development layout: binary in `<repo>/runtime/bin`, data in `<repo>/runtime/dev-data`.
    pub fn dev(repo_root: &Path, data_dir: &Path) -> Self {
        let portable_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .filter(|p| p.join(crate::paths::PORTABLE_FLAG).is_file())
            .map(|p| p.join("bin").join(pin::EXE_NAME));
        let relay_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("aec-relay.exe")))
            .unwrap_or_else(|| PathBuf::from("aec-relay.exe"));
        Self {
            data_dir: data_dir.to_path_buf(),
            bin: portable_bin
                .unwrap_or_else(|| repo_root.join("runtime").join("bin").join(pin::EXE_NAME)),
            relay_bin,
            ports: crate::controller::Settings::load(data_dir).core_ports(),
        }
    }

    /// PUT only to this application's verified control port, never to a foreign listener.
    pub fn select(
        &self,
        evidence: &dyn WriterEvidence,
        group: &str,
        member: &str,
    ) -> Result<(), CoreError> {
        let owned = self
            .owned()
            .filter(still_ours)
            .ok_or_else(|| CoreError::new("CORE_NOT_RUNNING", "no owned core is running"))?;
        let ports = preflight::check(evidence, &[self.ports.controller], Some(&owned))
            .map_err(|_| CoreError::new("PORT_PROBE_FAILED", "cannot verify control port owner"))?;
        if !matches!(ports.first(), Some((_, PortState::Ours))) {
            return Err(CoreError::new(
                "PORT_IN_USE",
                "control port is not owned by this core",
            ));
        }
        let secret = fs::read_to_string(self.config_path())
            .ok()
            .and_then(|s| secret_from(&s))
            .ok_or_else(|| CoreError::new("CFG_INVALID", "control secret is unavailable"))?;
        let path = format!("/proxies/{}", encode_segment(group));
        let body = serde_json::json!({"name": member}).to_string();
        match http_local(self.ports.controller, "PUT", &path, Some(&secret), &body) {
            Ok((200 | 204, _)) => Ok(()),
            Ok((status, _)) => Err(CoreError::new(
                "CORE_SWITCH_FAILED",
                format!("control API returned HTTP {status}"),
            )),
            Err(_) => Err(CoreError::new(
                "CORE_SWITCH_FAILED",
                "control API did not complete",
            )),
        }
    }

    pub fn apply_selections(
        &self,
        evidence: &dyn WriterEvidence,
        routing: &crate::profile::Routing,
    ) -> Result<(), CoreError> {
        for group in &routing.groups {
            if let Some(member) = &group.selected {
                self.select(evidence, &group.name, member)?;
            }
        }
        Ok(())
    }

    /// Read live connections only from the verified owned core. No credentials or raw API body
    /// enter error messages; never contact a foreign controller, even for observation.
    pub fn connections(&self, evidence: &dyn WriterEvidence) -> Result<String, CoreError> {
        let owned = self
            .owned()
            .filter(still_ours)
            .ok_or_else(|| CoreError::new("CORE_NOT_RUNNING", "no owned core is running"))?;
        let verify = || {
            let ports = preflight::check(
                evidence,
                &[self.ports.mixed, self.ports.controller],
                Some(&owned),
            )
            .map_err(|_| CoreError::new("PORT_PROBE_FAILED", "cannot verify core ports"))?;
            if !still_ours(&owned) || !ports.iter().all(|(_, s)| matches!(s, PortState::Ours)) {
                return Err(CoreError::new(
                    "PORT_IN_USE",
                    "core process or ports no longer belong to this session",
                ));
            }
            Ok(())
        };
        verify()?;
        let secret = fs::read_to_string(self.config_path())
            .ok()
            .and_then(|s| secret_from(&s))
            .ok_or_else(|| CoreError::new("CFG_INVALID", "control secret is unavailable"))?;
        let response = http_get_local(self.ports.controller, "/connections", Some(&secret));
        verify()?;
        match response {
            Ok((200, body)) => Ok(body),
            _ => Err(CoreError::new(
                "CORE_OBSERVE_FAILED",
                "connection snapshot unavailable",
            )),
        }
    }

    fn relay_config_path(&self) -> PathBuf {
        self.core_dir().join("relay.json")
    }
    fn relay_owned_path(&self) -> PathBuf {
        self.core_dir().join("relay-owned.json")
    }
    pub fn relay_log_path(&self) -> PathBuf {
        let day = rfc3339_utc(now_ms() / 1000)[..10].replace('-', "");
        self.data_dir.join("logs").join(format!("relay-{day}.log"))
    }
    pub fn relay_owned(&self) -> Option<OwnedProcess> {
        fs::read(self.relay_owned_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }
    fn clear_relay_files(&self) {
        let _ = fs::remove_file(self.relay_config_path());
        let _ = fs::remove_file(self.relay_owned_path());
    }

    fn stop_relay(&self) -> Result<(), CoreError> {
        if let Some(r) = self.relay_owned() {
            terminate(&r)?;
        }
        self.clear_relay_files();
        Ok(())
    }

    /// Start the relay (unless ours already runs), then the core. The relay must listen before the
    /// core starts, or the core's only upstream is dead by construction (legacy order).
    pub fn up_with_relay(
        &self,
        evidence: &dyn WriterEvidence,
        relay_cfg: &crate::relay::RelayConfig,
        make_config: impl FnOnce(&CorePorts, &str) -> Result<String, String>,
    ) -> Result<OwnedProcess, CoreError> {
        if self.relay_owned().filter(still_ours).is_none() {
            self.clear_relay_files();
            let busy = preflight::check(evidence, &[self.ports.relay], None)
                .map_err(|e| CoreError::new("PORT_PROBE_FAILED", e.to_string()))?;
            if let Some((p, PortState::Foreign(o))) = busy.into_iter().next() {
                return Err(CoreError::new(
                    "PORT_IN_USE",
                    format!(
                        "{p} held by pid {} ({})",
                        o.pid,
                        o.exe.as_deref().unwrap_or("path not readable")
                    ),
                ));
            }
            if !self.relay_bin.is_file() {
                return Err(CoreError::new(
                    "RELAY_BINARY_MISSING",
                    format!("{} not found", self.relay_bin.display()),
                ));
            }
            // The file carries upstream credentials; the relay deletes it as soon as it has read it.
            write_atomic(
                &self.relay_config_path(),
                &serde_json::to_vec(relay_cfg).expect("serializes"),
            )
            .map_err(|e| CoreError::new("IO_WRITE", e.to_string()))?;
            stop_std_handle_inheritance();
            let child = Command::new(&self.relay_bin)
                .arg("--config")
                .arg(self.relay_config_path())
                .arg("--log")
                .arg(self.relay_log_path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map_err(io("RELAY_START_FAILED", &self.relay_bin))?;
            let pid = child.id();
            drop(child);
            let Some((exe, created)) = process_identity(pid) else {
                self.clear_relay_files();
                return Err(CoreError::new(
                    "RELAY_START_FAILED",
                    format!(
                        "relay pid {pid} exited immediately; see {}",
                        self.relay_log_path().display()
                    ),
                ));
            };
            let owned = OwnedProcess {
                role: "relay".into(),
                pid,
                exe,
                created,
                started_at_unix_ms: now_ms(),
            };
            write_atomic(
                &self.relay_owned_path(),
                &serde_json::to_vec_pretty(&owned).expect("serializes"),
            )
            .map_err(|e| CoreError::new("IO_WRITE", e.to_string()))?;
            let deadline = Instant::now() + START_TIMEOUT;
            loop {
                let up = preflight::check(evidence, &[self.ports.relay], Some(&owned))
                    .map(|r| r[0].1 == PortState::Ours)
                    .unwrap_or(false);
                if up {
                    break;
                }
                if Instant::now() >= deadline || !still_ours(&owned) {
                    let _ = self.stop_relay();
                    return Err(CoreError::new(
                        "RELAY_START_TIMEOUT",
                        format!(
                            "relay did not listen on {} in time; see {}",
                            self.ports.relay,
                            self.relay_log_path().display()
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        match self.up(evidence, make_config) {
            Ok(o) => Ok(o),
            Err(e) => {
                let _ = self.stop_relay();
                Err(e)
            }
        }
    }

    pub fn core_dir(&self) -> PathBuf {
        self.data_dir.join("core")
    }
    pub fn config_path(&self) -> PathBuf {
        self.core_dir().join("config.yaml")
    }
    fn owned_path(&self) -> PathBuf {
        self.core_dir().join("owned.json")
    }
    /// Today's core log (UTC day), where the core writes one line per connection.
    pub fn log_path(&self) -> PathBuf {
        let day = rfc3339_utc(now_ms() / 1000)[..10].replace('-', "");
        self.data_dir.join("logs").join(format!("core-{day}.log"))
    }

    pub fn binary_state(&self) -> BinaryState {
        if !self.bin.is_file() {
            return BinaryState::Missing;
        }
        match crypto::sha256_file(&self.bin) {
            Ok(h) if h == pin::EXE_SHA256 => BinaryState::Ok,
            Ok(h) => BinaryState::Mismatch { sha256: h },
            Err(e) => BinaryState::Mismatch {
                sha256: format!("unreadable: {e}"),
            },
        }
    }

    /// Copy a mihomo executable into place after checking it is the pinned build.
    pub fn install(&self, src: &Path) -> Result<(), CoreError> {
        let h = crypto::sha256_file(src)
            .map_err(|e| CoreError::new("CORE_BINARY_MISSING", e.to_string()))?;
        if h != pin::EXE_SHA256 {
            return Err(CoreError::new(
                "CORE_BINARY_MISMATCH",
                format!(
                    "{} has sha256 {h}, pinned {} ({})",
                    src.display(),
                    pin::EXE_SHA256,
                    pin::VERSION
                ),
            ));
        }
        let dir = self.bin.parent().expect("bin has a parent");
        fs::create_dir_all(dir).map_err(io("IO_WRITE", dir))?;
        let tmp = self.bin.with_extension("exe.tmp");
        fs::copy(src, &tmp).map_err(io("IO_WRITE", &tmp))?;
        fs::rename(&tmp, &self.bin).map_err(io("IO_WRITE", &self.bin))?;
        Ok(())
    }

    pub fn owned(&self) -> Option<OwnedProcess> {
        fs::read(self.owned_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    fn clear_runtime_files(&self) {
        let _ = fs::remove_file(self.config_path());
        let _ = fs::remove_file(self.owned_path());
    }

    /// Validate a configuration with `mihomo -t` without starting anything.
    fn test_config(&self) -> Result<(), CoreError> {
        let out = Command::new(&self.bin)
            .args(["-t", "-d"])
            .arg(self.core_dir())
            .arg("-f")
            .arg(self.config_path())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(io("CORE_START_FAILED", &self.bin))?;
        if out.status.success() {
            Ok(())
        } else {
            // Parser diagnostics can quote an entire credential-bearing YAML scalar.
            Err(CoreError::new(
                "CORE_CONFIG_INVALID",
                "generated configuration was rejected by mihomo",
            ))
        }
    }

    /// Everything `up` checks before it changes anything: binary, and ports free or already ours.
    pub fn preflight(&self, evidence: &dyn WriterEvidence) -> Result<(), CoreError> {
        match self.binary_state() {
            BinaryState::Ok => {}
            BinaryState::Missing => {
                return Err(CoreError::new(
                    "CORE_BINARY_MISSING",
                    format!(
                        "{} not found; run `aec-dev core install <exe>`",
                        self.bin.display()
                    ),
                ))
            }
            BinaryState::Mismatch { sha256 } => {
                return Err(CoreError::new(
                    "CORE_BINARY_MISMATCH",
                    format!("{} has sha256 {sha256}", self.bin.display()),
                ))
            }
        }
        let owned = self.owned().filter(still_ours);
        let ports = preflight::check(evidence, &self.ports.all(), owned.as_ref())
            .map_err(|e| CoreError::new("PORT_PROBE_FAILED", e.to_string()))?;
        let busy: Vec<String> = ports
            .iter()
            .filter_map(|(p, s)| match s {
                PortState::Foreign(o) => Some(format!(
                    "{p} held by pid {} ({})",
                    o.pid,
                    o.exe.as_deref().unwrap_or("path not readable")
                )),
                _ => None,
            })
            .collect();
        if busy.is_empty() {
            Ok(())
        } else {
            Err(CoreError::new("PORT_IN_USE", busy.join("; ")))
        }
    }

    /// Start the core with a generated configuration. `make_config` receives the fresh secret.
    pub fn up(
        &self,
        evidence: &dyn WriterEvidence,
        make_config: impl FnOnce(&CorePorts, &str) -> Result<String, String>,
    ) -> Result<OwnedProcess, CoreError> {
        self.preflight(evidence)?;
        if let Some(o) = self.owned().filter(still_ours) {
            return Ok(o);
        }
        self.clear_runtime_files();

        let secret = crypto::random_hex(16)
            .map_err(|e| CoreError::new("CORE_START_FAILED", e.to_string()))?;
        let text =
            make_config(&self.ports, &secret).map_err(|e| CoreError::new("CFG_INVALID", e))?;
        // Listeners the profile adds (the tunnel entrance) must be free now and ours once started.
        let extra = super::config::extra_listen_ports(&text);
        if !extra.is_empty() {
            let held = preflight::check(evidence, &extra, None)
                .map_err(|e| CoreError::new("PORT_PROBE_FAILED", e.to_string()))?;
            if let Some((p, PortState::Foreign(o))) = held
                .into_iter()
                .find(|(_, s)| matches!(s, PortState::Foreign(_)))
            {
                return Err(CoreError::new(
                    "PORT_IN_USE",
                    format!(
                        "{p} held by pid {} ({})",
                        o.pid,
                        o.exe.as_deref().unwrap_or("path not readable")
                    ),
                ));
            }
        }
        let watched: Vec<u16> = self.ports.all().into_iter().chain(extra).collect();
        if text.contains("\ngeodata-mode: true\n") {
            self.install_geodata()?;
        }
        write_atomic(&self.config_path(), text.as_bytes())
            .map_err(|e| CoreError::new("IO_WRITE", e.to_string()))?;
        if let Err(e) = self.test_config() {
            self.clear_runtime_files();
            return Err(e);
        }

        let log = self.log_path();
        fs::create_dir_all(log.parent().expect("log has a parent"))
            .map_err(io("IO_WRITE", &log))?;
        let out = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(io("IO_WRITE", &log))?;
        let err = out.try_clone().map_err(io("IO_WRITE", &log))?;
        stop_std_handle_inheritance();
        let child = Command::new(&self.bin)
            .arg("-d")
            .arg(self.core_dir())
            .arg("-f")
            .arg(self.config_path())
            .current_dir(self.core_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(io("CORE_START_FAILED", &self.bin))?;
        let pid = child.id();
        drop(child);
        let Some((exe, created)) = process_identity(pid) else {
            self.clear_runtime_files();
            return Err(CoreError::new(
                "CORE_START_FAILED",
                format!(
                    "pid {pid} exited immediately; see {}",
                    self.log_path().display()
                ),
            ));
        };
        let owned = OwnedProcess {
            role: "core".into(),
            pid,
            exe,
            created,
            started_at_unix_ms: now_ms(),
        };
        write_atomic(
            &self.owned_path(),
            &serde_json::to_vec_pretty(&owned).expect("serializes"),
        )
        .map_err(|e| CoreError::new("IO_WRITE", e.to_string()))?;

        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            let ready = preflight::check(evidence, &watched, Some(&owned))
                .map(|r| r.iter().all(|(_, s)| *s == PortState::Ours))
                .unwrap_or(false)
                && matches!(
                    http_get_local(self.ports.controller, "/version", Some(&secret)),
                    Ok((200, _))
                );
            if ready {
                return Ok(owned);
            }
            if Instant::now() >= deadline || !still_ours(&owned) {
                let _ = terminate(&owned);
                self.clear_runtime_files();
                return Err(CoreError::new(
                    "CORE_START_TIMEOUT",
                    format!(
                        "core did not become ready within {}s; see {}",
                        START_TIMEOUT.as_secs(),
                        self.log_path().display()
                    ),
                ));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Stop the recorded core, wait for its ports to close, delete the generated configuration.
    pub fn down(&self, evidence: &dyn WriterEvidence) -> Result<bool, CoreError> {
        let core = self.down_core(evidence);
        // The relay goes after the core, the reverse of the start order.
        let relay = self.stop_relay();
        let stopped = core?;
        relay?;
        Ok(stopped)
    }

    fn down_core(&self, evidence: &dyn WriterEvidence) -> Result<bool, CoreError> {
        let Some(owned) = self.owned() else {
            self.clear_runtime_files();
            return Ok(false);
        };
        let was_running = still_ours(&owned);
        terminate(&owned)?;
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            let held = preflight::check(evidence, &self.ports.all(), Some(&owned))
                .map(|r| r.iter().any(|(_, s)| *s == PortState::Ours))
                .unwrap_or(true);
            if !held {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        self.clear_runtime_files();
        Ok(was_running)
    }

    pub fn status(&self, evidence: &dyn WriterEvidence) -> CoreStatus {
        let owned = self.owned();
        let alive = owned.as_ref().is_some_and(still_ours);
        let ports = match preflight::check(evidence, &self.ports.all(), owned.as_ref()) {
            Ok(r) => r
                .into_iter()
                .map(|(p, s)| {
                    let d = match s {
                        PortState::Free => "free".to_string(),
                        PortState::Ours => "ours".to_string(),
                        PortState::Foreign(o) => {
                            format!("foreign pid {} ({})", o.pid, o.exe.unwrap_or_default())
                        }
                    };
                    (p, d)
                })
                .collect(),
            Err(e) => vec![(0, format!("probe failed: {e}"))],
        };
        let version = if alive {
            fs::read_to_string(self.config_path())
                .ok()
                .and_then(|t| secret_from(&t))
                .and_then(|s| http_get_local(self.ports.controller, "/version", Some(&s)).ok())
                .map(|(_, body)| body.trim().to_string())
        } else {
            None
        };
        let relay = self.relay_owned();
        let relay_alive = relay.as_ref().is_some_and(still_ours);
        CoreStatus {
            binary: self.binary_state(),
            owned,
            alive,
            ports,
            version,
            relay,
            relay_alive,
        }
    }
}

/// Read a file fully; used by tests and the dev command line.
pub fn read_text(path: &Path) -> Option<String> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::WindowsHost;
    use std::net::TcpListener;

    fn temp_manager(tag: &str) -> (CoreManager, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "aec-core-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&root).unwrap();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let mut m = CoreManager::dev(&repo, &root);
        m.bin = crate::paths::strip_verbatim(&m.bin.canonicalize().unwrap_or(m.bin.clone()));
        (m, root)
    }

    #[test]
    fn dechunks_bodies() {
        assert_eq!(dechunk("4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"), "Wikipedia");
    }

    #[test]
    fn idle_mihomo_connection_report_has_no_connections_to_close() {
        assert!(relay_connection_ids(
            r#"{"downloadTotal":0,"uploadTotal":0,"connections":null,"memory":0}"#
        )
        .unwrap()
        .is_empty());
        for invalid in [r#"{}"#, r#"{"connections":false}"#, r#"{"connections":{}}"#] {
            assert!(relay_connection_ids(invalid).is_err());
        }
    }

    #[test]
    #[ignore = "needs runtime/bin/aec-mihomo.exe; isolated local core ports"]
    fn fresh_idle_core_allows_fixed_exit_connection_cleanup() {
        let (mut m, root) = temp_manager("idle-cleanup");
        m.ports = CorePorts {
            mixed: 33893,
            controller: 33909,
            dns: 33553,
            relay: 33325,
            tunnel: 33326,
        };
        let result = std::panic::catch_unwind(|| {
            m.up(&WindowsHost, |p, s| {
                Ok(config::direct_test(p, s, "agent-egress"))
            })
            .unwrap();
            let report = m.connections(&WindowsHost).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&report).unwrap();
            assert!(
                parsed["connections"].is_null(),
                "fresh core report: {report}"
            );
            m.close_relay_connections(&WindowsHost).unwrap();
        });
        let _ = m.down(&WindowsHost);
        let _ = fs::remove_dir_all(root);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn only_relay_connections_are_named_for_closing() {
        let report = r#"{"connections":[
            {"id":"a1","chains":["aec-relay-transport","relay"]},
            {"id":"b2","chains":["DIRECT"]},
            {"id":"c3","chains":["daily-group","daily-upstream"]},
            {"id":"d4","chains":["relay"]},
            {"id":"e5","chains":["relay-custom"]}
        ]}"#;
        assert_eq!(
            relay_connection_ids(report).unwrap(),
            vec!["a1".to_string(), "d4".to_string()]
        );
        assert!(
            relay_connection_ids("upstream error").is_err(),
            "missing closure evidence must be an error"
        );
        for invalid in [
            r#"{}"#,
            r#"{"connections":[{"id":"a"}]}"#,
            r#"{"connections":[{"chains":["relay"]}]}"#,
            r#"{"connections":[{"id":"a","chains":[null]}]}"#,
        ] {
            assert!(relay_connection_ids(invalid).is_err());
        }
        assert_eq!(
            relay_connection_ids(
                r#"{"connections":[{"id":"probe","chains":["aec-relay-transport"]}]}"#
            )
            .unwrap(),
            ["probe"]
        );
        assert!(relay_connection_ids(r#"{"connections":[]}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn local_put_encodes_names_and_sends_a_json_body() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let header = String::from_utf8(request).unwrap();
            assert!(header.starts_with("PUT /proxies/AI%20%2F%E7%BB%84 HTTP/1.1\r\n"));
            assert!(header.contains("Authorization: Bearer test-token\r\n"));
            let len: usize = header
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; len];
            stream.read_exact(&mut body).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["name"],
                "member\"x"
            );
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let path = format!("/proxies/{}", encode_segment("AI /组"));
        let body = serde_json::json!({"name":"member\"x"}).to_string();
        assert_eq!(
            http_local(port, "PUT", &path, Some("test-token"), &body)
                .unwrap()
                .0,
            204
        );
        server.join().unwrap();
    }

    #[test]
    #[ignore = "needs runtime/bin/aec-mihomo.exe"]
    fn imported_routing_validates_and_restores_saved_selection() {
        use crate::profile::{Candidate, ProfileSet, UpstreamProto};
        let (mut m, root) = temp_manager("import-cycle");
        m.ports = CorePorts {
            mixed: 30893,
            controller: 30909,
            dns: 30553,
            relay: 30325,
            tunnel: 30326,
        };
        let plan = crate::import::parse(include_str!("../../tests/fixtures/routing.yaml"), "test")
            .unwrap();
        let mut p = ProfileSet::default().agent.remove(0);
        p.egress.candidates.push(Candidate {
            id: "exit".into(),
            host: "198.51.100.1".into(),
            port: 12324,
            proto: UpstreamProto::Socks5,
            credential_ref: None,
        });
        p.routing = plan.routing;
        p.routing.groups[0].selected = Some("relay".into());
        struct Creds;
        impl config::CredentialSource for Creds {
            fn get(&self, _: &str) -> Option<(String, String)> {
                Some(("fixture-user".into(), "fixture-password".into()))
            }
        }
        let result = std::panic::catch_unwind(|| {
            for _ in 0..2 {
                m.up(&WindowsHost, |ports, secret| {
                    config::from_profile(&p, ports, secret, &Creds).map_err(|e| e.to_string())
                })
                .unwrap();
                m.apply_selections(&WindowsHost, &p.routing).unwrap();
                let secret = secret_from(&fs::read_to_string(m.config_path()).unwrap()).unwrap();
                let (status, body) =
                    http_get_local(m.ports.controller, "/proxies/AI%20sites", Some(&secret))
                        .unwrap();
                assert_eq!(status, 200);
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&body).unwrap()["now"],
                    "relay"
                );
                m.down(&WindowsHost).unwrap();
            }
        });
        let _ = m.down(&WindowsHost);
        let _ = fs::remove_dir_all(root);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn reads_secret_line() {
        assert_eq!(
            secret_from("a: 1\nsecret: \"abc\"\n").as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn desktop_observation_never_contacts_a_foreign_controller_or_reused_pid() {
        use crate::testing::FakeHost;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mut m, root) = temp_manager("observe-foreign");
        m.ports.controller = listener.local_addr().unwrap().port();
        let (exe, created) = process_identity(std::process::id()).unwrap();
        let mut owned = OwnedProcess {
            role: "core".into(),
            pid: std::process::id(),
            exe: exe.clone(),
            created,
            started_at_unix_ms: now_ms(),
        };
        let mut host = FakeHost::default();
        host.listeners.insert(
            m.ports.mixed,
            crate::model::ListenerOwner {
                port: m.ports.mixed,
                pid: owned.pid,
                exe: Some(exe),
            },
        );
        host.listeners.insert(
            m.ports.controller,
            crate::model::ListenerOwner {
                port: m.ports.controller,
                pid: 99999,
                exe: Some(r"C:\other\mihomo.exe".into()),
            },
        );
        fs::create_dir_all(m.core_dir()).unwrap();
        fs::write(m.owned_path(), serde_json::to_vec(&owned).unwrap()).unwrap();
        assert_eq!(m.connections(&host).unwrap_err().code, "PORT_IN_USE");
        owned.created += 1;
        fs::write(m.owned_path(), serde_json::to_vec(&owned).unwrap()).unwrap();
        assert_eq!(m.connections(&host).unwrap_err().code, "CORE_NOT_RUNNING");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "needs runtime/bin/aec-mihomo.exe; local sockets and read-only Windows TCP probe"]
    fn desktop_connection_snapshot_correlates_with_native_tcp_identity() {
        let (mut m, root) = temp_manager("desktop-observe");
        m.ports = CorePorts {
            mixed: 31893,
            controller: 31909,
            dns: 31553,
            relay: 31325,
            tunnel: 31326,
        };
        let result = std::panic::catch_unwind(|| {
            m.up(&WindowsHost, |ports, secret| {
                let mut y = config::direct_test(ports, secret, "agent-egress")
                    .replace("use-hosts: false", "use-hosts: true");
                // Local-only fake destination, not a request to Anthropic or a real Claude session.
                y.push_str("hosts:\n  'claude.ai': 127.0.0.1\n");
                Ok(y)
            })
            .unwrap();
            let destination = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut client = TcpStream::connect(("127.0.0.1", m.ports.mixed)).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            write!(
                client,
                "CONNECT claude.ai:{} HTTP/1.1\r\nHost: claude.ai\r\n\r\n",
                destination.local_addr().unwrap().port()
            )
            .unwrap();
            let mut response = Vec::new();
            while !response.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                client.read_exact(&mut b).unwrap();
                response.push(b[0]);
            }
            assert!(String::from_utf8(response).unwrap().contains("200"));
            // mihomo CONNECT is lazy: send a byte to trigger the upstream connection.
            client.write_all(b"x").unwrap();
            destination.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let (_peer, _) = loop {
                match destination.accept() {
                    Ok(peer) => break peer,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "local CONNECT did not dial destination within 5 seconds"
                        );
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => panic!("local destination accept failed: {error}"),
                }
            };
            let records =
                crate::desktop::parse_connections(&m.connections(&WindowsHost).unwrap()).unwrap();
            let local = client.local_addr().unwrap();
            assert!(records
                .iter()
                .any(|c| c.metadata.source_port == local.port().to_string()
                    && c.metadata.inbound_port == m.ports.mixed.to_string()
                    && c.metadata.host == "claude.ai"));
            let flows = crate::windows::desktop_proxy_flows(m.ports.mixed).unwrap();
            assert!(flows
                .iter()
                .any(|f| f.local == local && f.pid == std::process::id() && f.created != 0));
            let p = crate::profile::ProfileSet::default().agent.remove(0);
            let r = crate::desktop::observe(
                &p,
                m.ports.mixed,
                &crate::desktop::WindowsDesktopProbe(&m),
            );
            assert_eq!(
                r.status,
                crate::checks::Status::Unknown,
                "a test runner is not Claude Desktop"
            );
        });
        let _ = m.down(&WindowsHost);
        let _ = fs::remove_dir_all(root);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// PORT_IN_USE: a port held by another process is reported, and that process keeps its port.
    #[test]
    fn foreign_listener_blocks_start_and_survives() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (mut m, root) = temp_manager("busy");
        m.ports = CorePorts {
            mixed: port,
            controller: port,
            dns: port,
            relay: 0,
            tunnel: 0,
        };
        if m.binary_state() != BinaryState::Ok {
            // Without the binary the check stops earlier; exercise preflight directly instead.
            let r = preflight::check(&WindowsHost, &[port], None).unwrap();
            assert!(matches!(r[0].1, PortState::Foreign(_)));
        } else {
            let err = m
                .up(&WindowsHost, |p, s| {
                    Ok(config::direct_test(p, s, "agent-egress"))
                })
                .unwrap_err();
            assert_eq!(err.code, "PORT_IN_USE", "{err}");
        }
        assert!(listener.local_addr().is_ok());
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_ok(),
            "the foreign listener must still accept"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Full cycle against the real binary on alternative ports. Needs `runtime/bin/aec-mihomo.exe`
    /// (`aec-dev core install <exe>`); run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs runtime/bin/aec-mihomo.exe"]
    fn starts_and_stops_direct_test_core() {
        let (mut m, root) = temp_manager("cycle");
        m.ports = CorePorts {
            mixed: 27893,
            controller: 27909,
            dns: 27553,
            relay: 27325,
            tunnel: 27326,
        };
        assert_eq!(m.binary_state(), BinaryState::Ok);
        let owned = m
            .up(&WindowsHost, |p, s| {
                Ok(config::direct_test(p, s, "agent-egress"))
            })
            .unwrap();
        let st = m.status(&WindowsHost);
        assert!(
            st.alive && st.ports.iter().all(|(_, s)| s == "ours"),
            "{st:?}"
        );
        assert!(
            st.version.as_deref().is_some_and(|v| v.contains("version")),
            "{st:?}"
        );
        let again = m
            .up(&WindowsHost, |_, _| Err("must not regenerate".into()))
            .unwrap();
        assert_eq!(
            again.pid, owned.pid,
            "a running core is adopted, not started twice"
        );
        assert!(m.down(&WindowsHost).unwrap());
        assert!(
            !m.config_path().exists(),
            "the generated config (with its secret) is deleted"
        );
        let st = m.status(&WindowsHost);
        assert!(
            !st.alive && st.ports.iter().all(|(_, s)| s == "free"),
            "{st:?}"
        );
        let _ = fs::remove_dir_all(root);
    }
}
