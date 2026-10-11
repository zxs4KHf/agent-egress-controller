//! Upstream credentials in Windows Credential Manager (generic credentials of the current user).
//!
//! A profile only names a credential (`credentialRef`); the user name and password live here and are
//! read when the relay starts. Nothing in this module logs or formats a password.

use crate::core::config::CredentialSource;

/// Prefix of every target name this application writes, so its entries are recognisable in the
/// Credential Manager control panel and never collide with other programs.
pub const TARGET_PREFIX: &str = "AgentEgressController:";

pub fn target_name(reference: &str) -> String {
    format!("{TARGET_PREFIX}{reference}")
}

/// Windows Credential Manager, current user.
pub struct WindowsCredentials;

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND, FILETIME};
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide_ptr(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        // SAFETY: the API returns a NUL-terminated string.
        unsafe {
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
        }
    }

    pub fn write(target: &str, user: &str, pass: &str) -> Result<(), String> {
        let mut target_w = wide(target);
        let mut user_w = wide(user);
        let mut comment_w = wide("Agent Egress Controller upstream credential");
        // Stored as UTF-16 without terminator, the form the control panel displays.
        let mut blob: Vec<u8> = pass.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target_w.as_mut_ptr(),
            Comment: comment_w.as_mut_ptr(),
            LastWritten: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: user_w.as_mut_ptr(),
        };
        // SAFETY: every pointer refers to a buffer that outlives the call.
        let ok = unsafe { CredWriteW(&cred, 0) };
        blob.iter_mut().for_each(|b| *b = 0);
        if ok == 0 {
            return Err(format!("CredWriteW failed: error {}", unsafe {
                GetLastError()
            }));
        }
        Ok(())
    }

    pub fn read(target: &str) -> Result<Option<(String, String)>, String> {
        let target_w = wide(target);
        let mut p: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: on success `p` points at a block owned by the system until CredFree.
        let ok = unsafe { CredReadW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return if err == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(format!("CredReadW failed: error {err}"))
            };
        }
        // SAFETY: `p` is valid until CredFree below.
        let (user, pass) = unsafe {
            let c = &*p;
            let user = from_wide_ptr(c.UserName);
            let bytes: &[u8] = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
            };
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect();
            (user, String::from_utf16_lossy(&units))
        };
        unsafe { CredFree(p as *const _) };
        Ok(Some((user, pass)))
    }

    pub fn delete(target: &str) -> Result<bool, String> {
        let target_w = wide(target);
        // SAFETY: plain call with a NUL-terminated name.
        if unsafe { CredDeleteW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
            return Ok(true);
        }
        match unsafe { GetLastError() } {
            ERROR_NOT_FOUND => Ok(false),
            e => Err(format!("CredDeleteW failed: error {e}")),
        }
    }
}

#[cfg(windows)]
impl WindowsCredentials {
    pub fn store(&self, reference: &str, user: &str, pass: &str) -> Result<(), String> {
        imp::write(&target_name(reference), user, pass)
    }

    pub fn exists(&self, reference: &str) -> Result<bool, String> {
        imp::read(&target_name(reference)).map(|c| c.is_some())
    }

    /// The stored user name only, for showing in settings. The password is never returned to UI code.
    pub fn user_name(&self, reference: &str) -> Result<Option<String>, String> {
        imp::read(&target_name(reference)).map(|c| c.map(|(u, _)| u))
    }

    pub fn delete(&self, reference: &str) -> Result<bool, String> {
        imp::delete(&target_name(reference))
    }
}

#[cfg(windows)]
impl CredentialSource for WindowsCredentials {
    fn get(&self, reference: &str) -> Option<(String, String)> {
        imp::read(&target_name(reference)).ok().flatten()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn round_trip_in_credential_manager() {
        let c = WindowsCredentials;
        let r = format!("test-{}", std::process::id());
        assert_eq!(c.get(&r), None);
        c.store(&r, "user@x", "p\u{e4}ss:wörd").unwrap();
        assert_eq!(c.get(&r), Some(("user@x".into(), "p\u{e4}ss:wörd".into())));
        assert_eq!(c.user_name(&r).unwrap().as_deref(), Some("user@x"));
        c.store(&r, "u2", "").unwrap();
        assert_eq!(c.get(&r), Some(("u2".into(), String::new())));
        assert!(c.delete(&r).unwrap());
        assert!(!c.delete(&r).unwrap());
        assert!(!c.exists(&r).unwrap());
    }
}
