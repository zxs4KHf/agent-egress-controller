//! Regression: `aec-dev core up` must return even when its output goes to a pipe.
//!
//! Before the fix the long-lived core inherited the write end of the caller's pipe, so a caller
//! reading our output (`aec-dev core up --direct | Out-Null`) waited until the core exited.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PORTS: &str = "28893,28909,28553";

fn aec_dev(data: &PathBuf, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aec-dev"));
    c.args(args)
        .env("AEC_DEV_DATA_DIR", data)
        .env("AEC_DEV_PORTS", PORTS);
    c
}

#[test]
#[ignore = "needs runtime/bin/aec-mihomo.exe"]
fn core_up_returns_when_stdout_is_a_pipe() {
    let data = std::env::temp_dir().join(format!("aec-pipe-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();

    let mut child = aec_dev(&data, &["core", "up", "--direct"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    // Read on a thread: the read only finishes when every holder of the pipe's write end is gone.
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        out.read_to_string(&mut s).map(|_| s)
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    while !reader.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let finished = reader.is_finished();

    // Always clean up the core, whether or not the assertion below holds.
    let down = aec_dev(&data, &["core", "down"]).output().unwrap();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&data);

    assert!(
        finished,
        "stdout pipe still open 30s after `core up`: the core inherited it"
    );
    let text = reader.join().unwrap().unwrap();
    assert!(text.contains("core running"), "{text}");
    assert!(String::from_utf8_lossy(&down.stdout).contains("core stopped"));
}

#[test]
fn core_commands_use_the_saved_mixed_port() {
    let data = std::env::temp_dir().join(format!("aec-saved-port-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    aec_core::controller::Settings {
        takeover_enabled: false,
        core_mixed_port: Some(port),
        client_cooperation: false,
    }
    .save(&data)
    .unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_aec-dev"))
            .args(args)
            .env("AEC_DEV_DATA_DIR", &data)
            .env_remove("AEC_DEV_PORTS")
            .output()
            .unwrap()
    };
    let generated = invoke(&["core", "config", "--direct"]);
    let config = String::from_utf8_lossy(&generated.stdout);
    let _ = std::fs::remove_dir_all(&data);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    assert!(
        config.contains(&format!("mixed-port: {port}\n")),
        "saved port ignored by core command"
    );
}

#[test]
fn cli_mutations_cannot_overwrite_an_active_session() {
    let data = std::env::temp_dir().join(format!("aec-session-owner-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    // Model the tray's lifetime ownership using an OS lock, not a removable PID marker.
    let owner = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data.join("session.lock"))
        .unwrap();
    owner.try_lock().unwrap();
    let state = aec_core::controller::ControllerState {
        mode: aec_core::controller::Mode::RestoreIncomplete,
        ..Default::default()
    };
    let original = serde_json::to_vec(&state).unwrap();
    std::fs::write(data.join("state.json"), &original).unwrap();
    for args in [
        &["ack"][..],
        &["recheck"],
        &["exit"],
        &["enter"],
        &["core", "down"],
        &["core", "select", "test", "DIRECT"],
        &["profile", "init"],
    ] {
        let result = aec_dev(&data, args).output().unwrap();
        assert!(
            !result.status.success(),
            "CLI must refuse to mutate a tray-owned session: {args:?}"
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("SESSION_BUSY"));
        assert_eq!(
            std::fs::read(data.join("state.json")).unwrap(),
            original,
            "CLI overwrote active state"
        );
    }
    assert!(
        aec_dev(&data, &["state"])
            .output()
            .unwrap()
            .status
            .success(),
        "read-only state remains available"
    );
    drop(owner);
    assert!(
        aec_dev(&data, &["ack"]).output().unwrap().status.success(),
        "OS ownership is released when the holder exits"
    );
    let _ = std::fs::remove_dir_all(&data);
}
