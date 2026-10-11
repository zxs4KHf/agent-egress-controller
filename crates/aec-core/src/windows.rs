//! Windows implementation of the read-only probes. Apart from [`set_timezone`], which the controller
//! calls only behind an open takeover gate, nothing in this module writes the registry, creates a
//! mutex or changes any setting.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GlobalFree, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND,
    ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS, NO_ERROR,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinInet::{
    InternetQueryOptionW, INTERNET_OPTION_PER_CONNECTION_OPTION, INTERNET_PER_CONN_AUTOCONFIG_URL,
    INTERNET_PER_CONN_FLAGS, INTERNET_PER_CONN_FLAGS_UI, INTERNET_PER_CONN_OPTIONW,
    INTERNET_PER_CONN_OPTION_LISTW, INTERNET_PER_CONN_PROXY_BYPASS, INTERNET_PER_CONN_PROXY_SERVER,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumValueW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    REG_EXPAND_SZ, REG_SZ, RRF_NOEXPAND, RRF_RT_REG_DWORD, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::System::Threading::{
    OpenMutexW, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    SYNCHRONIZATION_SYNCHRONIZE,
};
use windows_sys::Win32::System::Time::{
    GetDynamicTimeZoneInformation, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID,
};

use crate::model::{
    ListenerOwner, ProxySettings, RegistryProxy, StartupItem, StartupLocation, TimezoneState,
};
use crate::probe::{ProbeError, ProbeResult, SystemProxyReader, TimezoneReader, WriterEvidence};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Script files whose text is read when scanning the Startup folder.
const SCRIPT_EXTENSIONS: [&str; 6] = ["vbs", "cmd", "bat", "ps1", "js", "wsf"];
const MAX_SCRIPT_BYTES: u64 = 64 * 1024;

/// The real machine.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsHost;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide_buf(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Copy a WinINet-allocated string and release it.
unsafe fn take_wininet_string(p: *mut u16) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
    GlobalFree(p as *mut c_void);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn last_error(what: &str) -> ProbeError {
    ProbeError::new(what, Some(unsafe { GetLastError() }))
}

impl WindowsHost {
    unsafe fn query_per_connection(flags_option: u32) -> ProbeResult<ProxySettings> {
        let mut options: [INTERNET_PER_CONN_OPTIONW; 4] = zeroed();
        options[0].dwOption = flags_option;
        options[1].dwOption = INTERNET_PER_CONN_PROXY_SERVER;
        options[2].dwOption = INTERNET_PER_CONN_PROXY_BYPASS;
        options[3].dwOption = INTERNET_PER_CONN_AUTOCONFIG_URL;
        let mut list = INTERNET_PER_CONN_OPTION_LISTW {
            dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
            pszConnection: null_mut(),
            dwOptionCount: options.len() as u32,
            dwOptionError: 0,
            pOptions: options.as_mut_ptr(),
        };
        let mut size = list.dwSize;
        if InternetQueryOptionW(
            null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            &mut list as *mut _ as *mut c_void,
            &mut size,
        ) == 0
        {
            return Err(last_error("InternetQueryOptionW(PER_CONNECTION_OPTION)"));
        }
        Ok(ProxySettings {
            flags: options[0].Value.dwValue,
            proxy_server: take_wininet_string(options[1].Value.pszValue),
            bypass: take_wininet_string(options[2].Value.pszValue),
            auto_config_url: take_wininet_string(options[3].Value.pszValue),
        })
    }

    fn reg_string(subkey: &str, value: &str) -> ProbeResult<Option<String>> {
        let (k, v) = (wide(subkey), wide(value));
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut bytes = 0u32;
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                k.as_ptr(),
                v.as_ptr(),
                flags,
                null_mut(),
                null_mut(),
                &mut bytes,
            )
        };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if rc != NO_ERROR {
            return Err(ProbeError::new(
                format!("RegGetValueW({value}) size"),
                Some(rc),
            ));
        }
        let mut buf = vec![0u16; (bytes as usize / 2) + 1];
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                k.as_ptr(),
                v.as_ptr(),
                flags,
                null_mut(),
                buf.as_mut_ptr() as *mut c_void,
                &mut bytes,
            )
        };
        if rc != NO_ERROR {
            return Err(ProbeError::new(format!("RegGetValueW({value})"), Some(rc)));
        }
        Ok(Some(from_wide_buf(&buf)))
    }

    fn reg_dword(subkey: &str, value: &str) -> ProbeResult<Option<u32>> {
        let (k, v) = (wide(subkey), wide(value));
        let mut data = 0u32;
        let mut bytes = size_of::<u32>() as u32;
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                k.as_ptr(),
                v.as_ptr(),
                RRF_RT_REG_DWORD,
                null_mut(),
                &mut data as *mut u32 as *mut c_void,
                &mut bytes,
            )
        };
        match rc {
            NO_ERROR => Ok(Some(data)),
            ERROR_FILE_NOT_FOUND => Ok(None),
            _ => Err(ProbeError::new(format!("RegGetValueW({value})"), Some(rc))),
        }
    }

    fn run_values() -> ProbeResult<Vec<StartupItem>> {
        let key_name = wide(RUN_KEY);
        let mut key: HKEY = null_mut();
        let rc =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_name.as_ptr(), 0, KEY_READ, &mut key) };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(Vec::new());
        }
        if rc != NO_ERROR {
            return Err(ProbeError::new("RegOpenKeyExW(Run)", Some(rc)));
        }
        let mut items = Vec::new();
        let mut index = 0u32;
        let result = loop {
            let mut name = vec![0u16; 16384];
            let mut name_len = name.len() as u32;
            let mut data = vec![0u8; 64 * 1024];
            let mut data_len = data.len() as u32;
            let mut kind = 0u32;
            let rc = unsafe {
                RegEnumValueW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    null(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut data_len,
                )
            };
            if rc == ERROR_NO_MORE_ITEMS {
                break Ok(());
            }
            if rc != NO_ERROR {
                break Err(ProbeError::new("RegEnumValueW(Run)", Some(rc)));
            }
            let content = if kind == REG_SZ || kind == REG_EXPAND_SZ {
                let words: Vec<u16> = data[..data_len as usize]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                Some(from_wide_buf(&words))
            } else {
                None
            };
            items.push(StartupItem {
                location: StartupLocation::HkcuRun,
                name: from_wide_buf(&name[..name_len as usize]),
                content,
            });
            index += 1;
        };
        unsafe { RegCloseKey(key) };
        result.map(|_| items)
    }

    fn startup_folder_items() -> ProbeResult<Vec<StartupItem>> {
        let appdata = std::env::var_os("APPDATA")
            .ok_or_else(|| ProbeError::new("APPDATA is not set", None))?;
        let dir = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Startup");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(ProbeError::new(
                    format!("read {}: {e}", dir.display()),
                    None,
                ))
            }
        };
        let mut items = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let content = if SCRIPT_EXTENSIONS.contains(&ext.as_str())
                && entry
                    .metadata()
                    .map(|m| m.len() <= MAX_SCRIPT_BYTES)
                    .unwrap_or(false)
            {
                std::fs::read(&path)
                    .ok()
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
            } else {
                None
            };
            items.push(StartupItem {
                location: StartupLocation::StartupFolder,
                name,
                content,
            });
        }
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(items)
    }

    fn process_exe(pid: u32) -> Option<String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut buf = vec![0u16; 32768];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 {
                None
            } else {
                Some(String::from_utf16_lossy(&buf[..len as usize]))
            }
        }
    }
}

