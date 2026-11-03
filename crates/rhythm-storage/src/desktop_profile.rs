//! Portable frontend state is embedded in database snapshots, never browser files.
//! During restore the pending row commits atomically with the journal; a restart
//! retries applying it until the frontend acknowledges this exact restore token.
use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result, Store};

const MAX_PROFILE_BYTES: usize = 20 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopProfile {
    pub schema: u32,
    pub preferences: BTreeMap<String, String>,
    pub backgrounds: PortableBackgrounds,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableBackgrounds {
    pub schema: u32,
    pub focus: PortableBackground,
    pub r#break: PortableBackground,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableBackground {
    pub choice: String,
    pub custom: Option<PortableImage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableImage {
    pub name: String,
    pub mime: String,
    pub base64: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingDesktopProfile {
    pub token: String,
    pub profile: DesktopProfile,
}

impl DesktopProfile {
    /// The frontend additionally validates learned-model details and decodes
    /// image pixels during preview, before the native replacement is permitted.
    pub fn validate(&self) -> Result<()> {
        let invalid = Error::InvalidDesktopProfile;
        if self.schema != 1 || self.backgrounds.schema != 1 {
            return Err(invalid("unsupported version"));
        }
        if self.preferences.len() > 5 {
            return Err(invalid("unsupported preference"));
        }
        for (key, raw) in &self.preferences {
            if raw.len() > 65_536 {
                return Err(invalid("preference exceeds the size limit"));
            }
            let valid = match key.as_str() {
                "rhythm.language" => ["system", "en", "zh-Hans", "zh-Hant"].contains(&raw.as_str()),
                "rhythm.theme" => ["system", "light", "dark"].contains(&raw.as_str()),
                "rhythm.entered" => ["true", "false"].contains(&raw.as_str()),
                "rhythm.adaptive.v1" | "rhythm.reminder-delay.v1" => {
                    let parsed: serde_json::Value = serde_json::from_str(raw)?;
                    parsed.is_object()
                        && parsed.get("schema").and_then(|v| v.as_u64()) == Some(1)
                        && parsed.get("enabled").is_some_and(|v| v.is_boolean())
                }
                _ => false,
            };
            if !valid {
                return Err(invalid("unsupported or malformed preference"));
            }
        }
        for background in [&self.backgrounds.focus, &self.backgrounds.r#break] {
            if !["forest", "coast", "dusk", "custom"].contains(&background.choice.as_str())
                || (background.choice == "custom" && background.custom.is_none())
            {
                return Err(invalid("unsupported background choice"));
            }
            if let Some(image) = &background.custom
                && (image.name.encode_utf16().count() > 180
                    || !["image/jpeg", "image/png", "image/webp", "image/avif"]
                        .contains(&image.mime.as_str())
                    || !canonical_image_base64(&image.base64))
            {
                return Err(invalid("invalid background image"));
            }
        }
        if serde_json::to_vec(self)?.len() > MAX_PROFILE_BYTES {
            return Err(invalid("backup preferences exceed the size limit"));
        }
        Ok(())
    }
}

fn canonical_image_base64(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty()
        || !bytes.len().is_multiple_of(4)
        || bytes.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4
    {
        return false;
    }
    let padding = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2 || bytes.len() / 4 * 3 - padding > MAX_IMAGE_BYTES {
        return false;
    }
    fn digit(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    if bytes[..bytes.len() - padding]
        .iter()
        .any(|&b| digit(b).is_none())
    {
        return false;
    }
    match padding {
        2 => digit(bytes[bytes.len() - 3]).is_some_and(|b| b & 15 == 0),
        1 => digit(bytes[bytes.len() - 2]).is_some_and(|b| b & 3 == 0),
        _ => true,
    }
}

pub(crate) fn write(connection: &Connection, pending: &PendingDesktopProfile) -> Result<()> {
    pending.profile.validate()?;
    let payload = serde_json::to_string(&pending.profile)?;
    let digest = Sha256::digest(payload.as_bytes()).to_vec();
    connection.execute("INSERT INTO local_desktop_profile(singleton,restore_token,payload_json,payload_sha256) VALUES(1,?1,?2,?3) ON CONFLICT(singleton) DO UPDATE SET restore_token=excluded.restore_token,payload_json=excluded.payload_json,payload_sha256=excluded.payload_sha256",
        (&pending.token, payload, digest))?;
    Ok(())
}

pub(crate) fn read(connection: &Connection) -> Result<Option<PendingDesktopProfile>> {
    let row: Option<(String, String, Vec<u8>)> = connection.query_row(
        "SELECT restore_token,payload_json,payload_sha256 FROM local_desktop_profile WHERE singleton=1", [],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
    row.map(|(token, payload, digest)| {
        if uuid::Uuid::parse_str(&token).is_err()
            || payload.len() > MAX_PROFILE_BYTES
            || Sha256::digest(payload.as_bytes()).as_slice() != digest
        {
            return Err(Error::InvalidDesktopProfile(
                "snapshot integrity check failed",
            ));
        }
        let profile: DesktopProfile = serde_json::from_str(&payload)?;
        profile.validate()?;
        Ok(PendingDesktopProfile { token, profile })
    })
    .transpose()
}

impl Store {
    pub fn pending_desktop_profile(&self) -> Result<Option<PendingDesktopProfile>> {
        read(&self.connection)
    }

    /// A stale acknowledgement cannot erase a more recent restoration.
    pub fn acknowledge_desktop_profile_restore(&mut self, token: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "DELETE FROM local_desktop_profile WHERE singleton=1 AND restore_token=?1",
            [token],
        )? == 1)
    }
}

#[cfg(test)]
mod tests;
