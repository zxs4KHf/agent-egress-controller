//! Existing CVR control channel only. Never enable its controller or invoke config reload.
use super::RuntimeHost;
use crate::probe::WriterEvidence;
use serde_json::Value;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
};

pub fn client_root() -> Result<PathBuf, &'static str> {
    std::env::var_os("APPDATA")
        .map(|p| PathBuf::from(p).join("io.github.clash-verge-rev.clash-verge-rev"))
        .ok_or("COOP_CLIENT_NOT_FOUND")
}
fn supported_gui(exe: &Path) -> bool {
    let wide: Vec<u16> = exe
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let len = GetFileVersionInfoSizeW(wide.as_ptr(), std::ptr::null_mut());
        if len == 0 || len > 1024 * 1024 {
            return false;
        }
        let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
        if GetFileVersionInfoW(wide.as_ptr(), 0, len, buffer.as_mut_ptr().cast()) == 0 {
            return false;
        }
        let mut result = std::ptr::null_mut();
        let mut size = 0;
        if VerQueryValueW(
            buffer.as_ptr().cast(),
            [b'\\' as u16, 0].as_ptr(),
            &mut result,
            &mut size,
        ) == 0
            || size < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
            || result.is_null()
        {
            return false;
        }
        let info = std::ptr::read_unaligned(result.cast::<VS_FIXEDFILEINFO>());
        info.dwSignature == 0xfeef04bd
            && info.dwFileVersionMS == (2 << 16 | 5)
            && info.dwFileVersionLS >> 16 == 8
    }
}
pub struct ClientRuntime {
    port: u16,
    secret: String,
    pid: u32,
    identity: (String, u64),
    file: PathBuf,
    hash: String,
}
impl ClientRuntime {
    pub fn open(root: &Path) -> Result<Self, &'static str> {
        let file = root.join("clash-verge.yaml");
        let config = super::client::yaml(&file)?;
        let endpoint = config["external-controller"]
            .as_str()
            .ok_or("COOP_CONTROLLER_DISABLED")?;
        let address = endpoint
            .parse::<std::net::SocketAddr>()
            .map_err(|_| "COOP_CONTROLLER_NOT_IPV4_LOOPBACK")?;
        if address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            || address.port() == 0
        {
            return Err("COOP_CONTROLLER_NOT_IPV4_LOOPBACK");
        }
        let owner = crate::windows::WindowsHost
            .listener_owner(address.port())
            .map_err(|_| "COOP_CONTROLLER_OWNER_UNKNOWN")?
            .ok_or("COOP_CONTROLLER_OWNER_UNKNOWN")?;
        let identity = crate::core::process::process_identity(owner.pid)
            .ok_or("COOP_CONTROLLER_OWNER_UNKNOWN")?;
        let path = PathBuf::from(&identity.0);
        if !path.file_name().is_some_and(|n| {
            n.eq_ignore_ascii_case("verge-mihomo.exe")
                || n.eq_ignore_ascii_case("verge-mihomo-alpha.exe")
        }) {
            return Err("COOP_CONTROLLER_OWNER_UNKNOWN");
        }
        let gui = path
            .parent()
            .ok_or("COOP_CLIENT_VERSION_UNSUPPORTED")?
            .join("clash-verge.exe");
        if !supported_gui(&gui) {
            return Err("COOP_CLIENT_VERSION_UNSUPPORTED");
        }
        let secret = config["secret"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024 && !s.contains(['\r', '\n']))
            .ok_or("COOP_CONTROLLER_AUTH_UNAVAILABLE")?
            .to_owned();
        Ok(Self {
            port: address.port(),
            secret,
            pid: owner.pid,
            identity,
            file: file.clone(),
            hash: crate::crypto::sha256_file(&file).map_err(|_| "COOP_FILE_UNAVAILABLE")?,
        })
    }
}
impl RuntimeHost for ClientRuntime {
    fn get(&self, path: &str) -> Result<Value, &'static str> {
        if !matches!(path, "/configs" | "/rules" | "/proxies" | "/version")
            && !(path.starts_with("/dns/query?name=")
                && path.ends_with(".test.dnscheck.tools&type=TXT"))
        {
            return Err("COOP_READ_PATH_INVALID");
        }
        let owner = crate::windows::WindowsHost
            .listener_owner(self.port)
            .map_err(|_| "COOP_CONTROLLER_OWNER_UNKNOWN")?
            .ok_or("COOP_CONTROLLER_OWNER_UNKNOWN")?;
        if owner.pid != self.pid
            || crate::core::process::process_identity(self.pid).as_ref() != Some(&self.identity)
            || crate::crypto::sha256_file(&self.file).ok().as_ref() != Some(&self.hash)
        {
            return Err("COOP_RUNTIME_CHANGED");
        }
        let (status, body) =
            crate::core::process::http_get_local(self.port, path, Some(&self.secret))
                .map_err(|_| "COOP_CONTROLLER_READ_FAILED")?;
        if status != 200 {
            return Err("COOP_CONTROLLER_READ_FAILED");
        }
        serde_json::from_str(&body).map_err(|_| "COOP_CONTROLLER_REPLY_INVALID")
    }
}
