//! Windows shell identity is independent of the launching agent and of network state.
use std::path::{Path, PathBuf};

use ::windows::core::{Interface, GUID, HSTRING, PROPVARIANT};
use ::windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
use ::windows::Win32::Storage::EnhancedStorage::{
    PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchCommand,
    PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
};
use ::windows::Win32::System::Com::StructuredStorage::{PropVariantChangeType, PVCHF_DEFAULT};
use ::windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READWRITE,
};
use ::windows::Win32::System::Variant::VT_LPWSTR;
use ::windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, SHGetPropertyStoreForWindow, PROPERTYKEY,
};
use ::windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Programs, IShellLinkW, SHGetKnownFolderPath,
    SetCurrentProcessExplicitAppUserModelID, ShellLink, KF_FLAG_DEFAULT, SLGP_RAWPATH,
};

pub const CONTROL_APP_ID: &str = "AgentEgress.Controller";
pub const TRAY_APP_ID: &str = "AgentEgress.Tray";

/// The presentation singleton is scoped to its installation. An isolated test/install must never
/// focus or close another installation's authenticated window. The gateway remains a singleton.
pub fn control_mutex_name(exe: &Path) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in crate::paths::strip_verbatim(exe)
        .to_string_lossy()
        .to_lowercase()
        .bytes()
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    format!("Local\\AgentEgressControl-{hash:016x}")
}

pub fn find_control_window(exe: &Path) -> isize {
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId,
    };
    struct Search {
        expected: String,
        found: isize,
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> i32 {
        let search = &mut *(data as *mut Search);
        let mut title = [0u16; 128];
        let n = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        if n <= 0 || String::from_utf16_lossy(&title[..n as usize]) != "Agent Egress Controller" {
            return 1;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return 1;
        }
        let mut path = vec![0u16; 32768];
        let mut size = path.len() as u32;
        let read = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut size);
        CloseHandle(process);
        if read != 0
            && String::from_utf16_lossy(&path[..size as usize]).to_lowercase() == search.expected
        {
            search.found = hwnd as isize;
            0
        } else {
            1
        }
    }
    let mut search = Search {
        expected: crate::paths::strip_verbatim(exe)
            .to_string_lossy()
            .to_lowercase(),
        found: 0,
    };
    unsafe { EnumWindows(Some(visit), &mut search as *mut Search as isize) };
    search.found
}

pub fn initialize_tray_process() -> Result<(), String> {
    unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(TRAY_APP_ID)) }
        .map_err(|e| format!("设置托盘任务栏标识失败：{}", e.code()))
}

pub fn initialize_control_process() -> Result<(), String> {
    unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(CONTROL_APP_ID)) }
        .map_err(|e| format!("设置控制中心任务栏标识失败：{}", e.code()))
}

fn set_string(
    store: &IPropertyStore,
    key: &PROPERTYKEY,
    value: &str,
) -> ::windows::core::Result<()> {
    // Shell property schemas require VT_LPWSTR, not the BSTR produced by From<&str>.
    let mut text = PROPVARIANT::new();
    unsafe {
        PropVariantChangeType(
            &mut text,
            &PROPVARIANT::from(value),
            PVCHF_DEFAULT,
            VT_LPWSTR,
        )?;
        store.SetValue(key, &text)
    }
}

/// Set window identity before showing it; independent_launch must first clear host package identity.
/// The HWND is supplied by the caller's own window; no other application's window is modified.
pub fn configure_control_window(hwnd: isize, control_exe: &Path) -> Result<(), String> {
    let control_exe = crate::paths::strip_verbatim(control_exe);
    let tray = control_exe.with_file_name("aec-tray.exe");
    let configure = || -> ::windows::core::Result<()> {
        let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(HWND(hwnd as _))? };
        set_string(
            &store,
            &PKEY_AppUserModel_RelaunchCommand,
            &format!("\"{}\" --dashboard", tray.display()),
        )?;
        set_string(
            &store,
            &PKEY_AppUserModel_RelaunchIconResource,
            &format!("{},0", control_exe.display()),
        )?;
        set_string(
            &store,
            &PKEY_AppUserModel_RelaunchDisplayNameResource,
            "Agent Egress Controller",
        )?;
        // Set ID last so the taskbar sees complete relaunch information on its first notification.
        set_string(&store, &PKEY_AppUserModel_ID, CONTROL_APP_ID)
    };
    configure().map_err(|e| format!("设置控制中心窗口标识失败：{}", e.code()))
}

