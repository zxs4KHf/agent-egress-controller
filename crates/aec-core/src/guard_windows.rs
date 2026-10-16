//! Windows Filtering Platform admission rules. Only explicitly selected executable paths and
//! the installing user's SID are affected. Persistent rules remain fail-closed if AEC crashes.
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE};
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn check(code: u32) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("GUARD_NATIVE_{code}"))
    }
}
fn same_guid(a: GUID, b: GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
pub fn valid_sid(sid: &str) -> bool {
    sid.starts_with("S-1-5-")
        && sid.len() < 128
        && sid[2..].chars().all(|c| c.is_ascii_digit() || c == '-')
}
pub fn current_sid() -> Result<String, String> {
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err("GUARD_IDENTITY_UNAVAILABLE".into());
        }
        let mut n = 0;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut n);
        let mut bytes = vec![0u8; n as usize];
        let read = GetTokenInformation(token, TokenUser, bytes.as_mut_ptr().cast(), n, &mut n);
        CloseHandle(token);
        if read == 0 {
            return Err("GUARD_IDENTITY_UNAVAILABLE".into());
        }
        let user = std::ptr::read_unaligned(bytes.as_ptr().cast::<TOKEN_USER>());
        let mut text = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err("GUARD_IDENTITY_UNAVAILABLE".into());
        }
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        Ok(result)
    }
}
pub struct SecurityDescriptor {
    pub pointer: PSECURITY_DESCRIPTOR,
    pub size: u32,
}
impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> Result<Self, String> {
        let mut pointer = null_mut();
        let mut size = 0;
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(sddl).as_ptr(),
                1,
                &mut pointer,
                &mut size,
            )
        } == 0
        {
            return Err(format!("GUARD_ACL_{}", unsafe { GetLastError() }));
        }
        Ok(Self { pointer, size })
    }
}
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.pointer);
        }
    }
}
struct Allocation(*mut std::ffi::c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            FwpmFreeMemory0(&mut self.0);
        }
    }
}

