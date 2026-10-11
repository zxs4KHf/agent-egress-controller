//! Structured JSONL log (SPEC 4.11): one file per UTC day, kept 14 days and at most 50 MB in total.
//! Callers pass already-redacted data; nothing here inspects values.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::snapshot::rfc3339_utc;

pub const KEEP_DAYS: usize = 14;
pub const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Serialize)]
struct Line<'a> {
    ts: String,
    level: Level,
    event: &'a str,
    mode: &'a str,
    phase: Option<&'a str>,
    #[serde(rename = "correlationId")]
    correlation_id: &'a str,
    data: &'a Value,
}

pub struct Logger {
    dir: PathBuf,
}

impl Logger {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("logs"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file_for(&self, unix_ms: u64) -> PathBuf {
        let day = rfc3339_utc(unix_ms / 1000)[..10].replace('-', "");
        self.dir.join(format!("aec-{day}.jsonl"))
    }

    /// Append one line. Logging must never break an operation, so failures are swallowed.
    #[allow(clippy::too_many_arguments)]
    pub fn write(
        &self,
        unix_ms: u64,
        level: Level,
        event: &str,
        mode: &str,
        phase: Option<&str>,
        correlation_id: &str,
        data: Value,
    ) {
        let line = Line {
            ts: format!(
                "{}.{:03}Z",
                &rfc3339_utc(unix_ms / 1000)[..19],
                unix_ms % 1000
            ),
            level,
            event,
            mode,
            phase,
            correlation_id,
            data: &data,
        };
        let Ok(mut text) = serde_json::to_string(&line) else {
            return;
        };
        text.push('\n');
        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file_for(unix_ms))
        {
            let _ = f.write_all(text.as_bytes());
        }
    }

    /// Delete the oldest log files beyond the age and size limits.
    pub fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(String, PathBuf, u64)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                (name.starts_with("aec-") && name.ends_with(".jsonl"))
                    .then(|| (name, e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)))
            })
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
        let mut total = 0u64;
        for (i, (_, path, len)) in files.iter().enumerate() {
            total += len;
            if i >= KEEP_DAYS || total > MAX_TOTAL_BYTES {
                let _ = fs::remove_file(path);
            }
        }
    }

    /// Last `n` lines across the newest files.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        files.sort();
        let mut lines: Vec<String> = Vec::new();
        for f in files.iter().rev() {
            let Ok(text) = fs::read_to_string(f) else {
                continue;
            };
            let mut chunk: Vec<String> = text.lines().map(str::to_string).collect();
            chunk.append(&mut lines);
            lines = chunk;
            if lines.len() >= n {
                break;
            }
        }
        let skip = lines.len().saturating_sub(n);
        lines.split_off(skip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("aec-log-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn writes_one_json_object_per_line() {
        let d = tmp("lines");
        let l = Logger::new(&d);
        l.write(
            1_791_701_261_123,
            Level::Info,
            "enter.start",
            "daily",
            None,
            "c1",
            serde_json::json!({"profile": "agent-default"}),
        );
        l.write(
            1_791_701_262_000,
            Level::Error,
            "enter.core",
            "entering",
            Some("core"),
            "c1",
            serde_json::json!({"code": "PORT_IN_USE"}),
        );
        let lines = l.tail(10);
        assert_eq!(lines.len(), 2);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["ts"], "2026-10-11T06:47:41.123Z");
        assert_eq!(v["correlationId"], "c1");
        let v: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(
            (
                v["level"].as_str(),
                v["phase"].as_str(),
                v["data"]["code"].as_str()
            ),
            (Some("error"), Some("core"), Some("PORT_IN_USE"))
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn prunes_by_age() {
        let d = tmp("prune");
        let l = Logger::new(&d);
        for day in 0..20u64 {
            l.write(
                1_790_000_000_000 + day * 86_400_000,
                Level::Info,
                "x",
                "daily",
                None,
                "c",
                Value::Null,
            );
        }
        l.prune();
        assert_eq!(fs::read_dir(l.dir()).unwrap().count(), KEEP_DAYS);
        fs::remove_dir_all(d).unwrap();
    }
}
