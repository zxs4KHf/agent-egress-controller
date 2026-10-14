//! On-disk storage inside the application's data directory. Nothing here writes anywhere else.
//!
//! Every write goes to a temporary file in the same directory, is flushed to disk, and then
//! replaces the target in one rename, so an interrupted write leaves either the old file or the new
//! one, never half of either.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::profile::{
    FallbackUpstream, ProfileSet, Routing, RoutingOutbound, RoutingOutboundKind, Target,
    UpstreamProto, TEMPLATE_SPLIT,
};
use crate::snapshot::{is_snapshot_id, snapshot_id, EnvironmentSnapshot};

/// Read-only history kept next to the current snapshot (user decision 2026-10-10).
pub const SNAPSHOT_HISTORY_LIMIT: usize = 10;
const TMP_MARKER: &str = ".tmp-";
const CURRENT_POINTER: &str = "current";

#[derive(Debug)]
pub enum StoreError {
    Io { path: PathBuf, source: io::Error },
    Parse { path: PathBuf, message: String },
    Missing { path: PathBuf },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            StoreError::Parse { path, message } => write!(f, "{}: {message}", path.display()),
            StoreError::Missing { path } => write!(f, "{}: not found", path.display()),
        }
    }
}

impl std::error::Error for StoreError {}

/// One owner of an installation's mutable session at a time. The OS releases ownership on exit,
/// including a crash; the empty marker file itself never signifies an unfinished session.
pub struct SessionLease {
    _file: fs::File,
}

impl SessionLease {
    pub fn acquire(data_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(data_dir).map_err(|e| format!("SESSION_LOCK: {e}"))?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(data_dir.join("session.lock"))
            .map_err(|e| format!("SESSION_LOCK: {e}"))?;
        file.try_lock().map_err(|e| match e {
            fs::TryLockError::WouldBlock => {
                "SESSION_BUSY: 本安装的会话正在使用中；请在托盘重新检测或正常退出，再运行修改命令。"
                    .into()
            }
            fs::TryLockError::Error(e) => format!("SESSION_LOCK: {e}"),
        })?;
        Ok(Self { _file: file })
    }
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Write `bytes` to `path` atomically.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(io_err(dir))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!("{name}{TMP_MARKER}{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp).map_err(io_err(&tmp))?;
        f.write_all(bytes).map_err(io_err(&tmp))?;
        f.sync_all().map_err(io_err(&tmp))?;
    }
    // On Windows std's rename replaces an existing target (MoveFileExW with REPLACE_EXISTING).
    fs::rename(&tmp, path).map_err(|source| {
        let _ = fs::remove_file(&tmp);
        StoreError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, StoreError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(StoreError::Missing {
                path: path.to_path_buf(),
            })
        }
        Err(e) => {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source: e,
            })
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| StoreError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

fn to_json<T: Serialize>(value: &T) -> Vec<u8> {
    let mut v = serde_json::to_vec_pretty(value).expect("model types serialize");
    v.push(b'\n');
    v
}

pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("profiles").join("profiles.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<ProfileSet, StoreError> {
        let mut value: serde_json::Value = read_json(&self.path)?;
        // v0.1.0 stored runtime template fields. Convert them in memory before strict deserialization;
        // the next normal save writes only the new data model. Never mutate the file on read.
        if let Some(agents) = value.get_mut("agent").and_then(|v| v.as_array_mut()) {
            for agent in agents {
                let targets: Vec<Target> =
                    serde_json::from_value(agent.get("targets").cloned().unwrap_or_default())
                        .map_err(|e| StoreError::Parse {
                            path: self.path.clone(),
                            message: e.to_string(),
                        })?;
                let Some(old) = agent.get("routing") else {
                    continue;
                };
                if old.get("rulesTemplate").is_none() {
                    continue;
                }
                let old: LegacyRouting =
                    serde_json::from_value(old.clone()).map_err(|e| StoreError::Parse {
                        path: self.path.clone(),
                        message: e.to_string(),
                    })?;
                if old.rules_template != "default-v1" && old.rules_template != TEMPLATE_SPLIT {
                    return Err(StoreError::Parse {
                        path: self.path.clone(),
                        message: "unknown legacy routing template".into(),
                    });
                }
                let outbound = old.fallback.map(|f| RoutingOutbound {
                    cipher: None,
                    dialer_proxy: None,
                    name: "daily-proxy".into(),
                    server: f.host,
                    port: f.port,
                    credential_ref: f.credential_ref,
                    kind: match f.proto {
                        UpstreamProto::Http => RoutingOutboundKind::Http,
                        UpstreamProto::Socks5 => RoutingOutboundKind::Socks5,
                    },
                });
                let route = Routing::preset(
                    &old.target_group,
                    &targets,
                    old.rules_template == TEMPLATE_SPLIT,
                    &old.extra_domains,
                    old.domestic_direct,
                    outbound,
                );
                agent["routing"] = serde_json::to_value(route).expect("routing serializes");
            }
        }
        serde_json::from_value(value).map_err(|e| StoreError::Parse {
            path: self.path.clone(),
            message: e.to_string(),
        })
    }