struct ComApartment(bool);
impl ComApartment {
    fn enter() -> ::windows::core::Result<Self> {
        let status = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if status == RPC_E_CHANGED_MODE {
            return Ok(Self(false));
        }
        status.ok()?;
        Ok(Self(true))
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn known_folder(id: &GUID) -> ::windows::core::Result<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)?;
        let result = path.to_string().map(PathBuf::from);
        CoTaskMemFree(Some(path.0.cast()));
        Ok(result?)
    }
}

#[derive(Clone, Copy)]
pub enum ShortcutScope {
    All,
    Desktop,
    StartMenu,
}

/// Installer sections select their own destinations. The tray preserves its established root
/// Start-menu location; existing installer entries are repaired too, never silently retargeted.
pub fn create_shortcuts(tray_exe: &Path, scope: ShortcutScope) -> Result<(), String> {
    let _com = ComApartment::enter().map_err(|e| e.code().to_string())?;
    let desktop = known_folder(&FOLDERID_Desktop).map_err(|e| e.code().to_string())?;
    let programs = known_folder(&FOLDERID_Programs).map_err(|e| e.code().to_string())?;
    let mut folders = match scope {
        ShortcutScope::All => vec![desktop, programs.clone()],
        ShortcutScope::Desktop => vec![desktop],
        ShortcutScope::StartMenu => vec![programs.join("Agent Egress")],
    };
    if matches!(scope, ShortcutScope::All)
        && programs
            .join("Agent Egress/Agent Egress Dashboard.lnk")
            .is_file()
    {
        folders.push(programs.join("Agent Egress"));
    }
    write_shortcuts(tray_exe, &folders)
}

fn utf16_text(buffer: &[u16]) -> String {
    String::from_utf16_lossy(&buffer[..buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len())])
}

fn same_target(actual: &str, expected: &Path) -> bool {
    // Shell links normalize separators and can expand Windows short paths. Resolve both actual
    // files before comparing; absence/unreadable paths is not evidence of ownership.
    match (
        std::fs::canonicalize(actual),
        std::fs::canonicalize(expected),
    ) {
        (Ok(a), Ok(b)) => a
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => false,
    }
}

