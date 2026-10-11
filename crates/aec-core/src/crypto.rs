//! SHA-256 and random bytes from Windows CNG, so the crate needs no extra crypto dependency.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptGenRandom, BCryptHashData, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE,
    BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALGORITHM, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};

#[derive(Debug)]
pub struct CryptoError(pub String);

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CryptoError {}

fn check(status: i32, what: &str) -> Result<(), CryptoError> {
    if status >= 0 {
        Ok(())
    } else {
        Err(CryptoError(format!(
            "{what} failed: NTSTATUS 0x{status:08x}"
        )))
    }
}

/// Lower-case hex SHA-256 of a file, read in chunks.
pub fn sha256_file(path: &Path) -> Result<String, CryptoError> {
    let mut file = File::open(path).map_err(|e| CryptoError(format!("{}: {e}", path.display())))?;
    let mut alg: BCRYPT_ALG_HANDLE = null_mut();
    let mut hash: BCRYPT_HASH_HANDLE = null_mut();
    let mut digest = [0u8; 32];
    let result = (|| unsafe {
        check(
            BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_SHA256_ALGORITHM, null(), 0),
            "BCryptOpenAlgorithmProvider",
        )?;
        check(
            BCryptCreateHash(alg, &mut hash, null_mut(), 0, null(), 0, 0),
            "BCryptCreateHash",
        )?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| CryptoError(format!("{}: {e}", path.display())))?;
            if n == 0 {
                break;
            }
            check(
                BCryptHashData(hash, buf.as_ptr(), n as u32, 0),
                "BCryptHashData",
            )?;
        }
        check(
            BCryptFinishHash(hash, digest.as_mut_ptr(), digest.len() as u32, 0),
            "BCryptFinishHash",
        )
    })();
    unsafe {
        if !hash.is_null() {
            BCryptDestroyHash(hash);
        }
        if !alg.is_null() {
            BCryptCloseAlgorithmProvider(alg, 0);
        }
    }
    result.map(|_| hex(&digest))
}

/// `n` random bytes from the system RNG, as lower-case hex.
pub fn random_hex(n: usize) -> Result<String, CryptoError> {
    let mut buf = vec![0u8; n];
    check(
        unsafe {
            BCryptGenRandom(
                null_mut(),
                buf.as_mut_ptr(),
                n as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        },
        "BCryptGenRandom",
    )?;
    Ok(hex(&buf))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        let p = std::env::temp_dir().join(format!("aec-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn random_hex_has_length_and_varies() {
        let a = random_hex(16).unwrap();
        let b = random_hex(16).unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