impl SystemProxyReader for WindowsHost {
    fn read(&self) -> ProbeResult<ProxySettings> {
        // FLAGS_UI reflects what the Settings UI shows; older systems only know FLAGS.
        unsafe {
            Self::query_per_connection(INTERNET_PER_CONN_FLAGS_UI)
                .or_else(|_| Self::query_per_connection(INTERNET_PER_CONN_FLAGS))
        }
    }

    fn read_registry(&self) -> ProbeResult<RegistryProxy> {
        Ok(RegistryProxy {
            proxy_enable: Self::reg_dword(INTERNET_SETTINGS, "ProxyEnable")?,
            proxy_server: Self::reg_string(INTERNET_SETTINGS, "ProxyServer")?,
            proxy_override: Self::reg_string(INTERNET_SETTINGS, "ProxyOverride")?,
            auto_config_url: Self::reg_string(INTERNET_SETTINGS, "AutoConfigURL")?,
        })
    }
}

impl TimezoneReader for WindowsHost {
    fn read(&self) -> ProbeResult<TimezoneState> {
        let mut info: DYNAMIC_TIME_ZONE_INFORMATION = unsafe { zeroed() };
        if unsafe { GetDynamicTimeZoneInformation(&mut info) } == TIME_ZONE_ID_INVALID {
            return Err(last_error("GetDynamicTimeZoneInformation"));
        }
        Ok(TimezoneState {
            windows_id: from_wide_buf(&info.TimeZoneKeyName),
            dynamic_daylight_disabled: info.DynamicDaylightTimeDisabled != 0,
        })
    }
}

