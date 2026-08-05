//! Database key material, protected by Windows DPAPI.
//!
//! DPAPI is an encrypt/decrypt service, not a KDF, so the flow is:
//!
//!   first run  -> generate 32 random bytes -> CryptProtectData -> write blob
//!   every run  -> read blob -> CryptUnprotectData -> key in memory
//!
//! The key therefore exists in plaintext only in process memory, and only for
//! as long as the `Zeroizing` wrapper is alive. What lands on disk is the DPAPI
//! blob, which is decryptable only by this Windows user on this machine. The
//! key is never written to the database file, to config, or to any log.

use std::fs;
use std::io;
use std::path::Path;

use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};
use zeroize::Zeroizing;

use super::DbError;

/// SQLCipher takes a 256-bit raw key.
pub const KEY_LEN: usize = 32;

/// Secondary entropy folded into every DPAPI call. Without it, any other
/// process running as this user could call CryptUnprotectData on our blob and
/// recover the key. It is a domain separator, not a secret -- it is compiled
/// into the binary and gains us nothing if the attacker already has our code.
const ENTROPY: &[u8] = b"paradigm.local-db.key.v1";

/// Borrowed view of `data` as a DPAPI blob. The caller must keep `data` alive
/// for as long as the returned struct is used.
fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

/// Copy a DPAPI output blob into owned memory and release the LocalAlloc'd
/// buffer DPAPI handed us.
///
/// # Safety
/// `out` must be a blob populated by a successful CryptProtectData /
/// CryptUnprotectData call and not yet freed.
unsafe fn take_and_free(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let owned = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
    LocalFree(out.pbData as HLOCAL);
    owned
}

/// Encrypt `plaintext` to a DPAPI blob bound to the current Windows user.
pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, DbError> {
    let input = blob(plaintext);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    // CRYPTPROTECT_UI_FORBIDDEN: fail rather than block on a UI prompt, since
    // this can run from a background thread during app startup.
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };

    if ok == 0 {
        return Err(DbError::Dpapi("CryptProtectData", io::Error::last_os_error()));
    }
    Ok(unsafe { take_and_free(out) })
}

/// Decrypt a DPAPI blob produced by [`protect`].
pub fn unprotect(ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, DbError> {
    let input = blob(ciphertext);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };

    if ok == 0 {
        return Err(DbError::Dpapi(
            "CryptUnprotectData",
            io::Error::last_os_error(),
        ));
    }
    Ok(Zeroizing::new(unsafe { take_and_free(out) }))
}

/// Load the database key, generating and persisting one on first run.
///
/// Never overwrites an existing key file: doing so would silently render an
/// existing encrypted database permanently unreadable. The create path uses
/// `create_new`, so if another process wins the race we fall back to reading
/// the key it wrote rather than clobbering it.
pub fn load_or_create_key(key_path: &Path) -> Result<Zeroizing<Vec<u8>>, DbError> {
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
    }

    loop {
        match fs::read(key_path) {
            Ok(blob) => {
                let key = unprotect(&blob)?;
                if key.len() != KEY_LEN {
                    return Err(DbError::KeyLength {
                        got: key.len(),
                        expected: KEY_LEN,
                    });
                }
                return Ok(key);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }

        let mut key = Zeroizing::new(vec![0u8; KEY_LEN]);
        getrandom::fill(&mut key).map_err(|e| DbError::Rand(e.to_string()))?;
        let protected = protect(&key)?;

        use std::io::Write;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(key_path)
        {
            Ok(mut f) => {
                f.write_all(&protected)?;
                f.sync_all()?;
                return Ok(key);
            }
            // Someone else created it between our read and our write; go back
            // and read theirs.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}
