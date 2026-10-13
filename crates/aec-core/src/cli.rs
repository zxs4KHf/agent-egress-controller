//! CLI entries (ticket 09): resolve `claude` / `codex` on PATH without going through an egress
//! shim, and start them in a new terminal with the proxy environment of this application's core.
//!
//! A CLI session is never ended by this application (SPEC 4.4 X1): on Windows 11 a new console can
//! be a tab of a shared Windows Terminal window, and closing that window would close other tabs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A directory holding this file is an egress shim directory (legacy `shim\`); resolving through it
/// would put a second proxy layer in front of the entry.
pub const SHIM_MARKER: &str = "_egress-env.cmd";

pub fn is_shim_dir(dir: &Path) -> bool {
    dir.join(SHIM_MARKER).is_file()
}

/// First match of `command` on `path_dirs`, trying each `pathext` extension (or the name as is when
/// it already has one), skipping directories `skip` rejects.
pub fn resolve(
    command: &str,
    path_dirs: &[PathBuf],
    pathext: &[String],
    exists: &dyn Fn(&Path) -> bool,
    skip: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let as_path = Path::new(command);
    if as_path.is_absolute() {
        let dir_ok = as_path.parent().is_none_or(|d| !skip(d));
        return (dir_ok && exists(as_path)).then(|| as_path.to_path_buf());
    }
    let has_ext = as_path.extension().is_some();
    for dir in path_dirs {
        if dir.as_os_str().is_empty() || skip(dir) {
            continue;
        }
        if has_ext {
            let p = dir.join(command);
            if exists(&p) {
                return Some(p);
            }
            continue;
        }
        for ext in pathext {
            let p = dir.join(format!("{command}{}", ext.to_ascii_lowercase()));
            if exists(&p) {
                return Some(p);
            }
        }
    }
    None
}

pub fn split_path(path: &str) -> Vec<PathBuf> {
    path.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(s.trim_matches('"')))
        .collect()
}

/// The environment a CLI entry runs with: the proxy variables legacy proved necessary (Node needs
/// NODE_USE_ENV_PROXY for its built-in fetch), PATH without shim directories, then the profile's
/// own variables, which win.
pub fn entry_env(
    proxy_port: u16,
    path_dirs: &[PathBuf],
    skip: &dyn Fn(&Path) -> bool,
    extra: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let proxy = format!("http://127.0.0.1:{proxy_port}");
    let no_proxy = "127.0.0.1,localhost,::1".to_string();
    let path = path_dirs
        .iter()
        .filter(|d| !skip(d))
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(";");
    let mut env = vec![
        ("HTTP_PROXY".to_string(), proxy.clone()),
        ("HTTPS_PROXY".to_string(), proxy.clone()),
        ("ALL_PROXY".to_string(), proxy.clone()),
        (
            "WSS_PROXY".to_string(),
            format!("ws://127.0.0.1:{proxy_port}"),
        ),
        ("NO_PROXY".to_string(), no_proxy.clone()),
        ("no_proxy".to_string(), no_proxy),
        ("NODE_USE_ENV_PROXY".to_string(), "1".to_string()),
        ("PATH".to_string(), path),
    ];
    for (k, v) in extra {
        env.retain(|(ek, _)| !ek.eq_ignore_ascii_case(k));
        env.push((k.clone(), v.clone()));
    }
    env
}

