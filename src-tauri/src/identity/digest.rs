//! One-way digests for Tier 2 record identity.
//!
//! Tier 2 exists because most sources publish no structural identifier. A
//! dashboard order is identified by the text `Order RS-1001`; an invoice by its
//! number. Storing that text in the ledger would make the ledger a store of
//! source content, which §3 refuses -- durable data is structural.
//!
//! So the ledger stores a digest instead. Two properties matter:
//!
//! * **One-way.** The ledger must not be reversible into the source content it
//!   describes, even by someone holding the database.
//! * **Salted.** An unsalted SHA-256 of `"Order RS-1001"` is recovered by
//!   guessing, and identifiers are extremely guessable -- sequential numbers,
//!   short alphabets, predictable prefixes. A bare digest would be privacy
//!   theatre. The salt is therefore **required**, not optional, and
//!   [`hmac_sha256`] refuses an empty one rather than silently producing a
//!   weak value.
//!
//! HMAC-SHA256 rather than `SHA256(salt || value)`: HMAC is the construction
//! designed for keyed digests, and CNG implements it directly, so choosing it
//! costs nothing and avoids length-extension questions entirely.
//!
//! ## Why Windows CNG rather than a crate
//!
//! `Win32_Security_Cryptography` is already an enabled `windows-sys` feature --
//! `db::crypto` uses DPAPI from it for the database key. DPAPI is an
//! encrypt/decrypt service and cannot produce a digest, so it could not be
//! reused directly, but the provider can: `BCryptHash` with the HMAC flag is a
//! one-shot HMAC-SHA256. That keeps this to zero new dependencies, matching how
//! key material is already handled rather than introducing a second crypto
//! stack alongside it.

use windows_sys::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE,
    BCRYPT_ALG_HANDLE_HMAC_FLAG, BCRYPT_SHA256_ALGORITHM,
};

/// SHA-256 output width.
pub const DIGEST_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DigestError {
    /// A salt is mandatory. See the module docs: identifiers are guessable, so
    /// an unsalted digest would not actually protect anything.
    #[error("a record-identity digest requires a non-empty salt")]
    MissingSalt,
    #[error("the platform hash provider failed (NTSTATUS {status:#010x})")]
    Provider { status: u32 },
}

/// HMAC-SHA256 of `value` under `salt`.
///
/// `salt` must be non-empty. Callers should use a per-playbook salt so that the
/// same identifier under two different playbooks does not produce the same
/// ledger key -- otherwise the ledger leaks that two workflows touched the same
/// record, which is exactly the cross-workflow correlation the per-playbook
/// scoping in `workflow_processed_rows` already refuses.
pub fn hmac_sha256(salt: &[u8], value: &[u8]) -> Result<[u8; DIGEST_LEN], DigestError> {
    if salt.is_empty() {
        return Err(DigestError::MissingSalt);
    }

    // SAFETY: every pointer handed to CNG below points at a live local or at a
    // slice that outlives the call. `alg` is closed on both the success and the
    // failure path before returning.
    unsafe {
        let mut alg: BCRYPT_ALG_HANDLE = std::ptr::null_mut();
        let status = BCryptOpenAlgorithmProvider(
            &mut alg,
            BCRYPT_SHA256_ALGORITHM,
            std::ptr::null(),
            BCRYPT_ALG_HANDLE_HMAC_FLAG,
        );
        if status != 0 {
            return Err(DigestError::Provider {
                status: status as u32,
            });
        }

        let mut out = [0u8; DIGEST_LEN];
        // An empty `value` is legitimate -- a record whose identity field is
        // blank is a real (bad) case, and it must digest to something stable
        // rather than error, so that "blank" is detectable downstream as a
        // repeated key rather than vanishing.
        let status = BCryptHash(
            alg,
            salt.as_ptr(),
            salt.len() as u32,
            value.as_ptr(),
            value.len() as u32,
            out.as_mut_ptr(),
            DIGEST_LEN as u32,
        );
        BCryptCloseAlgorithmProvider(alg, 0);

        if status != 0 {
            return Err(DigestError::Provider {
                status: status as u32,
            });
        }
        Ok(out)
    }
}

/// Lowercase hex, for storage in the ledger's `TEXT` key column.
pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_is_stable_for_the_same_input() {
        let a = hmac_sha256(b"salt-1", b"Order RS-1001").expect("digest");
        let b = hmac_sha256(b"salt-1", b"Order RS-1001").expect("digest");
        assert_eq!(a, b, "the same salt and value must digest identically");
        assert_eq!(a.len(), DIGEST_LEN);
    }

    #[test]
    fn different_values_digest_differently() {
        let a = hmac_sha256(b"salt-1", b"Order RS-1001").expect("digest");
        let b = hmac_sha256(b"salt-1", b"Order RS-1002").expect("digest");
        assert_ne!(a, b);
    }

    /// The property that makes per-playbook salting worth doing: the same
    /// record under two playbooks must not produce the same ledger key.
    #[test]
    fn the_salt_changes_the_digest() {
        let a = hmac_sha256(b"playbook-a", b"Order RS-1001").expect("digest");
        let b = hmac_sha256(b"playbook-b", b"Order RS-1001").expect("digest");
        assert_ne!(a, b);
    }

    #[test]
    fn an_empty_salt_is_refused_rather_than_weakly_accepted() {
        assert_eq!(hmac_sha256(b"", b"anything"), Err(DigestError::MissingSalt));
    }

    #[test]
    fn an_empty_value_still_digests() {
        // Not an error: a blank identity field is a real case, and it has to
        // stay visible downstream as a repeated key rather than disappear.
        let a = hmac_sha256(b"salt-1", b"").expect("digest");
        let b = hmac_sha256(b"salt-1", b"").expect("digest");
        assert_eq!(a, b);
    }

    #[test]
    fn hex_is_lowercase_and_double_width() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xa9, 0xff]), "000fa9ff");
        let d = hmac_sha256(b"s", b"v").expect("digest");
        assert_eq!(to_hex(&d).len(), DIGEST_LEN * 2);
    }

    /// The digest must not be the input in disguise. A weak "digest" that
    /// embedded the value would pass every test above.
    #[test]
    fn the_digest_does_not_contain_the_source_text() {
        let value = "Order RS-1001";
        let hex = to_hex(&hmac_sha256(b"salt-1", value.as_bytes()).expect("digest"));
        assert!(!hex.contains("1001"));
        assert!(!hex.to_lowercase().contains("order"));
    }
}