    /// Saves as given; validation is separate so that a profile with settings gaps can be kept.
    pub fn save(&self, set: &ProfileSet) -> Result<(), StoreError> {
        write_atomic(&self.path, &to_json(set))
    }

    /// Write the default profiles if no profile file exists. Returns whether it wrote.
    pub fn init_if_missing(&self) -> Result<bool, StoreError> {
        if self.path.exists() {
            return Ok(false);
        }
        self.save(&ProfileSet::default())?;
        Ok(true)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyRouting {
    target_group: String,
    rules_template: String,
    #[serde(default)]
    fallback: Option<FallbackUpstream>,
    #[serde(default)]
    domestic_direct: bool,
    #[serde(default)]
    extra_domains: Vec<String>,
}

#[derive(Debug)]
pub struct SnapshotEntry {
    pub id: String,
    pub current: bool,
    /// `Err` holds why the file could not be read; such a file is shown, not hidden.
    pub snapshot: Result<EnvironmentSnapshot, String>,
}

pub struct SnapshotStore {
    dir: PathBuf,
}

impl SnapshotStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("snapshots"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// A fresh id that does not collide with an existing snapshot.
    pub fn next_id(&self) -> (String, u64) {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut seq = 0;
        loop {
            let id = snapshot_id(ms, seq);
            if !self.file(&id).exists() {
                return (id, ms);
            }
            seq += 1;
        }
    }

    pub fn current_id(&self) -> Result<Option<String>, StoreError> {
        let p = self.dir.join(CURRENT_POINTER);
        match fs::read_to_string(&p) {
            Ok(s) => {
                let id = s.trim().to_string();
                if is_snapshot_id(&id) {
                    Ok(Some(id))
                } else {
                    Err(StoreError::Parse {
                        path: p,
                        message: format!("not a snapshot id: '{id}'"),
                    })
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Io { path: p, source: e }),
        }
    }

    pub fn load(&self, id: &str) -> Result<EnvironmentSnapshot, StoreError> {
        read_json(&self.file(id))
    }

    /// Persist a snapshot, optionally make it the current one, then prune history.
    /// Returns the ids that pruning deleted.
    pub fn save(
        &self,
        snap: &EnvironmentSnapshot,
        make_current: bool,
    ) -> Result<Vec<String>, StoreError> {
        write_atomic(&self.file(&snap.id), &to_json(snap))?;
        if make_current {
            write_atomic(
                &self.dir.join(CURRENT_POINTER),
                format!("{}\n", snap.id).as_bytes(),
            )?;
        }
        self.prune()
    }

    /// Ids of stored snapshots, oldest first. Leftover temporary files are not snapshots.
    pub fn ids(&self) -> Result<Vec<String>, StoreError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(StoreError::Io {
                    path: self.dir.clone(),
                    source: e,
                })
            }
        };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|n| n.strip_suffix(".json"))
                    .map(str::to_string)
            })
            .filter(|id| is_snapshot_id(id))
            .collect();
        ids.sort();
        Ok(ids)
    }

    pub fn list(&self) -> Result<Vec<SnapshotEntry>, StoreError> {
        let current = self.current_id().ok().flatten();
        Ok(self
            .ids()?
            .into_iter()
            .map(|id| SnapshotEntry {
                current: current.as_deref() == Some(id.as_str()),
                snapshot: self.load(&id).map_err(|e| e.to_string()),
                id,
            })
            .collect())
    }

    /// Keep the current snapshot plus the newest `SNAPSHOT_HISTORY_LIMIT` others; delete the rest and
    /// any temporary files this process left behind earlier.
    fn prune(&self) -> Result<Vec<String>, StoreError> {
        let current = self.current_id()?;
        let history: Vec<String> = self
            .ids()?
            .into_iter()
            .filter(|id| Some(id) != current.as_ref())
            .collect();
        let excess = history.len().saturating_sub(SNAPSHOT_HISTORY_LIMIT);
        let mut removed = Vec::new();
        for id in history.into_iter().take(excess) {
            let p = self.file(&id);
            fs::remove_file(&p).map_err(io_err(&p))?;
            removed.push(id);
        }
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().contains(TMP_MARKER) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::collect;
    use crate::testing::FakeHost;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "aec-store-{tag}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn snap(store: &SnapshotStore) -> EnvironmentSnapshot {
        let (id, ms) = store.next_id();
        EnvironmentSnapshot::from_status(
            id,
            ms,
            &collect(&FakeHost::default(), None),
            None,
            crate::snapshot::Enabled::default(),
        )
    }

    #[test]
    fn profiles_init_save_load() {
        let t = TempDir::new("profiles");
        let s = ProfileStore::new(&t.0);
        assert!(matches!(s.load(), Err(StoreError::Missing { .. })));
        assert!(s.init_if_missing().unwrap());
        assert!(
            !s.init_if_missing().unwrap(),
            "must not overwrite an existing profile file"
        );
        assert_eq!(s.load().unwrap(), ProfileSet::default());
    }

    #[test]
    fn old_templates_migrate_in_memory_and_only_a_save_replaces_fields() {
        let t = TempDir::new("migration");
        let store = ProfileStore::new(&t.0);
        let mut value = serde_json::to_value(ProfileSet::default()).unwrap();
        value["agent"][0]["routing"] = serde_json::json!({"targetGroup":"agent-egress", "rulesTemplate":"split-v1",
            "fallback":{"host":"127.0.0.1", "port":7890, "proto":"socks5", "credentialRef":"daily"}, "domesticDirect":false,"extraDomains":["example.com"]});
        let original = serde_json::to_vec(&value).unwrap();
        write_atomic(store.path(), &original).unwrap();
        let set = store.load().unwrap();
        assert_eq!(
            fs::read(store.path()).unwrap(),
            original,
            "loading is read-only"
        );
        assert_eq!(
            set.agent[0].routing.groups[1].members,
            ["daily-proxy", "DIRECT"]
        );
        assert_eq!(
            set.agent[0].routing.outbounds[0].credential_ref.as_deref(),
            Some("daily")
        );
        store.save(&set).unwrap();
        let saved = fs::read_to_string(store.path()).unwrap();
        assert!(!saved.contains("rulesTemplate") && !saved.contains("targetGroup"));
        assert_eq!(store.load().unwrap(), set);
    }

    #[test]
    fn keeps_current_plus_ten_after_twelve() {
        let t = TempDir::new("retention");
        let store = SnapshotStore::new(&t.0);
        let mut ids = Vec::new();
        for _ in 0..12 {
            let s = snap(&store);
            ids.push(s.id.clone());
            store.save(&s, true).unwrap();
        }
        let left = store.ids().unwrap();
        assert_eq!(left.len(), 1 + SNAPSHOT_HISTORY_LIMIT);
        assert_eq!(
            store.current_id().unwrap().as_deref(),
            Some(ids[11].as_str())
        );
        assert!(!left.contains(&ids[0]), "the oldest one is pruned");
        assert_eq!(left, ids[1..].to_vec());
    }

    #[test]
    fn current_survives_pruning_even_when_old() {
        let t = TempDir::new("pinned");
        let store = SnapshotStore::new(&t.0);
        let first = snap(&store);
        store.save(&first, true).unwrap();
        for _ in 0..12 {
            let s = snap(&store);
            store.save(&s, false).unwrap();
        }
        let left = store.ids().unwrap();
        assert!(left.contains(&first.id));
        assert_eq!(left.len(), 1 + SNAPSHOT_HISTORY_LIMIT);
    }

    #[test]
    fn interrupted_writes_never_look_like_snapshots() {
        let t = TempDir::new("torn");
        let store = SnapshotStore::new(&t.0);
        let good = snap(&store);
        store.save(&good, true).unwrap();
        // An interrupted atomic write leaves only a temporary file next to the intact target.
        fs::write(
            store.dir().join(format!("{}.json{TMP_MARKER}999", good.id)),
            b"{\"half\":",
        )
        .unwrap();
        // A file damaged by something else is reported, not hidden and not treated as valid.
        let (bad_id, _) = store.next_id();
        fs::write(
            store.dir().join(format!("{bad_id}.json")),
            b"{\"schemaVersion\": 1, \"id\"",
        )
        .unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list
            .iter()
            .any(|e| e.id == good.id && e.current && e.snapshot.is_ok()));
        assert!(list.iter().any(|e| e.id == bad_id && e.snapshot.is_err()));
        assert_eq!(store.load(&good.id).unwrap(), good);
        // The next save clears the leftover temporary file.
        store.save(&snap(&store), false).unwrap();
        assert!(!fs::read_dir(store.dir())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(TMP_MARKER)));
    }

    #[test]
    fn write_atomic_replaces_existing_file() {
        let t = TempDir::new("atomic");
        let p = t.0.join("x.json");
        write_atomic(&p, b"1").unwrap();
        write_atomic(&p, b"2").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"2");
    }
}
