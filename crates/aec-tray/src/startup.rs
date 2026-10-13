//! One per-user startup value, pointing at this exact installation. No system proxy writes.
use windows_sys::Win32::System::Registry::*;

const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "AgentEgressController";
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = aec_core::paths::strip_verbatim(&exe);
    Ok(format!("\"{}\" --enter", exe.display()))
}
fn current() -> Result<Option<String>, String> {
    let (key, name) = (wide(KEY), wide(NAME));
    let mut buf = vec![0u16; 32768];
    let mut len = (buf.len() * 2) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    match result {
        0 => Ok(Some(String::from_utf16_lossy(
            &buf[..(len as usize / 2).saturating_sub(1)],
        ))),
        2 => Ok(None),
        _ => Err(format!("读取登录自启失败：{result}")),
    }
}
pub fn enabled() -> bool {
    matches!((current(),command()),(Ok(Some(a)),Ok(b)) if a==b)
}
pub fn set(enabled: bool) -> Result<(), String> {
    let desired = command()?;
    if current()?.is_some_and(|c| c != desired) {
        return Err("登录自启指向另一份安装；请先在那份安装关闭自启".into());
    }
    let (key, name) = (wide(KEY), wide(NAME));
    let mut handle = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut handle,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(format!("打开登录自启设置失败：{status}"));
    }
    let value = wide(&desired);
    let status = unsafe {
        let result = if enabled {
            RegSetValueExW(
                handle,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(handle, name.as_ptr())
        };
        RegCloseKey(handle);
        result
    };
    if status == 0 || (!enabled && status == 2) {
        Ok(())
    } else {
        Err(format!("保存登录自启失败：{status}"))
    }
}

/// WScript.Shell serializes .lnk files. Paths travel as environment values, never shell code.
/// Refuse an existing link with another target instead of overwriting a user's shortcut.
pub fn shortcuts() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = aec_core::paths::strip_verbatim(&exe);
    let script = r#"$ErrorActionPreference='Stop'
$shell=New-Object -ComObject WScript.Shell
$destinations=@([Environment]::GetFolderPath('DesktopDirectory'),[Environment]::GetFolderPath('Programs'))
$plans=@()
foreach($directory in $destinations){
 if(-not $directory){throw 'Missing Windows folder'}
 foreach($entry in @(@('Agent Egress Controller.lnk',''),@('Agent Egress Dashboard.lnk','--dashboard'))){
  $path=Join-Path $directory $entry[0]
  $link=$shell.CreateShortcut($path)
  if((Test-Path -LiteralPath $path) -and ($link.TargetPath -ne $env:AEC_SHORTCUT_EXE -or $link.Arguments -ne $entry[1])){throw 'Existing shortcut belongs to another installation'}
  $plans+=@(@{Link=$link;Args=$entry[1]})
 }
}
foreach($plan in $plans){
 $link=$plan.Link
 $link.TargetPath=$env:AEC_SHORTCUT_EXE
 $link.Arguments=$plan.Args
 $link.WorkingDirectory=Split-Path -Parent $env:AEC_SHORTCUT_EXE
 $link.IconLocation=$env:AEC_SHORTCUT_EXE+',0'
 $link.Description='Agent Egress Controller - ISP paths and routing rules'
 $link.Save()
}"#;
    let powershell = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .ok_or("Windows 目录不可用")?
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let status = Command::new(powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("AEC_SHORTCUT_EXE", exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .status()
        .map_err(|_| "创建快捷方式失败")?;
    if status.success() {
        Ok(())
    } else {
        Err("创建快捷方式失败：请检查文件权限，或是否已有指向另一份安装的同名快捷方式".into())
    }
}
