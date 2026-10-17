//! Narrow Edge policy adapter. No Chrome policy, proxy, time zone, profile or cookie writes.
use crate::browser_environment::{Host, Policy, Value};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};

const EDGE: &str = r"SOFTWARE\Policies\Microsoft\Edge";
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
pub struct WindowsBrowserHost;
impl Host for WindowsBrowserHost {
    fn read(&self, policy: Policy, machine: bool) -> Result<Option<Value>, &'static str> {
        let key = wide(EDGE);
        let name = wide(policy.name());
        let mut kind = 0;
        let mut size = 0;
        let root = if machine {
            HKEY_LOCAL_MACHINE
        } else {
            HKEY_CURRENT_USER
        };
        let flags = RRF_RT_REG_DWORD | RRF_RT_REG_SZ;
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                &mut kind,
                null_mut(),
                &mut size,
            )
        };
        if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS || size > 16 * 1024 {
            return Err("BROWSER_POLICY_UNREADABLE");
        }
        // u16 allocation keeps REG_SZ aligned; DWORD bytes are decoded without pointer casts.
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        if unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                &mut kind,
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        } != ERROR_SUCCESS
        {
            return Err("BROWSER_POLICY_UNREADABLE");
        }
        match kind {
            REG_DWORD if size == 4 => {
                let bytes = [buf[0].to_le_bytes(), buf[1].to_le_bytes()].concat();
                Ok(Some(Value::Dword(u32::from_le_bytes(
                    bytes.try_into().unwrap(),
                ))))
            }
            REG_SZ if size % 2 == 0 => {
                buf.truncate(size as usize / 2);
                if buf.last() != Some(&0) {
                    return Err("BROWSER_POLICY_UNREADABLE");
                }
                buf.pop();
                String::from_utf16(&buf)
                    .map(|text| Some(Value::Text(text)))
                    .map_err(|_| "BROWSER_POLICY_UNREADABLE")
            }
            _ => Err("BROWSER_POLICY_UNREADABLE"),
        }
    }
    fn write(&self, policy: Policy, value: Option<&Value>) -> Result<(), &'static str> {
        let mut key = null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(EDGE).as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                null(),
                &mut key,
                null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err("BROWSER_POLICY_WRITE_FAILED");
        }
        let name = wide(policy.name());
        let status = match value {
            Some(Value::Dword(number)) => unsafe {
                RegSetValueExW(
                    key,
                    name.as_ptr(),
                    0,
                    REG_DWORD,
                    number.to_le_bytes().as_ptr(),
                    4,
                )
            },
            Some(Value::Text(text)) => {
                let text = wide(text);
                unsafe {
                    RegSetValueExW(
                        key,
                        name.as_ptr(),
                        0,
                        REG_SZ,
                        text.as_ptr().cast(),
                        (text.len() * 2) as u32,
                    )
                }
            }
            None => unsafe { RegDeleteValueW(key, name.as_ptr()) },
        };
        unsafe {
            RegCloseKey(key);
        }
        if status == ERROR_SUCCESS || (value.is_none() && status == ERROR_FILE_NOT_FOUND) {
            Ok(())
        } else {
            Err("BROWSER_POLICY_WRITE_FAILED")
        }
    }
}