impl WriterEvidence for WindowsHost {
    fn mutex_exists(&self, name: &str) -> ProbeResult<bool> {
        let n = wide(name);
        let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, n.as_ptr()) };
        if !h.is_null() {
            unsafe { CloseHandle(h) };
            return Ok(true);
        }
        match unsafe { GetLastError() } {
            ERROR_FILE_NOT_FOUND => Ok(false),
            // It exists; this token just may not open it.
            ERROR_ACCESS_DENIED => Ok(true),
            code => Err(ProbeError::new(format!("OpenMutexW({name})"), Some(code))),
        }
    }

    fn startup_items(&self) -> ProbeResult<Vec<StartupItem>> {
        let mut items = Self::startup_folder_items()?;
        items.extend(Self::run_values()?);
        Ok(items)
    }

    fn listener_owner(&self, port: u16) -> ProbeResult<Option<ListenerOwner>> {
        let mut size = 0u32;
        let mut buf: Vec<u8> = Vec::new();
        for _ in 0..4 {
            let rc = unsafe {
                GetExtendedTcpTable(
                    buf.as_mut_ptr() as *mut c_void,
                    &mut size,
                    0,
                    AF_INET as u32,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            if rc == NO_ERROR {
                break;
            }
            if rc != ERROR_INSUFFICIENT_BUFFER {
                return Err(ProbeError::new("GetExtendedTcpTable", Some(rc)));
            }
            buf = vec![0u8; size as usize + 1024];
        }
        if buf.is_empty() {
            return Err(ProbeError::new(
                "GetExtendedTcpTable returned no table",
                None,
            ));
        }
        let table = buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID;
        let count = unsafe { (*table).dwNumEntries } as usize;
        let rows = unsafe { std::slice::from_raw_parts((*table).table.as_ptr(), count) };
        for row in rows {
            // The port is stored in network byte order in the low 16 bits.
            let local_port = u16::from_be((row.dwLocalPort & 0xffff) as u16);
            if local_port == port {
                return Ok(Some(ListenerOwner {
                    port,
                    pid: row.dwOwningPid,
                    exe: Self::process_exe(row.dwOwningPid),
                }));
            }
        }
        Ok(None)
    }
}

/// DNS servers of the network adapters that are up (loopback and tunnel adapters included as they
/// are reported; nothing is filtered by guesswork). Read-only.
pub fn system_dns_servers() -> ProbeResult<Vec<String>> {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST,
        GAA_FLAG_SKIP_UNICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6};

    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_UNICAST;
    let mut size = 16 * 1024u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..4 {
        // u64 storage keeps the adapter structures aligned.
        buf = vec![0u64; (size as usize).div_ceil(8)];
        let rc = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                flags,
                null(),
                buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
                &mut size,
            )
        };
        if rc == NO_ERROR {
            break;
        }
        if rc != windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW {
            return Err(ProbeError::new("GetAdaptersAddresses", Some(rc)));
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !adapter.is_null() {
        let a = unsafe { &*adapter };
        if a.OperStatus == IfOperStatusUp {
            let mut dns = a.FirstDnsServerAddress;
            while !dns.is_null() {
                let d = unsafe { &*dns };
                let sa = d.Address.lpSockaddr;
                if !sa.is_null() {
                    let family = unsafe { (*sa).sa_family };
                    let text = if family == AF_INET {
                        let v4 = unsafe { &*(sa as *const SOCKADDR_IN) };
                        Some(
                            Ipv4Addr::from(u32::from_be(unsafe { v4.sin_addr.S_un.S_addr }))
                                .to_string(),
                        )
                    } else if family == AF_INET6 {
                        let v6 = unsafe { &*(sa as *const SOCKADDR_IN6) };
                        Some(Ipv6Addr::from(unsafe { v6.sin6_addr.u.Byte }).to_string())
                    } else {
                        None
                    };
                    if let Some(t) = text {
                        if !out.contains(&t) {
                            out.push(t);
                        }
                    }
                }
                dns = d.Next;
            }
        }
        adapter = a.Next;
    }
    Ok(out)
}

