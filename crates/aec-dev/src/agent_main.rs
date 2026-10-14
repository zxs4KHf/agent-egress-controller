//! Supported public bridge; no credentials in command arguments, environment or stdout.
#[cfg(windows)]
fn run() -> Result<(), String> {
    use aec_core::agent;
    use std::io::{Read, Write};
    let mut args = std::env::args().skip(1);
    let exe = std::env::current_exe().map_err(|_| "executable unavailable")?;
    let dir = exe.parent().ok_or("executable directory unavailable")?;
    let first = args.next().unwrap_or_else(|| "mcp".into());
    let (data, command) = if first == "--data-dir" {
        (
            std::path::PathBuf::from(args.next().ok_or("missing data directory")?),
            args.next().unwrap_or_else(|| "mcp".into()),
        )
    } else {
        let data = if dir.join("portable.flag").is_file() {
            dir.join("data")
        } else if dir.starts_with(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()
                .unwrap_or_default()
                .join("target"),
        ) {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/dev-data")
        } else {
            std::path::PathBuf::from(
                std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA unavailable")?,
            )
            .join("AgentEgressController")
        };
        (data, first)
    };
    let data = if data.is_absolute() {
        data
    } else {
        std::env::current_dir()
            .map_err(|_| "current directory unavailable")?
            .join(data)
    };
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    match command.as_str() {
        "mcp" => agent::serve_mcp(std::io::stdin().lock(), std::io::stdout().lock(), |r| {
            agent::local_call(&data, r)
        })
        .map_err(|_| "MCP transport failed".into()),
        "config" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&agent::config(&data, &exe)).unwrap()
            );
            Ok(())
        }
        "call" => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take(128 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "stdin unavailable")?;
            if bytes.len() > 128 * 1024 {
                return Err("request too large".into());
            }
            let request =
                serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|_| "invalid JSON")?;
            bytes.fill(0);
            let result = agent::local_call(&data, &request)?;
            writeln!(std::io::stdout(), "{result}").map_err(|_| "stdout unavailable".into())
        }
        _ => Err(
            "Usage: aec-agent [--data-dir PATH] [mcp|config|call]; call reads JSON from stdin"
                .into(),
        ),
    }
}
fn main() {
    #[cfg(windows)]
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        eprintln!("AEC requires Windows");
        std::process::exit(1);
    }
}
