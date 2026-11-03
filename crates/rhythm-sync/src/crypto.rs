use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng, Payload, rand_core::RngCore},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use pbkdf2::pbkdf2_hmac;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{Error, MAX_ENCRYPTED_BYTES, MAX_OBJECT_BYTES, Result};

const ITERATIONS: u32 = 600_000;
const VERIFIER: &[u8] = b"Rhythm sync collection key verification version 1";

/// Serialize only into OS-protected local credential storage. Never export this
/// structure, include it in a portable backup, or write it to a log.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct VaultKey {
    vault_id: String,
    key: [u8; 32],
}

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey([REDACTED])")
    }
}

impl VaultKey {
    pub fn vault_id(&self) -> &str {
        &self.vault_id
    }
}

/// Public bootstrap information. The passphrase authenticates this descriptor;
/// an enrolled installation must retain and compare its original descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultDescriptor {
    pub format: String,
    pub version: u32,
    pub vault_id: String,
    pub salt: String,
    pub verifier: String,
}

impl VaultDescriptor {
    pub fn create(passphrase: &str) -> Result<(Self, VaultKey)> {
        validate_passphrase(passphrase)?;
        let mut salt = [0u8; 32];
        OsRng.fill_bytes(&mut salt);
        let vault_id = uuid::Uuid::new_v4().to_string();
        let key = derive(passphrase, &salt, &vault_id);
        let verifier = encrypt_object(&key, "vault-verifier", VERIFIER)?;
        Ok((
            Self {
                format: "rhythm-sync-vault".into(),
                version: 1,
                vault_id,
                salt: STANDARD.encode(salt),
                verifier: String::from_utf8(verifier).map_err(|_| Error::Protocol)?,
            },
            key,
        ))
    }

    pub fn unlock(&self, passphrase: &str) -> Result<VaultKey> {
        validate_passphrase(passphrase)?;
        if self.format != "rhythm-sync-vault"
            || self.version != 1
            || uuid::Uuid::parse_str(&self.vault_id).is_err()
            || self.salt.len() > 64
            || self.verifier.len() > 4096
        {
            return Err(Error::Protocol);
        }
        let salt = STANDARD.decode(&self.salt).map_err(|_| Error::Protocol)?;
        if salt.len() != 32 {
            return Err(Error::Protocol);
        }
        let key = derive(passphrase, &salt, &self.vault_id);
        let clear = Zeroizing::new(
            decrypt_object(&key, "vault-verifier", self.verifier.as_bytes())
                .map_err(|_| Error::RecoveryKey)?,
        );
        if clear.as_slice() != VERIFIER {
            return Err(Error::RecoveryKey);
        }
        Ok(key)
    }
}

fn validate_passphrase(passphrase: &str) -> Result<()> {
    if passphrase.chars().count() < 12 || passphrase.len() > 1024 {
        return Err(Error::RecoveryKey);
    }
    Ok(())
}

fn derive(passphrase: &str, salt: &[u8], vault_id: &str) -> VaultKey {
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, ITERATIONS, &mut key);
    VaultKey {
        vault_id: vault_id.into(),
        key,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    version: u32,
    vault_id: String,
    object_id: String,
    nonce: String,
    ciphertext: String,
}

fn aad(vault_id: &str, object_id: &str) -> Result<Vec<u8>> {
    if uuid::Uuid::parse_str(vault_id).is_err()
        || object_id.is_empty()
        || object_id.len() > 256
        || !object_id.is_ascii()
        || object_id.bytes().any(|b| b.is_ascii_control())
    {
        return Err(Error::Protocol);
    }
    serde_json::to_vec(&["rhythm-sync-object", "1", vault_id, object_id])
        .map_err(|_| Error::Protocol)
}