/// Whether this process's token holds a privilege (enabled or not), e.g. `SeTimeZonePrivilege`.
/// Holding it is what allows a process to change the setting; nothing is enabled here.
pub fn token_has_privilege(name: &str) -> ProbeResult<bool> {
    use windows_sys::Win32::Foundation::LUID;
    use windows_sys::Win32::Security::{
        GetTokenInformation, LookupPrivilegeValueW, TokenPrivileges, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let wanted = wide(name);
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    if unsafe { LookupPrivilegeValueW(null(), wanted.as_ptr(), &mut luid) } == 0 {
        return Err(last_error("LookupPrivilegeValueW"));
    }
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    let mut size = 0u32;
    unsafe { GetTokenInformation(token, TokenPrivileges, null_mut(), 0, &mut size) };
    let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenPrivileges,
            buf.as_mut_ptr() as *mut c_void,
            size,
            &mut size,
        )
    };
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return Err(last_error("GetTokenInformation"));
    }
    let tp = buf.as_ptr() as *const TOKEN_PRIVILEGES;
    let count = unsafe { (*tp).PrivilegeCount } as usize;
    let entries = unsafe { std::slice::from_raw_parts((*tp).Privileges.as_ptr(), count) };
    Ok(entries
        .iter()
        .any(|e| e.Luid.LowPart == luid.LowPart && e.Luid.HighPart == luid.HighPart))
}

/// `App Paths` registration of an executable (HKLM, then HKCU), e.g. `msedge.exe`.
pub fn app_path(exe_name: &str) -> Option<PathBuf> {
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    let sub = wide(&format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe_name}"
    ));
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut bytes = 0u32;
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
        if unsafe {
            RegGetValueW(
                root,
                sub.as_ptr(),
                null(),
                flags,
                null_mut(),
                null_mut(),
                &mut bytes,
            )
        } != NO_ERROR
        {
            continue;
        }
        let mut buf = vec![0u16; bytes as usize / 2 + 1];
        if unsafe {
            RegGetValueW(
                root,
                sub.as_ptr(),
                null(),
                flags,
                null_mut(),
                buf.as_mut_ptr() as *mut c_void,
                &mut bytes,
            )
        } == NO_ERROR
        {
            let p = PathBuf::from(from_wide_buf(&buf).trim_matches('"'));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// A Chromium instance holds `lockfile` in its user-data-dir open without sharing while it runs.
pub fn chromium_profile_in_use(user_data_dir: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    let lock = user_data_dir.join("lockfile");
    if !lock.exists() {
        return false;
    }
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&lock)
        .is_err()
}

/// Post WM_CLOSE to every visible top-level window of `pid`. Returns how many were asked.
pub fn close_windows_of(pid: u32) -> usize {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
    };
    struct Ctx {
        pid: u32,
        windows: Vec<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam as *mut Ctx);
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, &mut owner);
        if owner == ctx.pid && IsWindowVisible(hwnd) != 0 {
            ctx.windows.push(hwnd);
        }
        1
    }
    let mut ctx = Ctx {
        pid,
        windows: Vec::new(),
    };
    unsafe { EnumWindows(Some(each), &mut ctx as *mut Ctx as LPARAM) };
    for &w in &ctx.windows {
        unsafe { PostMessageW(w, WM_CLOSE, 0, 0) };
    }
    ctx.windows.len()
}

