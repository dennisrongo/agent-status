//! At-rest encryption for API keys. AES-256-GCM with an Argon2id-derived key.
//! The KDF password is the machine UID, so ciphertext is bound to this machine
//! — a `settings.json` copied to another machine cannot be decrypted.

use aes_gcm::aead::{rand_core::RngCore, Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::Argon2;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    #[error("machine id unavailable: {0}")]
    MachineId(String),
    #[error("key derivation failed")]
    Kdf,
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed (key may be bound to another machine)")]
    Decrypt,
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),
}

/// A secret stored at rest. None of these fields are sensitive on their own.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptedSecret {
    pub ciphertext: String,
    pub nonce: String,
    pub salt: String,
}

fn machine_password() -> Result<String, EncryptionError> {
    machine_uid::get().map_err(|e| EncryptionError::MachineId(e.to_string()))
}

fn derive_key(password: &[u8], salt: &[u8]) -> Result<[u8; 32], EncryptionError> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password, salt, &mut key)
        .map_err(|_| EncryptionError::Kdf)?;
    Ok(key)
}

fn encrypt_with(password: &[u8], plaintext: &str) -> Result<EncryptedSecret, EncryptionError> {
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let key_bytes = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ct = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| EncryptionError::Encrypt)?;

    Ok(EncryptedSecret {
        ciphertext: STANDARD.encode(ct),
        nonce: STANDARD.encode(nonce_bytes),
        salt: STANDARD.encode(salt),
    })
}

fn decrypt_with(password: &[u8], secret: &EncryptedSecret) -> Result<String, EncryptionError> {
    let salt = STANDARD.decode(&secret.salt)?;
    let key_bytes = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));

    let nonce_bytes = STANDARD.decode(&secret.nonce)?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = STANDARD.decode(&secret.ciphertext)?;

    let pt = cipher
        .decrypt(nonce, ct.as_ref())
        .map_err(|_| EncryptionError::Decrypt)?;
    String::from_utf8(pt).map_err(|_| EncryptionError::Decrypt)
}

/// Encrypt a secret, binding it to this machine.
pub fn encrypt(plaintext: &str) -> Result<EncryptedSecret, EncryptionError> {
    let pw = machine_password()?;
    encrypt_with(pw.as_bytes(), plaintext)
}

/// Decrypt a machine-bound secret.
pub fn decrypt(secret: &EncryptedSecret) -> Result<String, EncryptionError> {
    let pw = machine_password()?;
    decrypt_with(pw.as_bytes(), secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_with_fixed_password() {
        let pw = b"fixed-test-machine-id";
        let secret = encrypt_with(pw, "sk-ant-admin-abc123").unwrap();
        // Stored form is not the plaintext.
        assert!(!secret.ciphertext.contains("sk-ant"));
        let back = decrypt_with(pw, &secret).unwrap();
        assert_eq!(back, "sk-ant-admin-abc123");
    }

    #[test]
    fn wrong_password_fails() {
        let secret = encrypt_with(b"machine-a", "topsecret").unwrap();
        let err = decrypt_with(b"machine-b", &secret);
        assert!(err.is_err());
    }

    #[test]
    fn nonce_and_salt_vary_per_encryption() {
        let a = encrypt_with(b"pw", "same").unwrap();
        let b = encrypt_with(b"pw", "same").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.salt, b.salt);
    }
}