/// Encrypt once and durably freeze these exact bytes for every retry of an upload.
pub fn encrypt_object(key: &VaultKey, object_id: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    if plaintext.len() > MAX_OBJECT_BYTES {
        return Err(Error::ObjectTooLarge);
    }
    let aad = aad(&key.vault_id, object_id)?;
    let cipher = Aes256Gcm::new_from_slice(&key.key).map_err(|_| Error::RecoveryKey)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| Error::Integrity)?;
    serde_json::to_vec(&Envelope {
        format: "rhythm-sync-object".into(),
        version: 1,
        vault_id: key.vault_id.clone(),
        object_id: object_id.into(),
        nonce: STANDARD.encode(nonce),
        ciphertext: STANDARD.encode(ciphertext),
    })
    .map_err(|_| Error::Protocol)
}

pub fn decrypt_object(key: &VaultKey, object_id: &str, envelope: &[u8]) -> Result<Vec<u8>> {
    if envelope.len() > MAX_ENCRYPTED_BYTES {
        return Err(Error::ObjectTooLarge);
    }
    let value: Envelope = serde_json::from_slice(envelope).map_err(|_| Error::Protocol)?;
    if value.format != "rhythm-sync-object"
        || value.version != 1
        || value.vault_id != key.vault_id
        || value.object_id != object_id
        || value.nonce.len() != 16
    {
        return Err(Error::Integrity);
    }
    let aad = aad(&key.vault_id, object_id)?;
    let nonce = STANDARD.decode(value.nonce).map_err(|_| Error::Integrity)?;
    let ciphertext = STANDARD
        .decode(value.ciphertext)
        .map_err(|_| Error::Integrity)?;
    if nonce.len() != 12 || ciphertext.len() < 16 || ciphertext.len() > MAX_OBJECT_BYTES + 16 {
        return Err(Error::Integrity);
    }
    let cipher = Aes256Gcm::new_from_slice(&key.key).map_err(|_| Error::RecoveryKey)?;
    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| Error::Integrity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_and_authenticated_objects_reject_wrong_keys_ids_and_changes() {
        let (descriptor, key) = VaultDescriptor::create("a long recovery phrase").unwrap();
        let reopened = descriptor.unlock("a long recovery phrase").unwrap();
        let bytes = encrypt_object(&key, "revision-1", "私人 words".as_bytes()).unwrap();
        assert_eq!(
            decrypt_object(&reopened, "revision-1", &bytes).unwrap(),
            "私人 words".as_bytes()
        );
        assert!(descriptor.unlock("another wrong phrase").is_err());
        assert!(decrypt_object(&key, "revision-2", &bytes).is_err());
        let mut parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        parsed["vault_id"] = uuid::Uuid::new_v4().to_string().into();
        assert!(decrypt_object(&key, "revision-1", &serde_json::to_vec(&parsed).unwrap()).is_err());
        parsed["vault_id"] = key.vault_id().into();
        parsed["ciphertext"] = STANDARD.encode([0u8; 32]).into();
        assert!(decrypt_object(&key, "revision-1", &serde_json::to_vec(&parsed).unwrap()).is_err());
        assert!(!String::from_utf8_lossy(&bytes).contains("私人 words"));
    }

    #[test]
    fn rejects_unsupported_descriptor_and_excessive_payload() {
        assert!(VaultDescriptor::create("short").is_err());
        let key = VaultKey {
            vault_id: uuid::Uuid::new_v4().to_string(),
            key: [7; 32],
        };
        assert!(encrypt_object(&key, "x", &vec![0; MAX_OBJECT_BYTES + 1]).is_err());
        assert!(decrypt_object(&key, "x", br#"{"version":99}"#).is_err());
        assert!(!format!("{key:?}").contains("7"));
        let mut envelope: serde_json::Value =
            serde_json::from_slice(&encrypt_object(&key, "x", b"text").unwrap()).unwrap();
        envelope["iterations"] = 1.into();
        assert!(decrypt_object(&key, "x", &serde_json::to_vec(&envelope).unwrap()).is_err());
        envelope.as_object_mut().unwrap().remove("iterations");
        envelope["version"] = 2.into();
        assert!(decrypt_object(&key, "x", &serde_json::to_vec(&envelope).unwrap()).is_err());
        assert!(serde_json::from_str::<VaultDescriptor>(r#"{"format":"rhythm-sync-vault","version":1,"vault_id":"x","salt":"x","verifier":"x","iterations":1}"#).is_err());
    }
}
