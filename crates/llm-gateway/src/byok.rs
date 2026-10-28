//! Bring-your-own-key storage: a hub's vendor keys as a JSON map encrypted with
//! XChaCha20-Poly1305 under a key derived from `GATEWAY_MASTER_KEY`.

use std::collections::BTreeMap;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::GatewayError;

const NONCE_LEN: usize = 24;

fn cipher(master: &str) -> XChaCha20Poly1305 {
    let key: [u8; 32] = Sha256::digest(master.as_bytes()).into();
    XChaCha20Poly1305::new(&Key::from(key))
}

/// `nonce || ciphertext`, a fresh random nonce per call.
pub fn encrypt(master: &str, plaintext: &[u8]) -> Result<Vec<u8>, GatewayError> {
    let mut nonce = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce);
    let ciphertext = cipher(master)
        .encrypt(&XNonce::from(nonce), plaintext)
        .map_err(|e| GatewayError::Crypto(format!("encryption failed: {e}")))?;
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

pub fn decrypt(master: &str, blob: &[u8]) -> Result<Vec<u8>, GatewayError> {
    let (nonce, ciphertext) = blob
        .split_at_checked(NONCE_LEN)
        .ok_or_else(|| GatewayError::Crypto("BYOK blob too short".to_string()))?;
    let nonce: [u8; NONCE_LEN] = nonce
        .try_into()
        .map_err(|_| GatewayError::Crypto("BYOK blob too short".to_string()))?;
    cipher(master)
        .decrypt(&XNonce::from(nonce), ciphertext)
        .map_err(|_| {
            GatewayError::Crypto("BYOK blob does not decrypt with this master key".to_string())
        })
}

/// The hub's keys by vendor name; an absent blob is an empty map.
pub fn keys_from_blob(
    master: &str,
    blob: Option<&[u8]>,
) -> Result<BTreeMap<String, String>, GatewayError> {
    let Some(blob) = blob else {
        return Ok(BTreeMap::new());
    };
    let plain = decrypt(master, blob)?;
    serde_json::from_slice(&plain).map_err(|e| GatewayError::Crypto(format!("BYOK payload: {e}")))
}

/// `None` when the map is empty, so a hub without keys stores NULL.
pub fn blob_from_keys(
    master: &str,
    keys: &BTreeMap<String, String>,
) -> Result<Option<Vec<u8>>, GatewayError> {
    if keys.is_empty() {
        return Ok(None);
    }
    let plain =
        serde_json::to_vec(keys).map_err(|e| GatewayError::Crypto(format!("BYOK payload: {e}")))?;
    encrypt(master, &plain).map(Some)
}
