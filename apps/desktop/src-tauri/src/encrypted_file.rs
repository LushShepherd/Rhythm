//! Save a validated encrypted note envelope to a native user-selected NEW file.
//! Cryptography stays in WebCrypto; this boundary never accepts plaintext notes,
//! passwords, a web-provided destination, or replacement of an existing file.

use serde::Deserialize;
use std::{fs::OpenOptions, io::Write, path::Path};

const MAX_ENVELOPE: usize = 12 * 1024 * 1024;
const MAX_CIPHERTEXT: usize = 8 * 1024 * 1024 + 16;
const INVALID: &str = "The encrypted notes file is invalid or unsupported.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    format: &'a str,
    version: u32,
    kdf: &'a str,
    iterations: u32,
    cipher: &'a str,
    salt: &'a str,
    iv: &'a str,
    ciphertext: &'a str,
}

fn sextet(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Check canonical base64 without allocating a second copy of the ciphertext.
fn decoded_length(value: &str) -> Result<usize, String> {
    if !value.len().is_multiple_of(4) {
        return Err(INVALID.into());
    }
    let padding = if value.ends_with("==") {
        2
    } else if value.ends_with('=') {
        1
    } else {
        0
    };
    let data = &value.as_bytes()[..value.len() - padding];
    if data.iter().any(|byte| sextet(*byte).is_none()) {
        return Err(INVALID.into());
    }
    if padding != 0 {
        let tail = data.last().and_then(|byte| sextet(*byte)).ok_or(INVALID)?;
        if (padding == 2 && tail & 15 != 0) || (padding == 1 && tail & 3 != 0) {
            return Err(INVALID.into());
        }
    }
    Ok(value.len() / 4 * 3 - padding)
}

pub fn validate(contents: &str) -> Result<(), String> {
    if contents.len() > MAX_ENVELOPE {
        return Err("The encrypted notes file exceeds the 12 MB limit.".into());
    }
    let envelope: Envelope<'_> = serde_json::from_str(contents).map_err(|_| INVALID.to_owned())?;
    if envelope.format != "rhythm-encrypted-notes"
        || ![1, 2].contains(&envelope.version)
        || envelope.kdf != "PBKDF2-SHA256"
        || envelope.iterations != 600_000
        || envelope.cipher != "AES-256-GCM"
        || decoded_length(envelope.salt)? != 32
        || decoded_length(envelope.iv)? != 12
        || !(16..=MAX_CIPHERTEXT).contains(&decoded_length(envelope.ciphertext)?)
    {
        return Err(INVALID.into());
    }
    Ok(())
}

pub fn save_new(path: &Path, contents: &str) -> Result<(), String> {
    validate(contents)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                "That file already exists. Choose a new filename to keep both copies.".to_owned()
            } else {
                "The selected encrypted notes file could not be created.".to_owned()
            }
        })?;
    let result = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if result.is_err() {
        // Only this invocation's newly created, incomplete file is removed.
        let _ = std::fs::remove_file(path);
        return Err(
            "The encrypted notes file could not be fully saved. Please try another destination."
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> serde_json::Value {
        // Envelope validation does not claim these bytes can be decrypted.
        serde_json::json!({"format":"rhythm-encrypted-notes","version":1,
            "kdf":"PBKDF2-SHA256","iterations":600000,"cipher":"AES-256-GCM",
            "salt":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "iv":"AAAAAAAAAAAAAAAA","ciphertext":"AAAAAAAAAAAAAAAAAAAAAA=="})
    }

    #[test]
    fn envelope_boundary_rejects_plaintext_unsupported_and_malformed_files() {
        assert!(validate(&sample().to_string()).is_ok());
        let mut newer = sample();
        newer["version"] = serde_json::json!(2);
        assert!(validate(&newer.to_string()).is_ok());
        for (field, value) in [
            ("notes", serde_json::json!(["private words"])),
            ("version", serde_json::json!(3)),
            ("iterations", serde_json::json!(1)),
            ("cipher", serde_json::json!("AES-CBC")),
            ("iv", serde_json::json!("AA==")),
            (
                "salt",
                serde_json::json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB="),
            ),
            ("ciphertext", serde_json::json!("")),
        ] {
            let mut envelope = sample();
            envelope[field] = value;
            assert!(validate(&envelope.to_string()).is_err(), "{field}");
        }
        assert!(validate("plain journal words").is_err());
        assert!(validate(&" ".repeat(MAX_ENVELOPE + 1)).is_err());
        for invalid in ["=", "====", "A==A", "AB==", "AAA!", "éé"] {
            assert!(decoded_length(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn export_flushes_exact_bytes_and_never_replaces_an_existing_file() {
        let directory =
            std::env::temp_dir().join(format!("rhythm-encrypted-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let target = directory.join("notes.rhythm");
        let contents = sample().to_string();
        save_new(&target, &contents).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), contents);
        assert!(
            save_new(&target, &contents)
                .unwrap_err()
                .contains("already exists")
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), contents);
        let invalid = directory.join("invalid.rhythm");
        assert!(save_new(&invalid, "plaintext").is_err());
        assert!(!invalid.exists());
        std::fs::remove_file(&target).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }
}
