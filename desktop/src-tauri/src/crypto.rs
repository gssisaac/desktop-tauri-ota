//! Ed25519 signature verification for full-stack OTA bundles.
//!
//! Replace `OTA_PUBLIC_KEY_HEX` with your production key (see docs/OTA_DEPLOYMENT.md).

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

/// 32-byte Ed25519 public key (hex). macOS and Windows bundles are signed with separate keys
/// (`.ota/credentials.env`, `.ota/credentials-windows.env`); scripts/ota-credentials.mjs keeps
/// each constant in sync with its key.
#[cfg(not(windows))]
pub const OTA_PUBLIC_KEY_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";
#[cfg(windows)]
pub const OTA_PUBLIC_KEY_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn verify_bundle(bytes: &[u8], expected_sha256: &str, signature_hex: &str) -> Result<(), String> {
    let computed = sha256_hex(bytes);
    if computed != expected_sha256.trim().to_lowercase() {
        return Err(format!(
            "SHA-256 mismatch (expected {expected_sha256}, got {computed})"
        ));
    }

    let pk_bytes =
        hex::decode(OTA_PUBLIC_KEY_HEX).map_err(|e| format!("Invalid embedded public key: {e}"))?;
    let pk_array: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| "Embedded public key must be 32 bytes".to_string())?;
    let verifying_key = VerifyingKey::from_bytes(&pk_array)
        .map_err(|e| format!("Invalid Ed25519 public key: {e}"))?;

    let sig_bytes =
        hex::decode(signature_hex.trim()).map_err(|e| format!("Invalid signature hex: {e}"))?;
    let sig_array: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| "Signature must be 64 bytes".to_string())?;
    let signature = Signature::from_bytes(&sig_array);

    verifying_key
        .verify(bytes, &signature)
        .map_err(|e| format!("Ed25519 verification failed: {e}"))?;

    Ok(())
}