/// PATH as a newly started process would get it: machine then user value from the registry, with
/// environment references expanded. `None` when neither can be read.
pub fn fresh_path() -> Option<String> {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    fn read(root: HKEY, sub: &str) -> Option<String> {
        let (k, v) = (wide(sub), wide("Path"));
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut bytes = 0u32;
        if unsafe {
            RegGetValueW(
                root,
                k.as_ptr(),
                v.as_ptr(),
                flags,
                null_mut(),
                null_mut(),
                &mut bytes,
            )
        } != NO_ERROR
        {
            return None;
        }
        let mut buf = vec![0u16; bytes as usize / 2 + 1];
        if unsafe {
            RegGetValueW(
                root,
                k.as_ptr(),
                v.as_ptr(),
                flags,
                null_mut(),
                buf.as_mut_ptr() as *mut c_void,
                &mut bytes,
            )
        } != NO_ERROR
        {
            return None;
        }
        Some(from_wide_buf(&buf))
    }
    let machine = read(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    );
    let user = read(HKEY_CURRENT_USER, "Environment");
    let joined = [machine, user]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(";");
    if joined.is_empty() {
        return None;
    }
    let src = wide(&joined);
    let need = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), null_mut(), 0) };
    let mut out = vec![0u16; need as usize + 1];
    unsafe { ExpandEnvironmentStringsW(src.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
    Some(from_wide_buf(&out))
}

/// Change the system time zone to a Windows time zone id (e.g. `Eastern Standard Time`), the way
/// `tzutil /s` does, and tell running programs. Requires SeTimeZonePrivilege, which standard users
/// hold; it is enabled on this process's token for the call. Called only behind an open takeover gate.
pub fn set_timezone(windows_id: &str) -> ProbeResult<()> {
    use windows_sys::Win32::Foundation::LUID;
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::Win32::System::Time::{
        EnumDynamicTimeZoneInformation, SetDynamicTimeZoneInformation,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_TIMECHANGE,
    };

    // Find the zone by its key name.
    let mut found: Option<DYNAMIC_TIME_ZONE_INFORMATION> = None;
    let mut i = 0u32;
    loop {
        let mut dtzi: DYNAMIC_TIME_ZONE_INFORMATION = unsafe { zeroed() };
        let rc = unsafe { EnumDynamicTimeZoneInformation(i, &mut dtzi) };
        if rc == ERROR_NO_MORE_ITEMS {
            break;
        }
        if rc != NO_ERROR {
            return Err(ProbeError::new("EnumDynamicTimeZoneInformation", Some(rc)));
        }
        if from_wide_buf(&dtzi.TimeZoneKeyName).eq_ignore_ascii_case(windows_id) {
            found = Some(dtzi);
            break;
        }
        i += 1;
    }
    let dtzi = found.ok_or_else(|| {
        ProbeError::new(format!("unknown Windows time zone id '{windows_id}'"), None)
    })?;

    let name = wide("SeTimeZonePrivilege");
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    if unsafe { LookupPrivilegeValueW(null(), name.as_ptr(), &mut luid) } == 0 {
        return Err(last_error("LookupPrivilegeValueW"));
    }
    let mut token = null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    } == 0
    {
        return Err(last_error("OpenProcessToken"));
    }
    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    let adjusted = unsafe { AdjustTokenPrivileges(token, 0, &tp, 0, null_mut(), null_mut()) };
    // AdjustTokenPrivileges succeeds with ERROR_NOT_ALL_ASSIGNED when the token lacks the privilege.
    let adjust_err = unsafe { GetLastError() };
    unsafe { CloseHandle(token) };
    if adjusted == 0 || adjust_err != NO_ERROR {
        return Err(ProbeError::new(
            "AdjustTokenPrivileges(SeTimeZonePrivilege)",
            Some(adjust_err),
        ));
    }

    if unsafe { SetDynamicTimeZoneInformation(&dtzi) } == 0 {
        return Err(last_error("SetDynamicTimeZoneInformation"));
    }
    // Browsers refresh their time zone on WM_TIMECHANGE; a hung window cannot block us.
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_TIMECHANGE,
            0,
            0,
            SMTO_ABORTIFHUNG,
            2000,
            &mut result,
        )
    };
    Ok(())
}
