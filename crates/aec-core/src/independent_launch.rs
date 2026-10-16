//! Start presentation applications outside a packaged launching agent's identity.
//! No elevation, job breakaway, global changes, shell interpreter or network operations.
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use windows_sys::Win32::System::Environment::GetCommandLineW;
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapAlloc, HeapFree, HEAP_ZERO_MEMORY};
use windows_sys::Win32::System::Threading::*;

const MARKER: &str = "--aec-independent-launch";

pub fn arguments() -> impl Iterator<Item = std::ffi::OsString> {
    std::env::args_os().skip(1).filter(|arg| arg != MARKER)
}

/// Call before any UI, mutex, credentials or worker initialization.
/// A packaged bootstrap stays as a UI-less wait-only parent, so old update launchers still see
/// a running process and maintenance commands retain their exit status. Only the unpackaged
/// child owns windows, the controller and network workers. No handoff token is persisted.
pub fn enter() -> Result<Option<i32>, String> {
    let mut length = 0;
    let identity = unsafe { GetCurrentPackageFullName(&mut length, null_mut()) };
    if identity == APPMODEL_ERROR_NO_PACKAGE {
        return Ok(None);
    }
    if identity != ERROR_INSUFFICIENT_BUFFER {
        return Err(format!("读取启动身份失败（{identity}）"));
    }
    if std::env::args_os().any(|arg| arg == MARKER) {
        return Err("独立启动未能脱离宿主身份；已中止，未启动网络服务".into());
    }
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    let exe: Vec<_> = std::env::current_exe()
        .map_err(|_| "无法定位独立启动程序")?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // Preserve Windows' original argument quoting exactly, including non-Unicode arguments.
    let mut command = Vec::new();
    unsafe {
        let raw = GetCommandLineW();
        if raw.is_null() {
            return Err("无法读取启动参数".into());
        }
        while *raw.add(command.len()) != 0 {
            if command.len() >= 32000 {
                return Err("启动参数过长".into());
            }
            command.push(*raw.add(command.len()));
        }
    }
    command.extend(format!(" {MARKER}").encode_utf16());
    command.push(0);

    let mut size = 0;
    unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
    if size == 0 {
        return Err("独立启动策略不可用".into());
    }
    let memory = unsafe { HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, size) };
    if memory.is_null() {
        return Err("独立启动内存分配失败".into());
    }
    struct Attributes {
        memory: *mut std::ffi::c_void,
        initialized: bool,
    }
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe {
                if self.initialized {
                    DeleteProcThreadAttributeList(self.memory);
                }
                HeapFree(GetProcessHeap(), 0, self.memory);
            }
        }
    }
    let mut attrs = Attributes {
        memory,
        initialized: false,
    };
    let failure = || {
        format!(
            "独立启动失败（{}）",
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
        )
    };
    if unsafe { InitializeProcThreadAttributeList(memory, 1, 0, &mut size) } == 0 {
        return Err(failure());
    }
    attrs.initialized = true;
    // PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE. This changes package
    // identity inheritance, NOT access tokens, privileges or job/security restrictions.
    let policy: u32 = 1;
    if unsafe {
        UpdateProcThreadAttribute(
            memory,
            0,
            PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY as usize,
            (&policy as *const u32).cast(),
            std::mem::size_of::<u32>(),
            null_mut(),
            null(),
        )
    } == 0
    {
        return Err(failure());
    }
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = memory;
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe {
        CreateProcessW(
            exe.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
            null(),
            null(),
            &startup.StartupInfo,
            &mut info,
        )
    } == 0
    {
        return Err(failure());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
    drop(thread);
    drop(attrs);
    if unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
        return Err(failure());
    }
    let mut exit = 0;
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut exit) } == 0 {
        return Err(failure());
    }
    Ok(Some(exit as i32))
}