#[cfg(windows)]
pub use windows_impl::{launch_in_new_console, CREATE_NEW_CONSOLE};

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};

    pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

    /// `cmd /k "<exe>"` in a new console with `env` applied. Returns the cmd.exe pid.
    pub fn launch_in_new_console(
        exe: &Path,
        env: &[(String, String)],
        cwd: Option<&Path>,
    ) -> std::io::Result<u32> {
        crate::core::process::stop_std_handle_inheritance();
        let mut c = Command::new("cmd.exe");
        c.arg("/k").raw_arg(format!("\"\"{}\"\"", exe.display()));
        for (k, v) in env {
            c.env(k, v);
        }
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c.creation_flags(CREATE_NEW_CONSOLE);
        Ok(c.spawn()?.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn ext() -> Vec<String> {
        vec![".COM".into(), ".EXE".into(), ".BAT".into(), ".CMD".into()]
    }

    #[test]
    fn resolves_past_shim_directories() {
        let files: HashSet<PathBuf> = [
            r"C:\shim\claude.cmd",
            r"C:\shim\_egress-env.cmd",
            r"C:\npm\claude.cmd",
            r"C:\bin\codex.exe",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let exists = |p: &Path| files.contains(p);
        let skip = |d: &Path| files.contains(&d.join(SHIM_MARKER));
        let path = split_path(r#"C:\shim;"C:\npm";;C:\bin"#);
        assert_eq!(
            resolve("claude", &path, &ext(), &exists, &skip),
            Some(PathBuf::from(r"C:\npm\claude.cmd"))
        );
        assert_eq!(
            resolve("codex", &path, &ext(), &exists, &skip),
            Some(PathBuf::from(r"C:\bin\codex.exe"))
        );
        assert_eq!(
            resolve("claude", &path, &ext(), &exists, &|_| false),
            Some(PathBuf::from(r"C:\shim\claude.cmd")),
            "without the skip the shim would win"
        );
        assert_eq!(
            resolve("codex.exe", &path, &ext(), &exists, &skip),
            Some(PathBuf::from(r"C:\bin\codex.exe"))
        );
        assert_eq!(resolve("gemini", &path, &ext(), &exists, &skip), None);
        assert_eq!(
            resolve(r"C:\bin\codex.exe", &[], &ext(), &exists, &skip),
            Some(PathBuf::from(r"C:\bin\codex.exe")),
            "absolute paths are taken as they are"
        );
        assert_eq!(
            resolve(r"C:\shim\claude.cmd", &[], &ext(), &exists, &skip),
            None,
            "but never inside a shim directory"
        );
    }

    #[test]
    fn env_carries_proxy_and_drops_shims_from_path() {
        let path = split_path(r"C:\shim;C:\npm");
        let skip = |d: &Path| d == Path::new(r"C:\shim");
        let mut extra = BTreeMap::new();
        extra.insert("no_proxy".to_string(), "x".to_string());
        extra.insert("ANTHROPIC_LOG".to_string(), "debug".to_string());
        let env: BTreeMap<String, String> =
            entry_env(17893, &path, &skip, &extra).into_iter().collect();
        assert_eq!(env["HTTPS_PROXY"], "http://127.0.0.1:17893");
        assert_eq!(env["NODE_USE_ENV_PROXY"], "1");
        assert_eq!(env["PATH"], r"C:\npm");
        assert_eq!(env["no_proxy"], "x", "profile variables win");
        assert!(!env.contains_key("NO_PROXY") || env["NO_PROXY"] == "127.0.0.1,localhost,::1");
        assert_eq!(env["ANTHROPIC_LOG"], "debug");
    }

    /// The environment really reaches a child: run `cmd /c set` with it (no window) and read back.
    #[cfg(windows)]
    #[test]
    fn env_reaches_the_child_process() {
        // Do not depend on a maintainer's installed shim or a terminal's stale PATH.
        let fixture = std::env::temp_dir().join(format!("aec-cli-env-{}", std::process::id()));
        let shim = fixture.join("shim");
        let native = fixture.join("native");
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(shim.join(SHIM_MARKER), b"synthetic shim marker").unwrap();
        let path = vec![shim.clone(), native.clone()];
        let env = entry_env(17893, &path, &|d| is_shim_dir(d), &BTreeMap::new());
        let out = std::process::Command::new("cmd.exe")
            .args(["/c", "set"])
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .env(
                "SystemRoot",
                std::env::var("SystemRoot").unwrap_or_default(),
            )
            .output()
            .unwrap();
        std::fs::remove_dir_all(&fixture).unwrap();
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
        assert!(
            text.contains("https_proxy=http://127.0.0.1:17893"),
            "{text}"
        );
        assert!(text.contains("node_use_env_proxy=1"));
        let path_line = text
            .lines()
            .find(|l| l.starts_with("path="))
            .unwrap_or_default()
            .to_string();
        assert!(
            !path_line.contains(&shim.display().to_string().to_ascii_lowercase())
                && path_line.contains(&native.display().to_string().to_ascii_lowercase()),
            "shim must not be on the child's PATH: {path_line}"
        );
    }
}
