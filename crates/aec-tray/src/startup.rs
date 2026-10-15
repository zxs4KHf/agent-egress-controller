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

/// Use the same native identity/shortcut implementation as the installer and control center.
pub fn shortcuts() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    aec_core::shell_identity::create_shortcuts(&exe, aec_core::shell_identity::ShortcutScope::All)
}
