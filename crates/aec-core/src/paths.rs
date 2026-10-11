//! Where the application keeps its data (SPEC 4.2).

use std::path::{Path, PathBuf};

pub const APP_DIR_NAME: &str = "AgentEgressController";
pub const PORTABLE_FLAG: &str = "portable.flag";

/// Drop the `\\?\` prefix that `canonicalize` adds on Windows, so the path compares equal to the
/// plain paths that process queries return (`\\?\UNC\server\share` becomes `\\server\share`).
pub fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    /// Running from the repository during development.
    Dev {
        repo_root: PathBuf,
    },
    /// `portable.flag` sits next to the executable.
    Portable {
        exe_dir: PathBuf,
    },
    Installed {
        local_app_data: PathBuf,
    },
}

impl RunMode {
    /// Pick the mode for an executable. `repo_root` is set only by development builds.
    pub fn detect(exe_dir: &Path, repo_root: Option<&Path>, local_app_data: &Path) -> Self {
        if let Some(root) = repo_root {
            return RunMode::Dev {
                repo_root: root.to_path_buf(),
            };
        }
        if exe_dir.join(PORTABLE_FLAG).is_file() {
            return RunMode::Portable {
                exe_dir: exe_dir.to_path_buf(),
            };
        }
        RunMode::Installed {
            local_app_data: local_app_data.to_path_buf(),
        }
    }

    pub fn data_dir(&self) -> PathBuf {
        match self {
            RunMode::Dev { repo_root } => repo_root.join("runtime").join("dev-data"),
            RunMode::Portable { exe_dir } => exe_dir.join("data"),
            RunMode::Installed { local_app_data } => local_app_data.join(APP_DIR_NAME),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_per_mode() {
        let dev = RunMode::Dev {
            repo_root: PathBuf::from(r"D:\repo"),
        };
        assert_eq!(dev.data_dir(), PathBuf::from(r"D:\repo\runtime\dev-data"));
        let portable = RunMode::Portable {
            exe_dir: PathBuf::from(r"E:\aec"),
        };
        assert_eq!(portable.data_dir(), PathBuf::from(r"E:\aec\data"));
        let installed = RunMode::Installed {
            local_app_data: PathBuf::from(r"C:\Users\u\AppData\Local"),
        };
        assert_eq!(
            installed.data_dir(),
            PathBuf::from(r"C:\Users\u\AppData\Local\AgentEgressController")
        );
    }

    #[test]
    fn strips_verbatim_prefixes() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\D:\repo")),
            PathBuf::from(r"D:\repo")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\srv\share\x")),
            PathBuf::from(r"\\srv\share\x")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"D:\repo")),
            PathBuf::from(r"D:\repo")
        );
    }

    #[test]
    fn detect_prefers_dev_then_portable_flag() {
        let tmp = std::env::temp_dir().join(format!("aec-paths-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let lad = PathBuf::from(r"C:\L");
        assert!(matches!(
            RunMode::detect(&tmp, None, &lad),
            RunMode::Installed { .. }
        ));
        std::fs::write(tmp.join(PORTABLE_FLAG), b"").unwrap();
        assert!(matches!(
            RunMode::detect(&tmp, None, &lad),
            RunMode::Portable { .. }
        ));
        assert!(matches!(
            RunMode::detect(&tmp, Some(Path::new(r"D:\r")), &lad),
            RunMode::Dev { .. }
        ));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