/// Preflight every link before changing any of them. Tests pass only isolated temp folders.
fn write_shortcuts(tray_exe: &Path, folders: &[PathBuf]) -> Result<(), String> {
    let _com = ComApartment::enter().map_err(|e| e.code().to_string())?;
    let tray_exe = crate::paths::strip_verbatim(tray_exe);
    let control_exe = tray_exe.with_file_name("aec-control.exe");
    let operation = || -> Result<(), String> {
        let mut plans = Vec::new();
        for folder in folders {
            for (name, args, app_id, icon) in [
                ("Agent Egress Controller.lnk", "", TRAY_APP_ID, &tray_exe),
                (
                    "Agent Egress Dashboard.lnk",
                    "--dashboard",
                    CONTROL_APP_ID,
                    &control_exe,
                ),
            ] {
                let path = folder.join(name);
                let link: IShellLinkW =
                    unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }
                        .map_err(|e| e.code().to_string())?;
                let persist: IPersistFile = link.cast().map_err(|e| e.code().to_string())?;
                if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                    if metadata.file_type().is_symlink() {
                        return Err("拒绝覆盖链接形式的快捷方式".into());
                    }
                    unsafe { persist.Load(&HSTRING::from(path.as_os_str()), STGM_READWRITE) }
                        .map_err(|e| e.code().to_string())?;
                    let (mut target, mut arguments) = (vec![0u16; 32768], vec![0u16; 32768]);
                    unsafe {
                        link.GetPath(&mut target, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
                            .map_err(|e| e.code().to_string())?;
                        link.GetArguments(&mut arguments)
                            .map_err(|e| e.code().to_string())?;
                    }
                    if !same_target(&utf16_text(&target), &tray_exe)
                        || utf16_text(&arguments) != args
                    {
                        return Err(
                            "同名快捷方式属于其他安装或包含自定义参数；未覆盖任何入口".into()
                        );
                    }
                }
                plans.push((path, link, persist, args, app_id, icon));
            }
        }
        for (path, link, persist, args, app_id, icon) in plans {
            std::fs::create_dir_all(path.parent().expect("shortcut parent"))
                .map_err(|e| e.to_string())?;
            unsafe {
                link.SetPath(&HSTRING::from(tray_exe.as_os_str()))
                    .map_err(|e| e.code().to_string())?;
                link.SetArguments(&HSTRING::from(args))
                    .map_err(|e| e.code().to_string())?;
                link.SetWorkingDirectory(&HSTRING::from(
                    tray_exe.parent().ok_or("安装路径无父目录")?.as_os_str(),
                ))
                .map_err(|e| e.code().to_string())?;
                link.SetIconLocation(&HSTRING::from(icon.as_os_str()), 0)
                    .map_err(|e| e.code().to_string())?;
                link.SetDescription(&HSTRING::from(
                    "Agent Egress Controller - ISP paths and routing rules",
                ))
                .map_err(|e| e.code().to_string())?;
                let properties: IPropertyStore = link.cast().map_err(|e| e.code().to_string())?;
                set_string(&properties, &PKEY_AppUserModel_ID, app_id)
                    .map_err(|e| e.code().to_string())?;
                properties.Commit().map_err(|e| e.code().to_string())?;
                persist
                    .Save(&HSTRING::from(path.as_os_str()), true)
                    .map_err(|e| e.code().to_string())?;
            }
        }
        Ok(())
    };
    operation()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::windows::core::BSTR;
    use ::windows::Win32::System::Com::STGM_READ;

    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aec-shell-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn shortcuts_roundtrip_identity_icon_arguments_and_preserve_foreign_links() {
        let root = temp();
        let exe = root.join("中文 installation/aec-tray.exe");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"test fixture, never executed").unwrap();
        let dirs = vec![root.join("desktop"), root.join("programs")];
        write_shortcuts(&exe, &dirs).unwrap();
        let _com = ComApartment::enter().unwrap();
        for folder in &dirs {
            for (name, expected_id, args) in [
                ("Agent Egress Controller.lnk", TRAY_APP_ID, ""),
                ("Agent Egress Dashboard.lnk", CONTROL_APP_ID, "--dashboard"),
            ] {
                unsafe {
                    let link: IShellLinkW =
                        CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                    let file: IPersistFile = link.cast().unwrap();
                    file.Load(&HSTRING::from(folder.join(name).as_os_str()), STGM_READ)
                        .unwrap();
                    let props: IPropertyStore = link.cast().unwrap();
                    assert_eq!(
                        BSTR::try_from(&props.GetValue(&PKEY_AppUserModel_ID).unwrap())
                            .unwrap()
                            .to_string(),
                        expected_id
                    );
                    let mut buffer = vec![0u16; 32768];
                    link.GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
                        .unwrap();
                    assert!(same_target(&utf16_text(&buffer), &exe));
                    link.GetArguments(&mut buffer).unwrap();
                    assert_eq!(utf16_text(&buffer), args);
                    let mut index = -1;
                    link.GetIconLocation(&mut buffer, &mut index).unwrap();
                    assert_eq!(index, 0);
                    assert_eq!(
                        PathBuf::from(utf16_text(&buffer)),
                        if args.is_empty() {
                            exe.clone()
                        } else {
                            exe.with_file_name("aec-control.exe")
                        }
                    );
                }
            }
        }
        // Repeat repair preserves identity and is safe for this exact installation.
        write_shortcuts(&exe, &dirs).unwrap();
        let first = dirs[0].join("Agent Egress Controller.lnk");
        let before = std::fs::read(&first).unwrap();
        let result = write_shortcuts(&root.join("another/aec-tray.exe"), &dirs);
        assert!(result.is_err());
        assert_eq!(std::fs::read(&first).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn later_foreign_arguments_abort_the_whole_shortcut_batch() {
        let root = temp();
        let exe = root.join("aec-tray.exe");
        std::fs::write(&exe, b"fixture").unwrap();
        let dirs = vec![root.join("first"), root.join("last")];
        write_shortcuts(&exe, &dirs).unwrap();
        let _com = ComApartment::enter().unwrap();
        let first = dirs[0].join("Agent Egress Dashboard.lnk");
        let last = dirs[1].join("Agent Egress Dashboard.lnk");
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let file: IPersistFile = link.cast().unwrap();
            file.Load(&HSTRING::from(first.as_os_str()), STGM_READWRITE)
                .unwrap();
            let props: IPropertyStore = link.cast().unwrap();
            set_string(&props, &PKEY_AppUserModel_ID, "Legacy.Fixture").unwrap();
            props.Commit().unwrap();
            file.Save(&HSTRING::from(first.as_os_str()), true).unwrap();
            file.Load(&HSTRING::from(last.as_os_str()), STGM_READWRITE)
                .unwrap();
            link.SetArguments(&HSTRING::from("--dashboard --user-custom-argument"))
                .unwrap();
            file.Save(&HSTRING::from(last.as_os_str()), true).unwrap();
        }
        let before = std::fs::read(&first).unwrap();
        let foreign = std::fs::read(&last).unwrap();
        assert!(write_shortcuts(&exe, &dirs).is_err());
        assert_eq!(std::fs::read(&first).unwrap(), before);
        assert_eq!(std::fs::read(&last).unwrap(), foreign);
        std::fs::remove_dir_all(root).unwrap();
    }
}
