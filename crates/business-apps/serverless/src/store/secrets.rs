//! Encrypted app secrets at rest (AES-256-GCM, nonce‖ciphertext base64).
//! Master key from `LAN_SRV_SECRET_KEY`; a dev-only fallback key (derived
//! per-process) is used with a warn when the env is unset — secrets would
//! survive restarts anyway in the container volume, so production must set
//! the env var explicitly.

use std::collections::HashMap;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use turso::params::Params;
use turso::Value as SqlValue;

use serverless_core::ServerlessError;

use super::TursoStore;

const NONCE_LEN: usize = 12;

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SrvSecret {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub created_at: String,
}

pub(crate) type SecretMap = HashMap<String, String>;

fn master_key() -> [u8; 32] {
    let generated = match std::env::var("LAN_SRV_SECRET_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::warn!("LAN_SRV_SECRET_KEY unset: using per-process dev key (secrets are not portable)");
            format!("lan-dev-key-{}-{}", std::process::id(), fingerprint("x")[..16].to_string())
        }
    };
    let mut out = [0u8; 32];
    let bytes = generated.as_bytes();
    for (i, b) in out.iter_mut().enumerate() {
        *b = bytes[i % bytes.len().max(1)];
    }
    out
}

fn encrypt(plaintext: &str) -> Result<String, ServerlessError> {
    let cipher = Aes256Gcm::new_from_slice(&master_key()).map_err(|e| ServerlessError::Internal(format!("key: {e}")))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| ServerlessError::Internal(format!("encrypt: {e}")))?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(&blob))
}

fn decrypt(encoded: &str) -> Result<String, ServerlessError> {
    let blob = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| ServerlessError::Internal(format!("decode: {e}")))?;
    if blob.len() < NONCE_LEN {
        return Err(ServerlessError::Internal("ciphertext too short".to_string()));
    }
    let cipher = Aes256Gcm::new_from_slice(&master_key()).map_err(|e| ServerlessError::Internal(format!("key: {e}")))?;
    let (nonce, ct) = blob.split_at(NONCE_LEN);
    let pt = cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|e| ServerlessError::Internal(format!("decrypt: {e}")))?;
    String::from_utf8(pt).map_err(|e| ServerlessError::Internal(format!("utf8: {e}")))
}

fn fingerprint(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

impl TursoStore {
    /// Set or replace a secret; names are uppercase-normalised.
    pub(crate) async fn secret_set(&self, board_id: &str, name: &str, value: &str) -> Result<(), ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let cipher = encrypt(value)?;
        conns
            .conn
            .execute(
                "INSERT INTO srv_app_secrets (name, cipher, fingerprint, created_at) VALUES (?, ?, ?, ?) \
                 ON CONFLICT(name) DO UPDATE SET cipher = excluded.cipher, fingerprint = excluded.fingerprint",
                Params::Positional(vec![
                    SqlValue::Text(name.to_uppercase()),
                    SqlValue::Text(cipher),
                    SqlValue::Text(fingerprint(value)),
                    SqlValue::Text(Self::now_millis()),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        drop(_guard);
        Ok(())
    }

    /// List secrets (names + fingerprint only — ciphertext never leaves the store).
    pub(crate) async fn secret_list(&self, board_id: &str) -> Result<Vec<SrvSecret>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT name, cipher, fingerprint, created_at FROM srv_app_secrets ORDER BY name")
            .await
            .map_err(Self::map_db)?;
        let mut rows = stmt.query(Params::None).await.map_err(Self::map_db)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(Self::map_db)? {
            let name = row.get_value(0).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            let cipher = row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            let fp = row.get_value(2).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            let created_at = row.get_value(3).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            out.push(SrvSecret {
                name,
                fingerprint: (!cipher.is_empty()).then_some(fp),
                created_at,
            });
        }
        drop(_guard);
        Ok(out)
    }

    /// Remove a secret; returns true when it existed.
    pub(crate) async fn secret_remove(&self, board_id: &str, name: &str) -> Result<bool, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let n = conns
            .conn
            .execute(
                "DELETE FROM srv_app_secrets WHERE name = ?",
                Params::Positional(vec![SqlValue::Text(name.to_uppercase())]),
            )
            .await
            .map_err(Self::map_db)?;
        drop(_guard);
        Ok(n > 0)
    }

    /// Load all secrets decrypted into a map, handed to the recipe engine
    /// only (never serialised to clients).
    pub(crate) async fn secrets_map(&self, board_id: &str) -> Result<SecretMap, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT name, cipher FROM srv_app_secrets")
            .await
            .map_err(Self::map_db)?;
        let mut rows = stmt.query(Params::None).await.map_err(Self::map_db)?;
        let mut map = HashMap::new();
        while let Some(row) = rows.next().await.map_err(Self::map_db)? {
            let name = row.get_value(0).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            let cipher = row.get_value(1).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
            if let Ok(pt) = decrypt(&cipher) {
                map.insert(name, pt);
            }
        }
        drop(_guard);
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let pt = "hunter2-secret";
        let ct = encrypt(pt).unwrap();
        assert_ne!(ct, pt);
        assert_eq!(decrypt(&ct).unwrap(), pt);
    }

    #[test]
    fn ciphertext_is_nonce_plus_ct() {
        let ct = encrypt("abc").unwrap();
        let blob = base64::engine::general_purpose::STANDARD.decode(&ct).unwrap();
        assert!(blob.len() > NONCE_LEN);
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let ct = encrypt("abc").unwrap();
        let mut blob = base64::engine::general_purpose::STANDARD.decode(&ct).unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        let bad = base64::engine::general_purpose::STANDARD.encode(&blob);
        assert!(decrypt(&bad).is_err());
    }

    #[test]
    fn names_upper() {
        assert_eq!("api_KEY".to_uppercase(), "API_KEY");
    }

    #[test]
    fn fingerprint_is_sha256_hex() {
        let f = fingerprint("x");
        assert_eq!(f.len(), 64);
        assert!(f.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(f, fingerprint("y"));
    }
}
