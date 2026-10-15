//! Read-only presentation resources, switched as one revision without restarting networking.
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Active {
    schema_version: u32,
    revision: String,
}

fn root() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = repo.canonicalize().unwrap_or(repo);
    if exe.starts_with(repo.join("target")) {
        repo.join("ui")
    } else {
        exe.parent().unwrap_or(Path::new(".")).join("ui")
    }
}

fn valid_revision(revision: &str) -> bool {
    !revision.is_empty()
        && revision.len() <= 80
        && revision
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn active_revision(root: &Path) -> Option<String> {
    let manifest = std::fs::read(root.join("active.json")).ok()?;
    if manifest.len() > 1024 {
        return None;
    }
    let active: Active = serde_json::from_slice(&manifest).ok()?;
    (active.schema_version == 2 && valid_revision(&active.revision)).then_some(active.revision)
}

fn read_revision(root: &Path, revision: &str, name: &str) -> Option<String> {
    if !matches!(
        name,
        "setup.html"
            | "help.html"
            | "control.html"
            | "dashboard.html"
            | "agent.html"
            | "theme.css"
            | "app.js"
            | "icon.svg"
    ) {
        return None;
    }
    if !valid_revision(revision) {
        return None;
    }
    let revisions = root.join("revisions").canonicalize().ok()?;
    let path = revisions.join(revision).join(name).canonicalize().ok()?;
    if !path.starts_with(&revisions) || std::fs::metadata(&path).ok()?.len() > 2 * 1024 * 1024 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn template_at(root: &Path, name: &str, fallback: &str) -> String {
    let (revision, body) = active_revision(root)
        .and_then(|revision| read_revision(root, &revision, name).map(|body| (revision, body)))
        .unwrap_or_else(|| ("bundled".into(), fallback.into()));
    // Pin CSS/JS/icons to the HTML's revision across subsequent HTTP requests.
    body.replace("/ui/", &format!("/ui/{revision}/"))
}

pub fn template(name: &str, fallback: &str) -> String {
    template_at(&root(), name, fallback)
}

pub fn asset(name: &str) -> Option<(&'static str, String)> {
    let (revision, name) = name
        .split_once('/')
        .map_or((None, name), |(r, n)| (Some(r), n));
    let (ctype, fallback) = match name {
        "theme.css" => (
            "text/css; charset=utf-8",
            include_str!("../../../ui/revisions/fluent-1/theme.css"),
        ),
        "app.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../../../ui/revisions/fluent-1/app.js"),
        ),
        "icon.svg" => (
            "image/svg+xml",
            include_str!("../../../ui/revisions/fluent-1/icon.svg"),
        ),
        _ => return None,
    };
    let body = match revision {
        Some("bundled") => fallback.into(),
        Some(revision) => read_revision(&root(), revision, name)?,
        None => active_revision(&root())
            .and_then(|r| read_revision(&root(), &r, name))
            .unwrap_or_else(|| fallback.into()),
    };
    Some((ctype, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changing_only_the_active_revision_hot_updates_presentation() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("aec-ui-{}-{stamp}", std::process::id()));
        for revision in ["old", "new"] {
            let dir = root.join("revisions").join(revision);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("agent.html"), revision).unwrap();
            std::fs::write(dir.join("theme.css"), revision).unwrap();
        }
        for revision in ["old", "new"] {
            std::fs::write(
                root.join("active.json"),
                format!(r#"{{"schemaVersion":2,"revision":"{revision}"}}"#),
            )
            .unwrap();
            assert_eq!(template_at(&root, "agent.html", "fallback"), revision);
        }
        assert_eq!(
            read_revision(&root, "old", "theme.css").as_deref(),
            Some("old"),
            "an in-flight page keeps its matching assets after a pointer swap"
        );
        assert!(read_revision(&root, "new", "../data/settings.json").is_none());
        std::fs::write(
            root.join("active.json"),
            r#"{"schemaVersion":3,"revision":"new"}"#,
        )
        .unwrap();
        assert!(active_revision(&root).is_none());
        assert_eq!(
            template_at(&root, "agent.html", "<link href='/ui/theme.css'>"),
            "<link href='/ui/bundled/theme.css'>"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