pub struct Engine {
    handle: HANDLE,
    provider: GUID,
    sublayer: GUID,
    dynamic: bool,
    owner: String,
}
impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            FwpmEngineClose0(self.handle);
        }
    }
}
impl Engine {
    pub fn open(owner: &str, dynamic: bool) -> Result<Self, String> {
        Self::open_scoped(owner, dynamic, "")
    }
    /// Dedicated namespace for native integration fixtures, never the production provider.
    pub fn open_test(owner: &str, scope: &str, persistent: bool) -> Result<Self, String> {
        if scope.len() != 32 || !scope.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("GUARD_TEST_SCOPE_INVALID".into());
        }
        Self::open_scoped(owner, !persistent, scope)
    }
    fn open_scoped(owner: &str, dynamic: bool, scope: &str) -> Result<Self, String> {
        if !valid_sid(owner) {
            return Err("GUARD_IDENTITY_INVALID".into());
        }
        let hash = owner
            .bytes()
            .chain(scope.bytes())
            .fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
            });
        let namespace = if !scope.is_empty() {
            0x83cf7ef0c31542eb8fd3000000000000u128
        } else {
            0x94a6e162d12546ffb842000000000000u128
        };
        let provider = GUID::from_u128(namespace ^ u128::from(hash));
        let sublayer = GUID::from_u128(namespace ^ u128::from(hash) ^ 0x10000000000000000);
        let mut session: FWPM_SESSION0 = unsafe { std::mem::zeroed() };
        session.flags = if dynamic {
            FWPM_SESSION_FLAG_DYNAMIC
        } else {
            0
        };
        session.txnWaitTimeoutInMSec = 3000;
        let mut handle = null_mut();
        check(unsafe { FwpmEngineOpen0(null(), 10, null(), &session, &mut handle) })?;
        let engine = Self {
            handle,
            provider,
            sublayer,
            dynamic,
            owner: owner.into(),
        };
        let mut name = wide("Agent Egress application protection");
        let mut p: FWPM_PROVIDER0 = unsafe { std::mem::zeroed() };
        p.providerKey = provider;
        p.displayData.name = name.as_mut_ptr();
        p.flags = if dynamic {
            0
        } else {
            FWPM_PROVIDER_FLAG_PERSISTENT
        };
        let code = unsafe { FwpmProviderAdd0(handle, &p, null_mut()) };
        if code != 0x80320009 {
            check(code)?;
        }
        let mut key = provider;
        let mut layer: FWPM_SUBLAYER0 = unsafe { std::mem::zeroed() };
        layer.subLayerKey = sublayer;
        layer.providerKey = &mut key;
        layer.weight = 0x7000;
        layer.displayData.name = name.as_mut_ptr();
        layer.flags = if dynamic {
            0
        } else {
            FWPM_SUBLAYER_FLAG_PERSISTENT
        };
        let code = unsafe { FwpmSubLayerAdd0(handle, &layer, null_mut()) };
        if code != 0x80320009 {
            check(code)?;
        }
        Ok(engine)
    }
    fn filter_keys(&self) -> Result<Vec<GUID>, String> {
        let mut keys = Vec::new();
        for layer in [
            FWPM_LAYER_ALE_AUTH_CONNECT_V4,
            FWPM_LAYER_ALE_AUTH_CONNECT_V6,
        ] {
            keys.extend(self.layer_filter_keys(layer)?);
        }
        Ok(keys)
    }
    fn layer_filter_keys(&self, layer: GUID) -> Result<Vec<GUID>, String> {
        let mut provider = self.provider;
        let mut template: FWPM_FILTER_ENUM_TEMPLATE0 = unsafe { std::mem::zeroed() };
        template.providerKey = &mut provider;
        template.layerKey = layer;
        template.enumType = FWP_FILTER_ENUM_FULLY_CONTAINED;
        template.actionMask = u32::MAX;
        let mut enumeration = null_mut();
        check(unsafe { FwpmFilterCreateEnumHandle0(self.handle, &template, &mut enumeration) })?;
        struct EnumHandle(HANDLE, HANDLE);
        impl Drop for EnumHandle {
            fn drop(&mut self) {
                unsafe {
                    FwpmFilterDestroyEnumHandle0(self.0, self.1);
                }
            }
        }
        let _enumeration = EnumHandle(self.handle, enumeration);
        let mut keys = Vec::new();
        loop {
            let mut entries = null_mut();
            let mut count = 0;
            check(unsafe {
                FwpmFilterEnum0(self.handle, enumeration, 64, &mut entries, &mut count)
            })?;
            let _memory = Allocation(entries.cast());
            if count == 0 {
                break;
            }
            for entry in unsafe { std::slice::from_raw_parts(entries, count as usize) } {
                let entry = unsafe { &**entry };
                if !same_guid(entry.subLayerKey, self.sublayer)
                    || entry.action.r#type != FWP_ACTION_BLOCK
                {
                    return Err("GUARD_FILTER_IDENTITY_MISMATCH".into());
                }
                keys.push(entry.filterKey);
                if keys.len() > 128 {
                    return Err("GUARD_FILTER_LIMIT".into());
                }
            }
        }
        Ok(keys)
    }
    pub fn count(&self) -> Result<usize, String> {
        self.filter_keys().map(|v| v.len())
    }
    pub fn destroy_empty(&self) -> Result<(), String> {
        if self.count()? != 0 {
            return Err("GUARD_STILL_ACTIVE".into());
        }
        check(unsafe { FwpmSubLayerDeleteByKey0(self.handle, &self.sublayer) })?;
        check(unsafe { FwpmProviderDeleteByKey0(self.handle, &self.provider) })
    }
    /// Atomic replacement never leaves an unfiltered gap. An empty list is explicit release.
    pub fn apply(&self, port: u16, apps: &[String]) -> Result<usize, String> {
        if port == 0
            || apps.len() > 16
            || apps.iter().any(|p| {
                p.contains('\0')
                    || p.len() > 32700
                    || !std::path::Path::new(p).is_absolute()
                    || !p.to_ascii_lowercase().ends_with(".exe")
            })
        {
            return Err("GUARD_POLICY_INVALID".into());
        }
        let keys = self.filter_keys()?;
        check(unsafe { FwpmTransactionBegin0(self.handle, 0) })?;
        struct Transaction(HANDLE, bool);
        impl Drop for Transaction {
            fn drop(&mut self) {
                if !self.1 {
                    unsafe {
                        FwpmTransactionAbort0(self.0);
                    }
                }
            }
        }
        let mut txn = Transaction(self.handle, false);
        for key in keys {
            check(unsafe { FwpmFilterDeleteByKey0(self.handle, &key) })?;
        }
        let sd = SecurityDescriptor::from_sddl(&format!("D:(A;;CC;;;{})", self.owner))?;
        let mut user = FWP_BYTE_BLOB {
            size: sd.size,
            data: sd.pointer.cast(),
        };
        for path in apps {
            let mut app = null_mut();
            check(unsafe { FwpmGetAppIdFromFileName0(wide(path).as_ptr(), &mut app) })?;
            let _app = Allocation(app.cast());
            let common = [
                FWPM_FILTER_CONDITION0 {
                    fieldKey: FWPM_CONDITION_ALE_APP_ID,
                    matchType: FWP_MATCH_EQUAL,
                    conditionValue: FWP_CONDITION_VALUE0 {
                        r#type: FWP_BYTE_BLOB_TYPE,
                        Anonymous: FWP_CONDITION_VALUE0_0 { byteBlob: app },
                    },
                },
                FWPM_FILTER_CONDITION0 {
                    fieldKey: FWPM_CONDITION_ALE_USER_ID,
                    matchType: FWP_MATCH_EQUAL,
                    conditionValue: FWP_CONDITION_VALUE0 {
                        r#type: FWP_SECURITY_DESCRIPTOR_TYPE,
                        Anonymous: FWP_CONDITION_VALUE0_0 { sd: &mut user },
                    },
                },
            ];
            let deny = [
                FWPM_FILTER_CONDITION0 {
                    fieldKey: FWPM_CONDITION_IP_REMOTE_ADDRESS,
                    matchType: FWP_MATCH_NOT_EQUAL,
                    conditionValue: FWP_CONDITION_VALUE0 {
                        r#type: FWP_UINT32,
                        Anonymous: FWP_CONDITION_VALUE0_0 { uint32: 0x7f000001 },
                    },
                },
                FWPM_FILTER_CONDITION0 {
                    fieldKey: FWPM_CONDITION_IP_REMOTE_PORT,
                    matchType: FWP_MATCH_NOT_EQUAL,
                    conditionValue: FWP_CONDITION_VALUE0 {
                        r#type: FWP_UINT16,
                        Anonymous: FWP_CONDITION_VALUE0_0 { uint16: port },
                    },
                },
                FWPM_FILTER_CONDITION0 {
                    fieldKey: FWPM_CONDITION_IP_PROTOCOL,
                    matchType: FWP_MATCH_NOT_EQUAL,
                    conditionValue: FWP_CONDITION_VALUE0 {
                        r#type: FWP_UINT8,
                        Anonymous: FWP_CONDITION_VALUE0_0 { uint8: 6 },
                    },
                },
            ];
            for restriction in deny {
                let mut conditions = common.to_vec();
                conditions.push(restriction);
                self.add_filter(FWPM_LAYER_ALE_AUTH_CONNECT_V4, &mut conditions)?;
            }
            self.add_filter(FWPM_LAYER_ALE_AUTH_CONNECT_V6, &mut common.to_vec())?;
        }
        check(unsafe { FwpmTransactionCommit0(self.handle) })?;
        txn.1 = true;
        self.count()
    }
    fn add_filter(
        &self,
        layer: GUID,
        conditions: &mut [FWPM_FILTER_CONDITION0],
    ) -> Result<(), String> {
        let mut name = wide("AEC: selected application must use its IPv4 gateway");
        let mut provider = self.provider;
        let mut filter: FWPM_FILTER0 = unsafe { std::mem::zeroed() };
        filter.filterKey = GUID::from_u128(
            u128::from_str_radix(
                &crate::crypto::random_hex(16).map_err(|_| "GUARD_RANDOM_FAILED")?,
                16,
            )
            .map_err(|_| "GUARD_RANDOM_FAILED")?,
        );
        filter.displayData.name = name.as_mut_ptr();
        filter.providerKey = &mut provider;
        filter.layerKey = layer;
        filter.subLayerKey = self.sublayer;
        filter.flags = if self.dynamic {
            0
        } else {
            FWPM_FILTER_FLAG_PERSISTENT
        };
        filter.action.r#type = FWP_ACTION_BLOCK;
        filter.numFilterConditions = conditions.len() as u32;
        filter.filterCondition = conditions.as_mut_ptr();
        check(unsafe { FwpmFilterAdd0(self.handle, &filter, null_mut(), null_mut()) })
    }
}
