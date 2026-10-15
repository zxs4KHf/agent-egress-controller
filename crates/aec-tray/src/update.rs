//! User-initiated updater. Downloading never grants permission to stop networking.
use aec_core::controller::Mode;

pub fn launch(ui_only: bool, mode: Mode, busy: bool, data: &std::path::Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    if busy || !matches!(mode, Mode::Daily | Mode::AgentReady | Mode::AgentUnverified) {
        return Err("请等待当前操作结束并完成恢复后，再检查更新".into());
    }
    let exe = std::env::current_exe().map_err(|_| "无法定位程序目录")?;
    let script = exe.with_file_name("aec-update.ps1");
    if !script.is_file() {
        return Err("更新器文件不存在，请使用完整发布包".into());
    }
    let mut command = std::process::Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg("-CurrentPid")
        .arg(std::process::id().to_string())
        .arg("-CurrentVersion")
        .arg(env!("CARGO_PKG_VERSION"))
        .arg("-DataDir")
        .arg(aec_core::paths::strip_verbatim(data));
    if ui_only {
        command.arg("-UiOnly");
    }
    if mode != Mode::Daily {
        command.arg("-ResumeAgent");
    }
    command
        .arg("-OpenDashboard")
        .creation_flags(0x0800_0000)
        .spawn()
        .map_err(|_| "启动更新检查失败".to_string())?;
    Ok(())
}
